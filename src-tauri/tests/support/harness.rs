use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use video_fetch_lib::testing::{
    AppSettings, Db, DownloadJob, DownloadManager, DownloadProgressEvent, JobStatus,
    ProgressEmitter, YtDlpConfig, cleanup_orphan_work_dirs, work_dir_for, work_dir_keep_ids,
};

use super::fixture;
use super::server::TestServer;

#[derive(Default)]
pub struct CountingEmitter {
    pub events: Mutex<Vec<DownloadProgressEvent>>,
}
impl ProgressEmitter for CountingEmitter {
    fn emit_progress(&self, event: DownloadProgressEvent) {
        self.events.lock().unwrap().push(event);
    }
}

pub struct Harness {
    pub server: TestServer,
    pub work_root: PathBuf,
    pub save_dir: PathBuf,
    pub manager: DownloadManager,
    pub ffmpeg: PathBuf,
    yt_dlp: PathBuf,
    settings: AppSettings,
    db_path: PathBuf, // reopened on every restart
    _tmp: tempfile::TempDir,
}

impl Harness {
    pub fn new(concurrency: u32) -> Harness {
        let yt_dlp = find_sidecar("yt-dlp");
        let ffmpeg = find_sidecar("ffmpeg");
        let fx = fixture::ensure(&ffmpeg);
        let server = TestServer::start(&fx.dir);

        let tmp = tempfile::tempdir().expect("tempdir");
        let work_root = tmp.path().join("download-work");
        let save_dir = tmp.path().join("downloads");
        std::fs::create_dir_all(&work_root).unwrap();
        std::fs::create_dir_all(&save_dir).unwrap();
        let db_path = tmp.path().join("jobs.db");
        let settings = AppSettings {
            save_dir: save_dir.to_string_lossy().into_owned(),
            concurrency,
            filename_template: "%(title)s [%(id)s].%(ext)s".into(),
            skip_existing: false,
        };
        let manager = build_manager(&db_path, &settings, &work_root, &yt_dlp, &ffmpeg);
        Harness {
            server,
            work_root,
            save_dir,
            manager,
            ffmpeg,
            yt_dlp,
            settings,
            db_path,
            _tmp: tmp,
        }
    }

    /// Fresh literal job definition for video `n`. Every enqueue attempt needs a
    /// new value: `enqueue` takes the job by value (consumed even on Err).
    pub fn job_for(&self, n: usize) -> DownloadJob {
        DownloadJob {
            id: String::new(),
            url: self.server.url(n, "master.m3u8"),
            video_id: format!("v{n}"),
            page_index: 1,
            format_id: "bestvideo+bestaudio".into(),
            audio_format: None,
            title: format!("clip-{n}"),
            output_template: format!("clip-{n}.%(ext)s"),
            status: JobStatus::Pending,
            progress: 0.0,
            error: None,
            output_path: None,
            thumbnail_url: None,
            duration_secs: None,
            file_size: None,
            created_at: None,
        }
    }

    pub fn enqueue(&self, n: usize) -> DownloadJob {
        self.manager
            .enqueue(self.job_for(n), false)
            .expect("enqueue")
    }

    pub fn job(&self, id: &str) -> DownloadJob {
        self.manager
            .list()
            .unwrap()
            .into_iter()
            .find(|j| j.id == id)
            .unwrap_or_else(|| panic!("job {id} not found"))
    }

    /// The single delivered artifact `clip-<n>.*` — the merge container may be
    /// .mp4 or .mkv; assert uniqueness instead of hardcoding the extension.
    pub fn delivered_path(&self, n: usize) -> PathBuf {
        let prefix = format!("clip-{n}.");
        let mut hits: Vec<PathBuf> = std::fs::read_dir(&self.save_dir)
            .expect("read save dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|f| f.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(
            hits.len(),
            1,
            "expected exactly one delivered file for video {n}: {hits:?}"
        );
        hits.pop().unwrap()
    }

    pub async fn wait_status(&self, id: &str, want: JobStatus, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.job(id).status == want {
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "job {id} did not reach {want:?}; now {:?}",
                    self.job(id).status
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn work_dir_of(&self, id: &str) -> PathBuf {
        work_dir_for(&self.work_root, id)
    }

    /// Marker for process scans: every yt-dlp/ffmpeg this harness spawns has
    /// this unique path in its argv.
    pub fn marker(&self) -> String {
        self.work_root.to_string_lossy().into_owned()
    }

    /// Production-order restart: quit-kill → manager lands → rebuild → orphan
    /// cleanup with the keep-list → reconcile (mirrors `build_app_state`).
    pub fn clean_restart(&mut self) {
        self.manager.kill_all_children();
        self.rebuild();
    }

    /// Hard-crash flavor: no kill at all. The old manager is replaced and
    /// dropped while its runner tasks stay detached and alive (each task holds
    /// its own manager clone, and `kill_on_drop` cannot fire while the tasks
    /// still own the children) — recreating "Running rows + live children",
    /// a state the clean path can never produce.
    pub fn crash_restart(&mut self) {
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let yt_dlp = find_sidecar("yt-dlp");
        let ffmpeg = find_sidecar("ffmpeg");
        let fresh = build_manager(
            &self.db_path,
            &self.settings,
            &self.work_root,
            &yt_dlp,
            &ffmpeg,
        );
        let old = std::mem::replace(&mut self.manager, fresh);
        drop(old); // drop our handle before any new write; leaked task clones may linger (see soak)
        // A settling beat for superseded tasks so the new connection is not
        // fighting live writers during cleanup/reconcile.
        std::thread::sleep(Duration::from_millis(200));
        let keep = work_dir_keep_ids(&self.manager.list().unwrap());
        cleanup_orphan_work_dirs(&self.work_root, &keep);
        self.manager.reconcile_interrupted_runs().unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Must run on assertion panics too, or a leaked yt-dlp keeps CI busy.
        self.manager.kill_all_children();
        super::oracles::kill_processes_containing(&self.marker());
    }
}

fn build_manager(
    db_path: &std::path::Path,
    settings: &AppSettings,
    work_root: &std::path::Path,
    yt_dlp: &std::path::Path,
    ffmpeg: &std::path::Path,
) -> DownloadManager {
    DownloadManager::with_ytdlp(
        Db::open(db_path).expect("open test db"),
        settings.clone(),
        YtDlpConfig {
            yt_dlp_path: yt_dlp.to_path_buf(),
            ffmpeg_path: Some(ffmpeg.to_path_buf()),
        },
        None,
        Arc::new(CountingEmitter::default()),
        work_root.to_path_buf(),
    )
    .expect("build manager")
}

/// Mirrors sidecar.rs's `dev_sidecar_path`: prefix-scan the fetched binaries
/// dir. Fail loud — anyone running this target should have fetched sidecars.
pub fn find_sidecar(base: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
    let prefix = format!("{base}-");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().starts_with(&prefix))
                        .unwrap_or(false)
                        && p.is_file()
                })
                .collect()
        })
        .unwrap_or_else(|e| {
            panic!(
                "read {}: {e}; run python3 scripts/fetch_sidecars.py",
                dir.display()
            )
        });
    found.sort();
    found.into_iter().next().unwrap_or_else(|| {
        panic!(
            "no {base}-* under {}; run python3 scripts/fetch_sidecars.py",
            dir.display()
        )
    })
}
