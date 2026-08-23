use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Serialize;
use tauri::async_runtime::{self, JoinHandle};
use tokio::process::Child;
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::error::{AppError, AppResult};
use crate::fsutil;
use crate::models::{
    AppSettings, CancelAllResult, ClearFinishedResult, DownloadConflict, DownloadJob, JobConflict,
    JobStatus, VideoMeta,
};
use crate::naming;
use crate::ytdlp::{self, ProgressUpdate, YtDlpConfig, kill_download};

pub const PROGRESS_EVENT: &str = "download://progress";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DownloadProgressEvent {
    pub id: String,
    pub progress: f64,
    pub status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub downloaded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

pub trait ProgressEmitter: Send + Sync {
    fn emit_progress(&self, event: DownloadProgressEvent);
}

#[cfg(test)]
pub struct ChannelProgressEmitter {
    tx: tokio::sync::mpsc::UnboundedSender<DownloadProgressEvent>,
}

#[cfg(test)]
impl ChannelProgressEmitter {
    pub fn new() -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<DownloadProgressEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

#[cfg(test)]
impl ProgressEmitter for ChannelProgressEmitter {
    fn emit_progress(&self, event: DownloadProgressEvent) {
        let _ = self.tx.send(event);
    }
}

#[async_trait]
pub trait Downloader: Send + Sync {
    async fn run(
        &self,
        job: &DownloadJob,
        on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
    ) -> Result<PathBuf, String>;
}

pub struct YtDlpDownloader {
    cfg: YtDlpConfig,
    work_root: PathBuf,
    cookies_path: Option<PathBuf>,
    children: Arc<Mutex<HashMap<String, Child>>>,
    cancelled: Arc<Mutex<HashMap<String, bool>>>,
}

impl YtDlpDownloader {
    pub fn new(
        cfg: YtDlpConfig,
        work_root: PathBuf,
        cookies_path: Option<PathBuf>,
        children: Arc<Mutex<HashMap<String, Child>>>,
        cancelled: Arc<Mutex<HashMap<String, bool>>>,
    ) -> Self {
        Self {
            cfg,
            work_root,
            cookies_path,
            children,
            cancelled,
        }
    }
}

#[async_trait]
impl Downloader for YtDlpDownloader {
    async fn run(
        &self,
        job: &DownloadJob,
        on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
    ) -> Result<PathBuf, String> {
        let work = fsutil::work_dir_for(&self.work_root, &job.id);
        if let Err(e) = fs::create_dir_all(&work) {
            return Err(e.to_string());
        }

        let output_template =
            naming::bake_local_datetime_tokens(&job.output_template, &chrono::Local::now());

        match ytdlp::download(
            ytdlp::DownloadRequest {
                cfg: &self.cfg,
                job_id: &job.id,
                url: &job.url,
                format_id: &job.format_id,
                audio_format: job.audio_format.as_deref(),
                output_template: &output_template,
                output_dir: &work,
                cookies_path: self.cookies_path.as_deref(),
                children: &self.children,
                cancelled: &self.cancelled,
            },
            on_progress,
        )
        .await
        {
            Ok(path) => Ok(path),
            Err(e) => {
                let _ = fsutil::remove_job_work_dir(&self.work_root, &job.id);
                Err(e.to_string())
            }
        }
    }
}

#[derive(Clone)]
pub struct DownloadManager {
    db: Arc<Mutex<Db>>,
    settings: Arc<Mutex<AppSettings>>,
    downloader: Arc<dyn Downloader>,
    progress: Arc<dyn ProgressEmitter>,
    children: Arc<Mutex<HashMap<String, Child>>>,
    work_root: PathBuf,
    semaphore: Arc<Semaphore>,
    /// Configured max concurrent downloads (matches semaphore capacity accounting).
    concurrency_limit: Arc<AtomicUsize>,
    running_tasks: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
    cancelled: Arc<Mutex<HashMap<String, bool>>>,
}

impl DownloadManager {
    #[cfg(test)]
    pub fn new(
        db: Db,
        settings: AppSettings,
        downloader: Arc<dyn Downloader>,
        progress: Arc<dyn ProgressEmitter>,
        children: Arc<Mutex<HashMap<String, Child>>>,
        work_root: PathBuf,
    ) -> AppResult<Self> {
        Self::new_with_cancelled(
            db,
            settings,
            downloader,
            progress,
            children,
            work_root,
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    fn new_with_cancelled(
        db: Db,
        settings: AppSettings,
        downloader: Arc<dyn Downloader>,
        progress: Arc<dyn ProgressEmitter>,
        children: Arc<Mutex<HashMap<String, Child>>>,
        work_root: PathBuf,
        cancelled: Arc<Mutex<HashMap<String, bool>>>,
    ) -> AppResult<Self> {
        let concurrency = settings.concurrency.max(1) as usize;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
            settings: Arc::new(Mutex::new(settings)),
            downloader,
            progress,
            children,
            work_root,
            semaphore: Arc::new(Semaphore::new(concurrency)),
            concurrency_limit: Arc::new(AtomicUsize::new(concurrency)),
            running_tasks: Arc::new(Mutex::new(HashMap::new())),
            cancelled,
        })
    }

    pub fn with_ytdlp(
        db: Db,
        settings: AppSettings,
        cfg: YtDlpConfig,
        cookies_path: Option<PathBuf>,
        progress: Arc<dyn ProgressEmitter>,
        work_root: PathBuf,
    ) -> AppResult<Self> {
        let children = Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(Mutex::new(HashMap::new()));
        let downloader = Arc::new(YtDlpDownloader::new(
            cfg,
            work_root.clone(),
            cookies_path,
            Arc::clone(&children),
            Arc::clone(&cancelled),
        )) as Arc<dyn Downloader>;
        Self::new_with_cancelled(
            db, settings, downloader, progress, children, work_root, cancelled,
        )
    }

    /// When `save_as_copy` is true, skip duplicate-done and local-file checks but never bypass
    /// the active-queue lock above.
    pub fn enqueue(&self, mut job: DownloadJob, save_as_copy: bool) -> AppResult<DownloadJob> {
        let settings = self.settings.lock().map_err(lock_err)?.clone();
        let save_dir = PathBuf::from(&settings.save_dir);

        // Never allow a second active job for the same video page (not bypassed by save_as_copy).
        {
            let active = self
                .db
                .lock()
                .map_err(lock_err)?
                .has_active_job(&job.video_id, job.page_index)?;
            if active {
                return Err(AppError::Message(format!(
                    "该视频已在下载队列中（{} P{}），请等待完成或取消后再试",
                    job.video_id, job.page_index
                )));
            }
        }

        if !save_as_copy && settings.skip_existing {
            let recorded = self
                .db
                .lock()
                .map_err(lock_err)?
                .find_done_output_paths(&job.video_id, job.page_index)?;
            let audio_format = job.audio_format.as_deref();
            if recorded_output_exists(&recorded, audio_format)
                || local_output_exists(
                    &save_dir,
                    &job.output_template,
                    &job.title,
                    &job.video_id,
                    "",
                    job.page_index,
                    naming::conflict_exts(audio_format),
                )
            {
                return Err(AppError::Message(format!(
                    "本地已存在该视频文件（{} P{}），已跳过",
                    job.video_id, job.page_index
                )));
            }
        }

        if job.id.is_empty() {
            job.id = new_job_id();
        }
        job.status = JobStatus::Pending;
        job.progress = 0.0;
        job.error = None;
        job.output_path = None;

        self.db.lock().map_err(lock_err)?.insert_job(&job)?;
        self.emit(&job);
        self.spawn_runner(job.id.clone());
        Ok(job)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn check_conflict(
        &self,
        video_id: &str,
        page_indexes: &[u32],
        format_id: &str,
        audio_format: Option<&str>,
        title: &str,
        uploader: &str,
        template: &str,
    ) -> AppResult<DownloadConflict> {
        let settings = self.settings.lock().map_err(lock_err)?.clone();
        let save_dir = PathBuf::from(&settings.save_dir);
        let db = self.db.lock().map_err(lock_err)?;
        let mut downloading = false;
        let mut exists = false;
        let mut file_exists = false;
        for &page_index in page_indexes {
            if db.has_active_job(video_id, page_index)? {
                downloading = true;
            }
            match db.find_job_conflict(video_id, page_index, format_id, audio_format)? {
                JobConflict::Done => exists = true,
                JobConflict::Active | JobConflict::None => {}
            }
            let recorded = db.find_done_output_paths(video_id, page_index)?;
            if recorded_output_exists(&recorded, audio_format)
                || local_output_exists(
                    &save_dir,
                    template,
                    title,
                    video_id,
                    uploader,
                    page_index,
                    naming::conflict_exts(audio_format),
                )
            {
                file_exists = true;
            }
        }
        Ok(DownloadConflict {
            downloading,
            exists,
            file_exists,
        })
    }

    pub fn cancel(&self, id: &str) -> AppResult<DownloadJob> {
        {
            let mut cancelled = self.cancelled.lock().map_err(lock_err)?;
            cancelled.insert(id.to_string(), true);
        }

        kill_download(&self.children, id);

        if let Ok(mut tasks) = self.running_tasks.lock()
            && let Some(handle) = tasks.remove(id)
        {
            handle.abort();
        }

        let db = self.db.lock().map_err(lock_err)?;
        let mut job = db.get_job(id)?;
        let _ = fsutil::remove_job_work_dir(&self.work_root, &job.id);
        if job.status == JobStatus::Done {
            return Ok(job);
        }

        job.status = JobStatus::Failed;
        job.error = Some("用户取消下载".into());
        db.update_job(&job)?;
        drop(db);

        self.emit(&job);
        Ok(job)
    }

    pub fn cancel_all(&self) -> AppResult<CancelAllResult> {
        let ids: Vec<String> = self
            .list()?
            .into_iter()
            .filter(|j| j.status == JobStatus::Pending || j.status == JobStatus::Running)
            .map(|j| j.id)
            .collect();

        let mut cancelled = 0u32;
        let mut errors = Vec::new();
        for id in ids {
            match self.cancel(&id) {
                // Only count when cancel actually marked the job failed.
                // A race to Done returns Ok without changing status.
                Ok(job) if job.status == JobStatus::Failed => cancelled += 1,
                Ok(_) => {}
                Err(e) => errors.push(format!("{id}: {e}")),
            }
        }
        Ok(CancelAllResult { cancelled, errors })
    }

    pub fn clear_finished(&self) -> AppResult<ClearFinishedResult> {
        let cleared = self.db.lock().map_err(lock_err)?.delete_finished_jobs()?;
        Ok(ClearFinishedResult { cleared })
    }

    pub fn retry(&self, id: &str) -> AppResult<DownloadJob> {
        let mut job = self.db.lock().map_err(lock_err)?.get_job(id)?;
        if job.status != JobStatus::Failed {
            return Err(AppError::Message("只能重试失败的任务".into()));
        }

        job.status = JobStatus::Pending;
        job.progress = 0.0;
        job.error = None;
        job.output_path = None;
        self.db.lock().map_err(lock_err)?.update_job(&job)?;

        {
            let mut cancelled = self.cancelled.lock().map_err(lock_err)?;
            cancelled.remove(id);
        }

        self.emit(&job);
        self.spawn_runner(id.to_string());
        Ok(job)
    }

    pub fn list(&self) -> AppResult<Vec<DownloadJob>> {
        self.db.lock().map_err(lock_err)?.list_jobs()
    }

    pub fn get_resolve_cache(&self, key: &str) -> AppResult<Option<(VideoMeta, i64)>> {
        self.db.lock().map_err(lock_err)?.get_resolve_cache(key)
    }

    pub fn upsert_resolve_cache(
        &self,
        key: &str,
        meta: &VideoMeta,
        fetched_at: i64,
    ) -> AppResult<()> {
        self.db
            .lock()
            .map_err(lock_err)?
            .upsert_resolve_cache(key, meta, fetched_at)
    }

    pub fn delete(&self, id: &str, delete_file: bool) -> AppResult<()> {
        let job = self.db.lock().map_err(lock_err)?.get_job(id)?;
        if job.status == JobStatus::Running || job.status == JobStatus::Pending {
            let _ = self.cancel(id);
        }
        if delete_file && let Some(path) = job.output_path.as_deref() {
            let p = PathBuf::from(path);
            if p.is_file() {
                let _ = fs::remove_file(p);
            }
        }
        self.db.lock().map_err(lock_err)?.delete_job(id)?;
        Ok(())
    }

    pub fn update_settings(&self, settings: AppSettings) -> AppResult<()> {
        let new_limit = settings.concurrency.max(1) as usize;
        *self.settings.lock().map_err(lock_err)? = settings;
        self.resize_concurrency(new_limit);
        Ok(())
    }

    /// Grow/shrink the download semaphore to match the configured concurrency.
    fn resize_concurrency(&self, new_limit: usize) {
        let old = self.concurrency_limit.swap(new_limit, Ordering::SeqCst);
        if new_limit > old {
            self.semaphore.add_permits(new_limit - old);
        } else if new_limit < old {
            // Permanently remove idle permits. If permits are currently held by
            // running jobs, try_acquire fails and we leave the extra capacity
            // until the next settings change (or process restart).
            let mut to_remove = old - new_limit;
            while to_remove > 0 {
                match self.semaphore.try_acquire() {
                    Ok(permit) => {
                        permit.forget();
                        to_remove -= 1;
                    }
                    Err(_) => break,
                }
            }
        }
    }

    fn spawn_runner(&self, job_id: String) {
        let this = self.clone();
        let task_id = job_id.clone();
        // Use Tauri's runtime so sync commands can spawn without a current Tokio handle.
        let handle = async_runtime::spawn(async move {
            this.run_job(job_id).await;
        });

        if let Ok(mut tasks) = self.running_tasks.lock() {
            tasks.insert(task_id, handle);
        }
    }

    async fn run_job(&self, job_id: String) {
        let _permit = match self.semaphore.acquire().await {
            Ok(permit) => permit,
            Err(_) => return,
        };

        if self.is_cancelled(&job_id) {
            return;
        }

        let save_dir = match self.save_dir() {
            Ok(dir) => dir,
            Err(e) => {
                let _ = self.fail_job(&job_id, e.to_string());
                return;
            }
        };
        if let Err(e) = check_save_dir_writable(&save_dir) {
            let _ = self.fail_job(&job_id, e.to_string());
            return;
        }

        let job = match self.db.lock().map_err(lock_err) {
            Ok(db) => match db.get_job(&job_id) {
                Ok(job) => job,
                Err(e) => {
                    let _ = self.fail_job(&job_id, e.to_string());
                    return;
                }
            },
            Err(e) => {
                let _ = self.fail_job(&job_id, e.to_string());
                return;
            }
        };

        if job.status != JobStatus::Pending {
            return;
        }

        let mut running = job.clone();
        running.status = JobStatus::Running;
        if let Ok(db) = self.db.lock() {
            let _ = db.update_job(&running);
        }
        tracing::info!(target: "core", "download: 开始 {job_id}");
        self.emit(&running);

        if self.is_cancelled(&job_id) {
            let _ = self.cancel(&job_id);
            return;
        }

        let work = fsutil::work_dir_for(&self.work_root, &job_id);
        if let Some(work_path) = fsutil::find_work_product(&work) {
            match self.complete_relocation(&job_id, &running, &work_path, &save_dir) {
                Ok(()) => {
                    if let Ok(mut tasks) = self.running_tasks.lock() {
                        tasks.remove(&job_id);
                    }
                    return;
                }
                Err(e) => {
                    let _ = self.fail_job(&job_id, e);
                    if let Ok(mut tasks) = self.running_tasks.lock() {
                        tasks.remove(&job_id);
                    }
                    return;
                }
            }
        }

        let db = Arc::clone(&self.db);
        let progress = Arc::clone(&self.progress);
        let cancelled = Arc::clone(&self.cancelled);
        let on_progress = {
            let job_id = job_id.clone();
            Box::new(move |update: ProgressUpdate| {
                if cancelled
                    .lock()
                    .ok()
                    .and_then(|m| m.get(&job_id).copied())
                    .unwrap_or(false)
                {
                    return;
                }
                if let Ok(db) = db.lock()
                    && let Ok(mut current) = db.get_job(&job_id)
                {
                    // Do not resurrect a cancelled/failed/done job from late progress ticks.
                    if current.status != JobStatus::Running && current.status != JobStatus::Pending
                    {
                        return;
                    }
                    current.progress = update.percent / 100.0;
                    current.status = JobStatus::Running;
                    let _ = db.update_job(&current);
                    progress.emit_progress(DownloadProgressEvent {
                        id: current.id.clone(),
                        progress: current.progress,
                        status: current.status.clone(),
                        error: current.error.clone(),
                        output_path: current.output_path.clone(),
                        speed: update.speed,
                        eta: update.eta,
                        downloaded_bytes: update.downloaded_bytes,
                        total_bytes: update.total_bytes,
                    });
                }
            }) as Box<dyn Fn(ProgressUpdate) + Send>
        };

        let result = self.downloader.run(&running, on_progress).await;

        if self.is_cancelled(&job_id) {
            let _ = self.cancel(&job_id);
            return;
        }

        match result {
            Ok(reported_path) => {
                let work_path = resolve_work_product(&work, &reported_path);
                match work_path {
                    Some(path) => {
                        if let Err(e) =
                            self.complete_relocation(&job_id, &running, &path, &save_dir)
                        {
                            let _ = self.fail_job(&job_id, e);
                        }
                    }
                    None => {
                        let _ = self.fail_job(
                            &job_id,
                            format!(
                                "下载完成但未找到输出文件（yt-dlp 回报: {}）",
                                reported_path.display()
                            ),
                        );
                    }
                }
            }
            Err(err) => {
                // Prefer the cancel marker over a kill/pipe race error from yt-dlp.
                if self.is_cancelled(&job_id) {
                    let _ = self.cancel(&job_id);
                } else {
                    let _ = self.fail_job(&job_id, err);
                }
            }
        }

        if let Ok(mut tasks) = self.running_tasks.lock() {
            tasks.remove(&job_id);
        }
    }

    fn fail_job(&self, job_id: &str, error: String) -> AppResult<()> {
        let mut job = self.db.lock().map_err(lock_err)?.get_job(job_id)?;
        job.status = JobStatus::Failed;
        job.error = Some(error);
        tracing::warn!(
            target: "core",
            "download: 失败 {job_id}: {}",
            crate::activity_log::clean_log_message(job.error.as_deref().unwrap_or("未知错误"))
        );
        self.db.lock().map_err(lock_err)?.update_job(&job)?;
        self.emit(&job);
        Ok(())
    }

    fn complete_relocation(
        &self,
        job_id: &str,
        running: &DownloadJob,
        work_path: &Path,
        save_dir: &Path,
    ) -> Result<(), String> {
        let work = fsutil::work_dir_for(&self.work_root, job_id);
        let rel = match work_path.strip_prefix(&work) {
            Ok(p) => p.to_path_buf(),
            Err(_) => work_path.file_name().map(PathBuf::from).unwrap_or_default(),
        };
        if rel.as_os_str().is_empty() {
            return Err("无法确定下载文件的相对路径".into());
        }
        if rel.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir
            )
        }) {
            return Err("下载相对路径非法，已拒绝写出保存目录外".into());
        }
        let dest = save_dir.join(&rel);
        fsutil::relocate_file(work_path, &dest).map_err(|e| format!("搬迁到保存目录失败: {e}"))?;
        let _ = fsutil::remove_job_work_dir(&self.work_root, job_id);
        let mut done = running.clone();
        done.status = JobStatus::Done;
        done.progress = 1.0;
        done.output_path = Some(dest.to_string_lossy().into());
        done.error = None;
        if let Ok(db) = self.db.lock() {
            let _ = db.update_job(&done);
        }
        tracing::info!(target: "core", "download: 完成 {job_id} -> {}", dest.display());
        self.emit(&done);
        Ok(())
    }

    fn is_cancelled(&self, job_id: &str) -> bool {
        self.cancelled
            .lock()
            .ok()
            .and_then(|m| m.get(job_id).copied())
            .unwrap_or(false)
    }

    fn emit(&self, job: &DownloadJob) {
        self.progress.emit_progress(DownloadProgressEvent {
            id: job.id.clone(),
            progress: job.progress,
            status: job.status.clone(),
            error: job.error.clone(),
            output_path: job.output_path.clone(),
            speed: None,
            eta: None,
            downloaded_bytes: None,
            total_bytes: None,
        });
    }

    fn save_dir(&self) -> AppResult<PathBuf> {
        Ok(PathBuf::from(
            &self.settings.lock().map_err(lock_err)?.save_dir,
        ))
    }
}

pub fn check_save_dir_writable(path: &Path) -> AppResult<()> {
    if path.as_os_str().is_empty() {
        return Err(AppError::Message("未设置保存目录".into()));
    }
    fs::create_dir_all(path)?;
    let probe = path.join(format!(
        ".videofetch_write_test_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&probe, b"")?;
    fs::remove_file(probe)?;
    Ok(())
}

fn new_job_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("job-{nanos}-{seq}")
}

fn lock_err<T>(_: std::sync::PoisonError<T>) -> AppError {
    AppError::Message("lock poisoned".into())
}

/// Prefer the path yt-dlp reported when it exists; otherwise scan the work dir.
/// Needed because `--print after_move:filepath` can be garbled on Windows locales.
fn resolve_work_product(work: &Path, reported: &Path) -> Option<PathBuf> {
    if reported.is_file() {
        return Some(reported.to_path_buf());
    }
    fsutil::find_work_product(work)
}

/// True when a recorded path of the same kind (and, for audio, the same
/// container) already exists on disk.
fn recorded_output_exists(recorded_paths: &[String], audio_format: Option<&str>) -> bool {
    recorded_paths.iter().any(|path| {
        let p = Path::new(path);
        let kind_matches = match audio_format {
            Some(fmt) => p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case(fmt))
                .unwrap_or(false),
            None => !naming::path_is_audio_output(p),
        };
        kind_matches && p.is_file()
    })
}

/// True when the predicted output name already exists on disk for this media kind.
fn local_output_exists(
    save_dir: &Path,
    template: &str,
    title: &str,
    video_id: &str,
    uploader: &str,
    page_index: u32,
    exts: &[&str],
) -> bool {
    for ext in exts {
        let relative =
            naming::preview_filename(template, title, video_id, uploader, ext, page_index);
        if save_dir.join(&relative).is_file() {
            return true;
        }
    }
    false
}

pub fn cleanup_orphan_work_dirs(work_root: &Path, keep_job_ids: &[String]) {
    let Ok(entries) = std::fs::read_dir(work_root) else {
        return;
    };
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if ent.path().is_dir() && !keep_job_ids.iter().any(|id| id == &name) {
            let _ = std::fs::remove_dir_all(ent.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::{Duration, sleep};

    struct MockDownloader {
        progress: f64,
        succeed: bool,
        delay_ms: u64,
        cancelled: Arc<Mutex<HashMap<String, bool>>>,
        reported_progress: Arc<Mutex<Option<f64>>>,
        /// When set, emitted instead of a percent-only update.
        rich_progress: Option<ProgressUpdate>,
        /// Isolated scratch dir for mock output files (not the OS shared temp root).
        scratch: tempfile::TempDir,
    }

    impl MockDownloader {
        fn new_scratch() -> tempfile::TempDir {
            tempfile::tempdir().expect("mock scratch dir")
        }

        fn success(progress: f64) -> Self {
            Self {
                progress,
                succeed: true,
                delay_ms: 0,
                cancelled: Arc::new(Mutex::new(HashMap::new())),
                reported_progress: Arc::new(Mutex::new(None)),
                rich_progress: None,
                scratch: Self::new_scratch(),
            }
        }

        fn success_with_update(update: ProgressUpdate) -> Self {
            Self {
                progress: update.percent,
                succeed: true,
                delay_ms: 0,
                cancelled: Arc::new(Mutex::new(HashMap::new())),
                reported_progress: Arc::new(Mutex::new(None)),
                rich_progress: Some(update),
                scratch: Self::new_scratch(),
            }
        }

        fn failure() -> Self {
            Self {
                progress: 0.0,
                succeed: false,
                delay_ms: 0,
                cancelled: Arc::new(Mutex::new(HashMap::new())),
                reported_progress: Arc::new(Mutex::new(None)),
                rich_progress: None,
                scratch: Self::new_scratch(),
            }
        }

        fn slow_success(delay_ms: u64) -> Self {
            Self {
                progress: 0.5,
                succeed: true,
                delay_ms,
                cancelled: Arc::new(Mutex::new(HashMap::new())),
                reported_progress: Arc::new(Mutex::new(None)),
                rich_progress: None,
                scratch: Self::new_scratch(),
            }
        }

        fn mark_cancelled(&self, id: &str) {
            if let Ok(mut map) = self.cancelled.lock() {
                map.insert(id.to_string(), true);
            }
        }
    }

    #[async_trait]
    impl Downloader for MockDownloader {
        async fn run(
            &self,
            job: &DownloadJob,
            on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
        ) -> Result<PathBuf, String> {
            if self.delay_ms > 0 {
                for _ in 0..self.delay_ms / 10 {
                    if self
                        .cancelled
                        .lock()
                        .ok()
                        .and_then(|m| m.get(&job.id).copied())
                        .unwrap_or(false)
                    {
                        return Err("用户取消下载".into());
                    }
                    sleep(Duration::from_millis(10)).await;
                }
            }

            if self.progress > 0.0 || self.rich_progress.is_some() {
                let update = self.rich_progress.clone().unwrap_or(ProgressUpdate {
                    percent: self.progress,
                    ..Default::default()
                });
                let percent = update.percent;
                on_progress(update);
                if let Ok(mut slot) = self.reported_progress.lock() {
                    *slot = Some(percent);
                }
            }

            if !self.succeed {
                return Err("mock download failed".into());
            }

            let path = self.scratch.path().join(format!("{}.mp4", job.id));
            std::fs::write(&path, b"mock").map_err(|e| e.to_string())?;
            Ok(path)
        }
    }

    struct WorkDirMockDownloader {
        work_root: PathBuf,
        /// When set, return this path instead of the real work-dir file (simulates garbled yt-dlp print).
        reported_path: Option<PathBuf>,
    }

    #[async_trait]
    impl Downloader for WorkDirMockDownloader {
        async fn run(
            &self,
            job: &DownloadJob,
            _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
        ) -> Result<PathBuf, String> {
            let work = fsutil::work_dir_for(&self.work_root, &job.id);
            std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
            let path = work.join("demo.mp4");
            std::fs::write(&path, b"video").map_err(|e| e.to_string())?;
            Ok(self.reported_path.clone().unwrap_or(path))
        }
    }

    fn test_settings(dir: &Path) -> AppSettings {
        AppSettings {
            save_dir: dir.to_string_lossy().into(),
            skip_existing: true,
            ..AppSettings::default()
        }
    }

    fn sample_job(id: &str) -> DownloadJob {
        DownloadJob {
            id: id.into(),
            url: "https://www.bilibili.com/video/BV1xx".into(),
            video_id: "BV1xx".into(),
            page_index: 1,
            format_id: "80".into(),
            audio_format: None,
            title: "demo".into(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            status: JobStatus::Pending,
            progress: 0.0,
            error: None,
            output_path: None,
        }
    }

    fn test_manager(
        save_dir: &Path,
        work_root: &Path,
        downloader: Arc<dyn Downloader>,
    ) -> (
        DownloadManager,
        tokio::sync::mpsc::UnboundedReceiver<DownloadProgressEvent>,
    ) {
        let db = Db::open(&save_dir.join("jobs.db")).unwrap();
        let (emitter, rx) = ChannelProgressEmitter::new();
        let manager = DownloadManager::new(
            db,
            test_settings(save_dir),
            downloader,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            work_root.to_path_buf(),
        )
        .unwrap();
        (manager, rx)
    }

    async fn wait_for_status(manager: &DownloadManager, id: &str, status: JobStatus) {
        for _ in 0..100 {
            let jobs = manager.list().unwrap();
            if let Some(job) = jobs.iter().find(|j| j.id == id)
                && job.status == status
            {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for status {status:?}");
    }

    #[test]
    fn cleanup_orphan_work_dirs_removes_orphans_keeps_active() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        std::fs::create_dir_all(&work_root).unwrap();
        std::fs::create_dir_all(work_root.join("orphan-a")).unwrap();
        std::fs::write(work_root.join("orphan-a").join("file.part"), b"x").unwrap();
        std::fs::create_dir_all(work_root.join("orphan-b")).unwrap();
        std::fs::create_dir_all(work_root.join("keep-job-1")).unwrap();
        std::fs::write(work_root.join("keep-job-1").join("file.part"), b"x").unwrap();
        std::fs::create_dir_all(work_root.join("keep-failed-1")).unwrap();
        std::fs::write(work_root.join("keep-failed-1").join("done.mp4"), b"x").unwrap();

        cleanup_orphan_work_dirs(&work_root, &["keep-job-1".into(), "keep-failed-1".into()]);

        assert!(!work_root.join("orphan-a").exists());
        assert!(!work_root.join("orphan-b").exists());
        assert!(work_root.join("keep-job-1").is_dir());
        assert!(work_root.join("keep-failed-1").is_dir());
    }

    #[tokio::test]
    async fn moves_finished_file_from_work_to_save_dir() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();

        let job_id = "job-move";
        let (manager, _rx) = test_manager(
            &save_dir,
            &work_root,
            Arc::new(WorkDirMockDownloader {
                work_root: work_root.clone(),
                reported_path: None,
            }),
        );
        let job = manager.enqueue(sample_job(job_id), false).unwrap();
        wait_for_status(&manager, &job.id, JobStatus::Done).await;

        assert!(save_dir.join("demo.mp4").is_file());
        assert!(!work_root.join(job_id).exists());
    }

    /// yt-dlp `--print after_move:filepath` can return a path that does not exist on disk
    /// (common on Windows when console encoding mangles non-ASCII filenames). Retry already
    /// recovers via `find_work_product`; first completion must do the same.
    #[tokio::test]
    async fn relocates_via_work_dir_when_reported_path_missing() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();

        let job_id = "job-bad-report";
        let (manager, _rx) = test_manager(
            &save_dir,
            &work_root,
            Arc::new(WorkDirMockDownloader {
                work_root: work_root.clone(),
                reported_path: Some(dir.path().join("does-not-exist-garbled.mp4")),
            }),
        );
        let job = manager.enqueue(sample_job(job_id), false).unwrap();
        wait_for_status(&manager, &job.id, JobStatus::Done).await;

        assert!(save_dir.join("demo.mp4").is_file());
        assert!(!work_root.join(job_id).exists());
    }

    #[tokio::test]
    async fn enqueue_reports_progress_and_completes() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("work");
        let (manager, mut rx) = test_manager(
            dir.path(),
            &work_root,
            Arc::new(MockDownloader::success(50.0)),
        );
        let job = manager.enqueue(sample_job("job-success"), false).unwrap();

        let mut saw_running = false;
        let mut saw_half = false;
        for _ in 0..100 {
            while let Ok(event) = rx.try_recv() {
                if event.id == job.id && event.status == JobStatus::Running {
                    saw_running = true;
                    if (event.progress - 0.5).abs() < f64::EPSILON {
                        saw_half = true;
                    }
                }
            }
            let jobs = manager.list().unwrap();
            if jobs
                .iter()
                .any(|j| j.id == job.id && j.status == JobStatus::Done)
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        let done = manager
            .list()
            .unwrap()
            .into_iter()
            .find(|j| j.id == job.id)
            .unwrap();
        assert_eq!(
            done.status,
            JobStatus::Done,
            "job error: {:?} progress={}",
            done.error,
            done.progress
        );
        assert!((done.progress - 1.0).abs() < f64::EPSILON);
        assert!(done.output_path.is_some());
        assert!(saw_running || saw_half);
    }

    #[tokio::test]
    async fn enqueue_emits_structured_progress_fields() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("work");
        let (manager, mut rx) = test_manager(
            dir.path(),
            &work_root,
            Arc::new(MockDownloader::success_with_update(ProgressUpdate {
                percent: 40.0,
                speed: Some(1_048_576.0),
                eta: Some(12),
                downloaded_bytes: Some(4_000_000),
                total_bytes: Some(10_000_000),
            })),
        );
        let job = manager
            .enqueue(sample_job("job-rich-progress"), false)
            .unwrap();

        let mut saw_structured = false;
        for _ in 0..100 {
            while let Ok(event) = rx.try_recv() {
                if event.id == job.id
                    && event.status == JobStatus::Running
                    && (event.progress - 0.4).abs() < f64::EPSILON
                    && event.speed == Some(1_048_576.0)
                    && event.eta == Some(12)
                    && event.downloaded_bytes == Some(4_000_000)
                    && event.total_bytes == Some(10_000_000)
                {
                    saw_structured = true;
                }
            }
            if manager
                .list()
                .unwrap()
                .iter()
                .any(|j| j.id == job.id && j.status == JobStatus::Done)
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        assert!(
            saw_structured,
            "expected DownloadProgressEvent with speed/eta/bytes"
        );
    }

    #[tokio::test]
    async fn enqueue_failure_marks_job_failed() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::failure()),
        );
        let job = manager.enqueue(sample_job("job-fail"), false).unwrap();
        wait_for_status(&manager, &job.id, JobStatus::Failed).await;

        let failed = manager
            .list()
            .unwrap()
            .into_iter()
            .find(|j| j.id == job.id)
            .unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap()
                .contains("mock download failed")
        );
    }

    #[tokio::test]
    async fn cancel_marks_job_with_cancel_message() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Arc::new(MockDownloader::slow_success(500));
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::clone(&mock) as Arc<dyn Downloader>,
        );
        let job = manager.enqueue(sample_job("job-cancel"), false).unwrap();

        sleep(Duration::from_millis(50)).await;
        mock.mark_cancelled(&job.id);
        let cancelled = manager.cancel(&job.id).unwrap();
        assert_eq!(cancelled.status, JobStatus::Failed);
        assert_eq!(cancelled.error.as_deref(), Some("用户取消下载"));
    }

    #[tokio::test]
    async fn cancel_all_marks_active_jobs_failed_and_skips_done() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("jobs.db");
        {
            let db = Db::open(&db_path).unwrap();
            let mut done = sample_job("done-keep");
            done.video_id = "BV-done".into();
            done.status = JobStatus::Done;
            done.progress = 1.0;
            done.output_path = Some(dir.path().join("kept.mp4").to_string_lossy().into());
            std::fs::write(dir.path().join("kept.mp4"), b"keep").unwrap();
            db.insert_job(&done).unwrap();
        }

        let mock = Arc::new(MockDownloader::slow_success(800));
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::clone(&mock) as Arc<dyn Downloader>,
        );
        let a = manager.enqueue(sample_job("job-a"), false).unwrap();
        let mut job_b = sample_job("job-b");
        job_b.video_id = "BV2xx".into();
        let b = manager.enqueue(job_b, false).unwrap();
        sleep(Duration::from_millis(40)).await;
        mock.mark_cancelled(&a.id);
        mock.mark_cancelled(&b.id);

        let result = manager.cancel_all().unwrap();
        assert_eq!(result.cancelled, 2);
        assert!(result.errors.is_empty());

        let jobs = manager.list().unwrap();
        let done = jobs.iter().find(|j| j.id == "done-keep").unwrap();
        assert_eq!(done.status, JobStatus::Done);
        assert_eq!(
            done.output_path.as_deref(),
            Some(dir.path().join("kept.mp4").to_string_lossy().as_ref())
        );
        assert!(dir.path().join("kept.mp4").is_file());

        for id in [&a.id, &b.id] {
            let job = jobs.iter().find(|j| j.id == *id).unwrap();
            assert_eq!(job.status, JobStatus::Failed);
            assert_eq!(job.error.as_deref(), Some("用户取消下载"));
        }
    }

    #[tokio::test]
    async fn cancel_all_with_no_active_returns_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(50)) as Arc<dyn Downloader>,
        );
        let result = manager.cancel_all().unwrap();
        assert_eq!(result.cancelled, 0);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn clear_finished_deletes_records_keeps_files_and_active_rows() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mp4");
        std::fs::write(&file_path, b"data").unwrap();

        {
            let db = Db::open(&dir.path().join("jobs.db")).unwrap();
            let mut pending = sample_job("pending-1");
            pending.status = JobStatus::Pending;
            db.insert_job(&pending).unwrap();

            let mut done = sample_job("done-1");
            done.status = JobStatus::Done;
            done.progress = 1.0;
            done.output_path = Some(file_path.to_string_lossy().into());
            db.insert_job(&done).unwrap();

            let mut failed = sample_job("failed-1");
            failed.status = JobStatus::Failed;
            failed.error = Some("用户取消下载".into());
            db.insert_job(&failed).unwrap();
        }

        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(50)) as Arc<dyn Downloader>,
        );
        let result = manager.clear_finished().unwrap();
        assert_eq!(result.cleared, 2);
        assert!(file_path.is_file(), "local file must remain");

        let jobs = manager.list().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "pending-1");
        assert_eq!(jobs[0].status, JobStatus::Pending);
    }

    type ProgressCallback = Box<dyn Fn(ProgressUpdate) + Send>;
    type ProgressSink = Arc<Mutex<Option<ProgressCallback>>>;

    /// Late yt-dlp progress must not flip a cancelled job back to Running.
    ///
    /// The progress callback is stashed outside the downloader future so we can
    /// invoke it after `cancel()` aborts the JoinHandle (abort alone must not
    /// make this test pass).
    #[tokio::test]
    async fn cancel_ignores_progress_after_cancel() {
        use tokio::sync::Notify;

        let dir = tempfile::tempdir().unwrap();
        let after_first = Arc::new(Notify::new());
        let progress_sink: ProgressSink = Arc::new(Mutex::new(None));

        struct LateProgressMock {
            after_first: Arc<Notify>,
            progress_sink: ProgressSink,
        }

        #[async_trait]
        impl Downloader for LateProgressMock {
            async fn run(
                &self,
                _job: &DownloadJob,
                on_progress: ProgressCallback,
            ) -> Result<PathBuf, String> {
                on_progress(ProgressUpdate {
                    percent: 10.0,
                    ..Default::default()
                });
                *self.progress_sink.lock().map_err(|e| e.to_string())? = Some(on_progress);
                self.after_first.notify_one();
                std::future::pending::<()>().await;
                Err("unreachable".into())
            }
        }

        let (manager, mut rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(LateProgressMock {
                after_first: Arc::clone(&after_first),
                progress_sink: Arc::clone(&progress_sink),
            }),
        );
        let job = manager
            .enqueue(sample_job("job-cancel-progress"), false)
            .unwrap();

        after_first.notified().await;
        let cancelled = manager.cancel(&job.id).unwrap();
        assert_eq!(cancelled.status, JobStatus::Failed);

        let late = progress_sink
            .lock()
            .unwrap()
            .take()
            .expect("progress callback should be stashed before cancel");
        late(ProgressUpdate {
            percent: 90.0,
            ..Default::default()
        });
        sleep(Duration::from_millis(50)).await;

        let final_job = manager
            .list()
            .unwrap()
            .into_iter()
            .find(|j| j.id == job.id)
            .unwrap();
        assert_eq!(final_job.status, JobStatus::Failed);
        assert_eq!(final_job.error.as_deref(), Some("用户取消下载"));
        assert!(final_job.progress < 0.5);

        let mut saw_running_after_cancel = false;
        while let Ok(event) = rx.try_recv() {
            if event.id == job.id && event.status == JobStatus::Running && event.progress >= 0.9 {
                saw_running_after_cancel = true;
            }
        }
        assert!(
            !saw_running_after_cancel,
            "late progress must not emit Running after cancel"
        );
    }

    #[tokio::test]
    async fn dedupe_skips_when_local_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("demo [BV1xx].mp4");
        std::fs::write(&file_path, b"x").unwrap();

        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        let mut done = sample_job("done-1");
        done.status = JobStatus::Done;
        done.progress = 1.0;
        done.output_path = Some(file_path.to_string_lossy().into());
        db.insert_job(&done).unwrap();

        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::success(100.0)),
        );
        let err = manager
            .enqueue(sample_job("job-dedupe"), false)
            .unwrap_err();
        assert!(err.to_string().contains("已跳过"));
    }

    #[tokio::test]
    async fn allows_redownload_when_done_record_but_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        let mut done = sample_job("done-1");
        done.status = JobStatus::Done;
        done.progress = 1.0;
        done.output_path = Some(dir.path().join("missing.mp4").to_string_lossy().into());
        db.insert_job(&done).unwrap();

        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::success(50.0)),
        );
        let job = manager
            .enqueue(sample_job("job-redownload"), false)
            .unwrap();
        assert_eq!(job.status, JobStatus::Pending);
    }

    #[tokio::test]
    async fn save_as_copy_uses_numbered_template_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("demo [BV1xx].mp4"), b"x").unwrap();

        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::success(50.0)),
        );
        let mut job = sample_job("job-copy");
        job.output_template = naming::next_available_output_template(
            dir.path(),
            "%(title)s [%(id)s].%(ext)s",
            "demo",
            "BV1xx",
            "",
            1,
            None,
        );
        assert!(job.output_template.contains(" (1)"));
        let enqueued = manager.enqueue(job, true).unwrap();
        assert!(enqueued.output_template.contains(" (1)"));
    }

    #[tokio::test]
    async fn force_enqueue_allows_done_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        let mut done = sample_job("done-1");
        done.status = JobStatus::Done;
        done.progress = 1.0;
        done.output_path = Some("virtual/demo.mp4".into());
        db.insert_job(&done).unwrap();

        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::success(50.0)),
        );
        let job = manager.enqueue(sample_job("job-force"), true).unwrap();
        assert_eq!(job.status, JobStatus::Pending);
    }

    #[tokio::test]
    async fn rejects_active_duplicate_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(5_000)),
        );
        let first = manager.enqueue(sample_job("job-active-1"), false).unwrap();
        assert_eq!(first.status, JobStatus::Pending);

        // Wait until the first job is actively running so conflict detection is stable.
        wait_for_status(&manager, &first.id, JobStatus::Running).await;

        let err = manager
            .enqueue(sample_job("job-active-2"), false)
            .unwrap_err();
        assert!(err.to_string().contains("已在下载队列"));
    }

    #[tokio::test]
    async fn rejects_active_duplicate_even_with_force() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(5_000)),
        );
        let first = manager.enqueue(sample_job("job-active-1"), false).unwrap();
        wait_for_status(&manager, &first.id, JobStatus::Running).await;

        let err = manager
            .enqueue(sample_job("job-active-force"), true)
            .unwrap_err();
        assert!(err.to_string().contains("已在下载队列"));
    }

    #[tokio::test]
    async fn rejects_active_duplicate_across_formats() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(5_000)),
        );
        let mut first = sample_job("job-fmt-1");
        first.format_id = "80".into();
        let first = manager.enqueue(first, false).unwrap();
        wait_for_status(&manager, &first.id, JobStatus::Running).await;

        let mut second = sample_job("job-fmt-2");
        second.format_id = "64".into();
        let err = manager.enqueue(second, false).unwrap_err();
        assert!(err.to_string().contains("已在下载队列"));
    }

    #[tokio::test]
    async fn check_conflict_reports_downloading_and_exists() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        let mut done = sample_job("done-1");
        done.status = JobStatus::Done;
        done.page_index = 1;
        db.insert_job(&done).unwrap();

        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::success(100.0)),
        );
        let mut pending = sample_job("pending-1");
        pending.page_index = 2;
        manager.enqueue(pending, false).unwrap();

        let mut conflict = manager
            .check_conflict(
                "BV1xx",
                &[1, 2],
                "80",
                None,
                "demo",
                "",
                "%(title)s [%(id)s].%(ext)s",
            )
            .unwrap();
        assert!(conflict.exists);
        assert!(conflict.downloading);
        assert!(conflict.downloading || conflict.exists);

        // Local file without a matching done job should still count as conflict.
        let file_path = dir.path().join("only-file [BV2yy].mp4");
        std::fs::write(&file_path, b"x").unwrap();
        conflict = manager
            .check_conflict(
                "BV2yy",
                &[1],
                "80",
                None,
                "only-file",
                "",
                "%(title)s [%(id)s].%(ext)s",
            )
            .unwrap();
        assert!(conflict.file_exists);
        assert!(!conflict.exists);
        assert!(!conflict.downloading);

        let audio_conflict = manager
            .check_conflict(
                "BV2yy",
                &[1],
                "bestaudio",
                Some("m4a"),
                "only-file",
                "",
                "%(title)s [%(id)s].%(ext)s",
            )
            .unwrap();
        assert!(!audio_conflict.file_exists);

        // An existing .m4a must not block a new .mp3 of the same source.
        let m4a_path = dir.path().join("only-file [BV2yy].m4a");
        std::fs::write(&m4a_path, b"x").unwrap();
        let mp3_conflict = manager
            .check_conflict(
                "BV2yy",
                &[1],
                "bestaudio",
                Some("mp3"),
                "only-file",
                "",
                "%(title)s [%(id)s].%(ext)s",
            )
            .unwrap();
        assert!(!mp3_conflict.file_exists);
        let m4a_conflict = manager
            .check_conflict(
                "BV2yy",
                &[1],
                "bestaudio",
                Some("m4a"),
                "only-file",
                "",
                "%(title)s [%(id)s].%(ext)s",
            )
            .unwrap();
        assert!(m4a_conflict.file_exists);
    }

    #[tokio::test]
    async fn retry_relocates_existing_work_product_without_download() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();

        let job_id = "job-reloc-retry";
        let work = fsutil::work_dir_for(&work_root, job_id);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("demo.mp4"), b"video").unwrap();

        let called = Arc::new(AtomicBool::new(false));
        struct NoCallDownloader {
            called: Arc<AtomicBool>,
        }
        #[async_trait]
        impl Downloader for NoCallDownloader {
            async fn run(
                &self,
                _job: &DownloadJob,
                _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
            ) -> Result<PathBuf, String> {
                self.called.store(true, Ordering::SeqCst);
                Err("downloader should not run".into())
            }
        }

        let db = Db::open(&save_dir.join("jobs.db")).unwrap();
        let mut failed = sample_job(job_id);
        failed.status = JobStatus::Failed;
        failed.error = Some("搬迁到保存目录失败: mock".into());
        db.insert_job(&failed).unwrap();

        let (emitter, _rx) = ChannelProgressEmitter::new();
        let manager = DownloadManager::new(
            db,
            test_settings(&save_dir),
            Arc::new(NoCallDownloader {
                called: Arc::clone(&called),
            }) as Arc<dyn Downloader>,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            work_root.clone(),
        )
        .unwrap();

        manager.retry(job_id).unwrap();
        wait_for_status(&manager, job_id, JobStatus::Done).await;

        assert!(!called.load(Ordering::SeqCst));
        assert!(save_dir.join("demo.mp4").is_file());
        assert!(!work_root.join(job_id).exists());
    }

    #[tokio::test]
    async fn retry_reruns_failed_job() {
        let dir = tempfile::tempdir().unwrap();
        let attempt = Arc::new(AtomicBool::new(false));
        let downloader = {
            let attempt = Arc::clone(&attempt);
            struct RetryMock {
                attempt: Arc<AtomicBool>,
                scratch: tempfile::TempDir,
            }
            #[async_trait]
            impl Downloader for RetryMock {
                async fn run(
                    &self,
                    _job: &DownloadJob,
                    _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
                ) -> Result<PathBuf, String> {
                    if self
                        .attempt
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        Err("first attempt failed".into())
                    } else {
                        let path = self.scratch.path().join("retry.mp4");
                        std::fs::write(&path, b"x").map_err(|e| e.to_string())?;
                        Ok(path)
                    }
                }
            }
            Arc::new(RetryMock {
                attempt,
                scratch: tempfile::tempdir().expect("retry scratch"),
            }) as Arc<dyn Downloader>
        };

        let (manager, _rx) = test_manager(dir.path(), &dir.path().join("work"), downloader);
        let job = manager.enqueue(sample_job("job-retry"), false).unwrap();
        wait_for_status(&manager, &job.id, JobStatus::Failed).await;

        manager.retry(&job.id).unwrap();
        wait_for_status(&manager, &job.id, JobStatus::Done).await;
        let done = manager
            .list()
            .unwrap()
            .into_iter()
            .find(|j| j.id == job.id)
            .unwrap();
        assert_eq!(done.status, JobStatus::Done);
    }

    #[tokio::test]
    async fn raising_concurrency_allows_parallel_downloads() {
        let dir = tempfile::tempdir().unwrap();
        // Hold downloads long enough that CI scheduling still leaves a window
        // where both jobs are Running after concurrency is raised.
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(2_000)),
        );

        let mut a = sample_job("job-conc-a");
        a.page_index = 1;
        let mut b = sample_job("job-conc-b");
        b.page_index = 2;
        manager.enqueue(a, false).unwrap();
        manager.enqueue(b, false).unwrap();

        // Default concurrency is 1 — only one should be running.
        let mut saw_one_running = false;
        for _ in 0..100 {
            let jobs = manager.list().unwrap();
            let running = jobs
                .iter()
                .filter(|j| j.status == JobStatus::Running)
                .count();
            let pending = jobs
                .iter()
                .filter(|j| j.status == JobStatus::Pending)
                .count();
            if running == 1 && pending == 1 {
                saw_one_running = true;
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        assert!(
            saw_one_running,
            "expected one running and one pending under concurrency 1"
        );

        let mut settings = test_settings(dir.path());
        settings.concurrency = 2;
        manager.update_settings(settings).unwrap();

        // After raising the limit, the waiting job should start.
        for _ in 0..150 {
            let jobs = manager.list().unwrap();
            let running = jobs
                .iter()
                .filter(|j| j.status == JobStatus::Running)
                .count();
            if running == 2 {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("expected both jobs running after concurrency increase");
    }

    #[test]
    fn save_dir_must_be_writable() {
        let dir = tempfile::tempdir().unwrap();
        check_save_dir_writable(dir.path()).unwrap();
    }
}
