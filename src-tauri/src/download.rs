use std::collections::{HashMap, HashSet};
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
    JobStatus, PauseAllResult, ResumeAllResult, VideoMeta,
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

/// Why a job was asked to stop. One map holds at most one kind per job; a
/// cancel must never be silently read as a pause or vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopKind {
    Cancel,
    Pause,
}

pub type StopFlags = Arc<Mutex<HashMap<String, StopKind>>>;

pub(crate) fn stop_kind_of(flags: &StopFlags, job_id: &str) -> Option<StopKind> {
    flags.lock().ok().and_then(|m| m.get(job_id).copied())
}

/// What the runner does at a checkpoint when it observes a stop flag: a cancel
/// re-asserts the Failed row; a pause returns silently — the `pause()` command
/// already wrote Paused and must not be overwritten, nor its work dir removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopDisposition {
    MarkCancelled,
    ReturnSilently,
}

pub(crate) fn stop_disposition(kind: Option<StopKind>) -> Option<StopDisposition> {
    match kind {
        None => None,
        Some(StopKind::Cancel) => Some(StopDisposition::MarkCancelled),
        Some(StopKind::Pause) => Some(StopDisposition::ReturnSilently),
    }
}

/// The downloader's Err cleanup removes the work dir — but a pause error is the
/// expected way a paused yt-dlp exits, and its `.part` files are the whole
/// point of resuming. Only cancel and real errors may remove the dir.
pub(crate) fn err_cleanup_removes_work_dir(kind: Option<StopKind>) -> bool {
    kind != Some(StopKind::Pause)
}

/// The one home for user-facing stop messages: cancel()'s job error, the
/// downloader's Err text and logs all read from here.
pub(crate) fn stop_message(kind: StopKind) -> &'static str {
    match kind {
        StopKind::Cancel => "用户取消下载",
        StopKind::Pause => "用户暂停下载",
    }
}

/// Work-dir ids the startup orphan cleanup must keep: every non-terminal row —
/// paused included (its `.part` files are exactly what resume needs), failed
/// too (a retry may relocate a finished leftover).
pub(crate) fn work_dir_keep_ids(jobs: &[DownloadJob]) -> Vec<String> {
    jobs.iter()
        .filter(|j| {
            matches!(
                j.status,
                JobStatus::Pending | JobStatus::Running | JobStatus::Failed | JobStatus::Paused
            )
        })
        .map(|j| j.id.clone())
        .collect()
}

/// A job row deleted concurrently (e.g. by `delete`) must surface as a Chinese
/// message, not a raw rusqlite "Query returned no rows".
fn missing_job_err() -> AppError {
    AppError::Message(crate::db::MISSING_JOB_MESSAGE.into())
}

pub struct YtDlpDownloader {
    cfg: YtDlpConfig,
    work_root: PathBuf,
    cookies_path: Option<PathBuf>,
    children: Arc<Mutex<HashMap<String, Child>>>,
    stop_flags: StopFlags,
}

impl YtDlpDownloader {
    pub fn new(
        cfg: YtDlpConfig,
        work_root: PathBuf,
        cookies_path: Option<PathBuf>,
        children: Arc<Mutex<HashMap<String, Child>>>,
        stop_flags: StopFlags,
    ) -> Self {
        Self {
            cfg,
            work_root,
            cookies_path,
            children,
            stop_flags,
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
                stop_flags: &self.stop_flags,
            },
            on_progress,
        )
        .await
        {
            Ok(path) => Ok(path),
            Err(e) => {
                if err_cleanup_removes_work_dir(stop_kind_of(&self.stop_flags, &job.id)) {
                    let _ = fsutil::remove_job_work_dir(&self.work_root, &job.id);
                } else {
                    tracing::debug!(target: "core", "download: 暂停保留工作目录 {}", job.id);
                }
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
    stop_flags: StopFlags,
}

/// Structured outcome of one enqueue attempt. The single-video command maps
/// kinds back to the exact same user-facing strings as before; the batch
/// command counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueKind {
    Enqueued,
    DuplicateActive,
    AlreadyExists,
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
        Self::new_with_stop_flags(
            db,
            settings,
            downloader,
            progress,
            children,
            work_root,
            Arc::new(Mutex::new(HashMap::new())),
        )
    }

    fn new_with_stop_flags(
        db: Db,
        settings: AppSettings,
        downloader: Arc<dyn Downloader>,
        progress: Arc<dyn ProgressEmitter>,
        children: Arc<Mutex<HashMap<String, Child>>>,
        work_root: PathBuf,
        stop_flags: StopFlags,
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
            stop_flags,
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
        let stop_flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let downloader = Arc::new(YtDlpDownloader::new(
            cfg,
            work_root.clone(),
            cookies_path,
            Arc::clone(&children),
            Arc::clone(&stop_flags),
        )) as Arc<dyn Downloader>;
        Self::new_with_stop_flags(
            db, settings, downloader, progress, children, work_root, stop_flags,
        )
    }

    /// Runs the full enqueue pipeline — active-queue check, skip-existing checks, DB
    /// insert, emit, runner spawn — and reports the outcome structurally so the batch
    /// command can classify results without parsing user-facing messages.
    ///
    /// When `save_as_copy` is true, skip duplicate-done and local-file checks but never bypass
    /// the active-queue lock above.
    pub fn enqueue_classified(
        &self,
        job: &mut DownloadJob,
        save_as_copy: bool,
    ) -> AppResult<EnqueueKind> {
        let settings = self.settings.lock().map_err(lock_err)?.clone();
        let save_dir = PathBuf::from(&settings.save_dir);

        // Never allow a second active job for the same video page (not bypassed by save_as_copy).
        {
            let active = self.db.lock().map_err(lock_err)?.has_active_job(
                &job.video_id,
                job.page_index,
                job.audio_format.as_deref(),
            )?;
            if active {
                return Ok(EnqueueKind::DuplicateActive);
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
                return Ok(EnqueueKind::AlreadyExists);
            }
        }

        if job.id.is_empty() {
            job.id = new_job_id();
        }
        job.status = JobStatus::Pending;
        job.progress = 0.0;
        job.error = None;
        job.output_path = None;

        let prepared = job.clone();
        self.db.lock().map_err(lock_err)?.insert_job(job)?;
        self.emit(&prepared);
        self.spawn_runner(prepared.id.clone());
        Ok(EnqueueKind::Enqueued)
    }

    /// When `save_as_copy` is true, skip duplicate-done and local-file checks but never bypass
    /// the active-queue lock above.
    pub fn enqueue(&self, mut job: DownloadJob, save_as_copy: bool) -> AppResult<DownloadJob> {
        let kind = self.enqueue_classified(&mut job, save_as_copy)?;
        match kind {
            EnqueueKind::Enqueued => Ok(job),
            EnqueueKind::DuplicateActive => {
                let media = if job.audio_format.is_some() {
                    "音频"
                } else {
                    "视频"
                };
                Err(AppError::Message(format!(
                    "该{media}已在下载队列中（{} P{}），请继续、等待或取消后再试",
                    job.video_id, job.page_index
                )))
            }
            EnqueueKind::AlreadyExists => {
                let media = if job.audio_format.is_some() {
                    "音频"
                } else {
                    "视频"
                };
                Err(AppError::Message(format!(
                    "本地已存在该{media}文件（{} P{}），已跳过",
                    job.video_id, job.page_index
                )))
            }
        }
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
            if db.has_active_job(video_id, page_index, audio_format)? {
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
            let mut flags = self.stop_flags.lock().map_err(lock_err)?;
            flags.insert(id.to_string(), StopKind::Cancel);
        }

        kill_download(&self.children, id);

        if let Ok(mut tasks) = self.running_tasks.lock()
            && let Some(handle) = tasks.remove(id)
        {
            handle.abort();
        }

        let db = self.db.lock().map_err(lock_err)?;
        let mut job = match db.get_job(id) {
            Ok(job) => job,
            Err(e) => {
                // The marker is set before the id is validated; a failed cancel
                // must not leak a Cancel flag for the process lifetime.
                drop(db);
                if let Ok(mut flags) = self.stop_flags.lock() {
                    flags.remove(id);
                }
                return Err(e);
            }
        };
        let _ = fsutil::remove_job_work_dir(&self.work_root, &job.id);
        if job.status == JobStatus::Done {
            // A finished row never runs the Err dispatch the marker exists for;
            // retire it so repeated no-op cancels cannot grow the map.
            drop(db);
            if let Ok(mut flags) = self.stop_flags.lock() {
                flags.remove(id);
            }
            return Ok(job);
        }

        job.status = JobStatus::Failed;
        job.error = Some(stop_message(StopKind::Cancel).into());
        db.update_job(&job)?;
        drop(db);

        self.emit(&job);
        Ok(job)
    }

    /// One bulk skeleton: collect ids by predicate, run the per-id op, count
    /// only actual status changes, collect "id: error" strings. cancel_all /
    /// pause_all / resume_all build their result structs from this.
    fn run_bulk<F, C>(
        &self,
        predicate: impl Fn(&DownloadJob) -> bool,
        op: F,
        counted: C,
    ) -> AppResult<(u32, Vec<String>)>
    where
        F: Fn(&Self, &str) -> AppResult<DownloadJob>,
        C: Fn(&JobStatus) -> bool,
    {
        let ids: Vec<String> = self
            .list()?
            .into_iter()
            .filter(|j| predicate(j))
            .map(|j| j.id)
            .collect();

        let mut count = 0u32;
        let mut errors = Vec::new();
        for id in ids {
            match op(self, &id) {
                // Counted only when the op actually changed the status (a race
                // to Done returns Ok without a transition).
                Ok(job) => {
                    if counted(&job.status) {
                        count += 1;
                    }
                }
                Err(e) => errors.push(format!("{id}: {e}")),
            }
        }
        Ok((count, errors))
    }

    pub fn cancel_all(&self) -> AppResult<CancelAllResult> {
        let (cancelled, errors) = self.run_bulk(
            |j| {
                matches!(
                    j.status,
                    JobStatus::Pending | JobStatus::Running | JobStatus::Paused
                )
            },
            |m, id| m.cancel(id),
            |s| *s == JobStatus::Failed,
        )?;
        Ok(CancelAllResult { cancelled, errors })
    }

    pub fn clear_finished(&self) -> AppResult<ClearFinishedResult> {
        let cleared_ids = self.db.lock().map_err(lock_err)?.delete_finished_jobs()?;
        // The rows are gone: drop their stop markers too, or every
        // cancelled-then-cleared job leaks its flag for the process lifetime.
        // Rows that survive the clear (paused included) keep their markers.
        if let Ok(mut flags) = self.stop_flags.lock() {
            let cleared: HashSet<&str> = cleared_ids.iter().map(String::as_str).collect();
            flags.retain(|id, _| !cleared.contains(id.as_str()));
        }
        Ok(ClearFinishedResult {
            cleared: cleared_ids.len() as u64,
        })
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
            let mut flags = self.stop_flags.lock().map_err(lock_err)?;
            flags.remove(id);
        }

        self.emit(&job);
        self.spawn_runner(id.to_string());
        Ok(job)
    }

    /// Pause a Pending or Running job: stop the whole transfer (process tree),
    /// keep the work dir so `resume` continues from the partial files.
    pub fn pause(&self, id: &str) -> AppResult<DownloadJob> {
        // Preliminary guard for a friendly error; the locked re-read below is
        // the authority (races resolve there).
        {
            let job = self
                .db
                .lock()
                .map_err(lock_err)?
                .find_job(id)?
                .ok_or_else(missing_job_err)?;
            match job.status {
                JobStatus::Pending | JobStatus::Running => {}
                // Already finished/failed: return the job unchanged (never set
                // the marker) so the UI shows its 「任务已结束，未执行暂停」
                // info toast and pause_all skips it instead of counting a
                // bogus failure — mirroring cancel()'s silent no-op.
                JobStatus::Done | JobStatus::Failed => return Ok(job),
                JobStatus::Paused => {
                    return Err(AppError::Message("只能暂停等待中或下载中的任务".into()));
                }
            }
        }

        {
            let mut flags = self.stop_flags.lock().map_err(lock_err)?;
            flags.insert(id.to_string(), StopKind::Pause);
        }

        kill_download(&self.children, id);
        if let Ok(mut tasks) = self.running_tasks.lock()
            && let Some(handle) = tasks.remove(id)
        {
            handle.abort();
        }

        // Re-read + write under one db lock (mirrors cancel()): the runner may
        // have reached a terminal state between the kill and this read — that
        // state wins, a finished or failed job must not become Paused.
        let job = {
            let db = self.db.lock().map_err(lock_err)?;
            let Some(mut job) = db.find_job(id)? else {
                // Row vanished (concurrent delete): drop the marker we just set
                // so it cannot outlive its job for the rest of the process.
                if let Ok(mut flags) = self.stop_flags.lock() {
                    flags.remove(id);
                }
                return Err(missing_job_err());
            };
            if matches!(job.status, JobStatus::Pending | JobStatus::Running) {
                job.status = JobStatus::Paused;
                job.error = None;
                db.update_job(&job)?;
            }
            job
        };
        // The pause marker only stays while the job is Paused (a racing Cancel
        // marker is never clobbered).
        if job.status != JobStatus::Paused
            && let Ok(mut flags) = self.stop_flags.lock()
            && flags.get(id).copied() == Some(StopKind::Pause)
        {
            flags.remove(id);
        }
        self.emit(&job);
        Ok(job)
    }

    /// Resume a paused job: keep progress and work dir; yt-dlp continues from
    /// the partial fragments (a local re-merge when they are already complete).
    pub fn resume(&self, id: &str) -> AppResult<DownloadJob> {
        // Read-modify-write under one db lock (mirrors pause()): a concurrent
        // delete between read and write would otherwise surface update_job's
        // missing-row error instead of the friendly one below.
        let job = {
            let db = self.db.lock().map_err(lock_err)?;
            let mut job = db.find_job(id)?.ok_or_else(missing_job_err)?;
            if job.status != JobStatus::Paused {
                return Err(AppError::Message("只能继续已暂停的任务".into()));
            }
            job.status = JobStatus::Pending;
            job.error = None;
            db.update_job(&job)?;
            job
        };
        // Only our own Pause marker is cleared: a racing Cancel marker must
        // survive so the freshly spawned runner re-asserts Failed instead of
        // leaving a ghost Pending row (pause() enforces the same rule; retry()
        // keeps its unconditional clear by design — it exists to resurrect).
        if let Ok(mut flags) = self.stop_flags.lock()
            && flags.get(id).copied() == Some(StopKind::Pause)
        {
            flags.remove(id);
        }
        // A resume whose work dir went missing (external cleanup) silently
        // re-downloads from scratch; leave a trace so "why did it restart" is
        // answerable from the log.
        if job.progress > 0.0 && !fsutil::work_dir_for(&self.work_root, id).exists() {
            tracing::info!(target: "core", "download: 工作目录缺失，重新下载 {id}");
        }
        self.emit(&job);
        self.spawn_runner(id.to_string());
        Ok(job)
    }

    /// Pause every Pending/Running job. Counts only jobs whose status actually
    /// changed (same rule as cancel_all).
    pub fn pause_all(&self) -> AppResult<PauseAllResult> {
        let (paused, errors) = self.run_bulk(
            |j| matches!(j.status, JobStatus::Pending | JobStatus::Running),
            |m, id| m.pause(id),
            |s| *s == JobStatus::Paused,
        )?;
        Ok(PauseAllResult { paused, errors })
    }

    /// Resume every paused job.
    pub fn resume_all(&self) -> AppResult<ResumeAllResult> {
        let (resumed, errors) = self.run_bulk(
            |j| j.status == JobStatus::Paused,
            |m, id| m.resume(id),
            |s| *s == JobStatus::Pending,
        )?;
        Ok(ResumeAllResult { resumed, errors })
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
        if matches!(
            job.status,
            JobStatus::Running | JobStatus::Pending | JobStatus::Paused
        ) {
            // try_cancel also removes the work dir; a deleted Paused row must
            // not leave its partial files behind (the startup cleanup keeps
            // paused ids, so nothing else would ever collect them).
            self.try_cancel(id);
        }
        if delete_file && let Some(path) = job.output_path.as_deref() {
            let p = PathBuf::from(path);
            if p.is_file() {
                let _ = fs::remove_file(p);
            }
        }
        self.db.lock().map_err(lock_err)?.delete_job(id)?;
        if let Ok(mut flags) = self.stop_flags.lock() {
            flags.remove(id);
        }
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

        if let Some(d) = stop_disposition(stop_kind_of(&self.stop_flags, &job_id)) {
            self.apply_stop_disposition(&job_id, d);
            return;
        }

        let save_dir = match self.save_dir() {
            Ok(dir) => dir,
            Err(e) => {
                self.try_fail_job(&job_id, &e.to_string());
                return;
            }
        };
        if let Err(e) = check_save_dir_writable(&save_dir) {
            self.try_fail_job(&job_id, &e.to_string());
            return;
        }

        // Read → status check → stop re-check → write Running must happen in
        // one db critical section: a pause landing between the checks must not
        // leave a zombie Running row (no process, no runner) behind. While the
        // db lock is held, only calls that never re-acquire it may run: Db
        // methods plus stop_kind_of (lock order db → stop_flags, never the
        // reverse); try_fail_job/emit run after the lock is released.
        let mut fail_msg: Option<String> = None;
        let mut start: Option<DownloadJob> = None;
        match self.db.lock() {
            Ok(db) => match db.get_job(&job_id) {
                Ok(job) => {
                    if stop_kind_of(&self.stop_flags, &job_id).is_none()
                        && job.status == JobStatus::Pending
                    {
                        let mut running = job;
                        running.status = JobStatus::Running;
                        // Bake datetime tokens once and persist the result: a
                        // resume/retry re-runs this same path, and re-baking
                        // with a fresh clock would rename the target file and
                        // restart the transfer instead of continuing the .part.
                        running.output_template = naming::bake_local_datetime_tokens(
                            &running.output_template,
                            &chrono::Local::now(),
                        );
                        if let Err(e) = db.update_job(&running) {
                            tracing::debug!(target: "core", "download: 状态写入失败 {job_id}: {e}");
                        }
                        start = Some(running);
                    }
                }
                Err(e) => fail_msg = Some(e.to_string()),
            },
            Err(e) => fail_msg = Some(e.to_string()),
        }
        if let Some(msg) = fail_msg {
            self.try_fail_job(&job_id, &msg);
            return;
        }
        let Some(running) = start else {
            if let Some(d) = stop_disposition(stop_kind_of(&self.stop_flags, &job_id)) {
                self.apply_stop_disposition(&job_id, d);
            }
            return;
        };
        tracing::info!(target: "core", "download: 开始 {job_id}");
        self.emit(&running);

        if let Some(d) = stop_disposition(stop_kind_of(&self.stop_flags, &job_id)) {
            self.apply_stop_disposition(&job_id, d);
            return;
        }

        let work = fsutil::work_dir_for(&self.work_root, &job_id);
        // Only a finished leftover is worth relocating. Fragments from a
        // previous attempt (a merge that never ran: the 0.4.0 macOS failure
        // mode, or a user config that keeps streams separate) are skipped so
        // the retry downloads again instead of delivering a video-only or
        // audio-only file as a success.
        let recoverable = fsutil::find_work_product(&work)
            .filter(|path| !crate::ytdlp::looks_unmerged(path, fsutil::count_media_files(&work)))
            // Fragments coexisting with a candidate product mean the pipeline
            // is mid-flight (a killed merge leaves `…temp.mp4` next to the
            // fragments); never relocate from such a dir — re-run instead.
            .filter(|_| !fsutil::has_fragment_leftovers(&work));
        if let Some(work_path) = recoverable {
            match self.complete_relocation(&job_id, &running, &work_path, &save_dir) {
                Ok(()) => {
                    if let Ok(mut tasks) = self.running_tasks.lock() {
                        tasks.remove(&job_id);
                    }
                    return;
                }
                Err(e) => {
                    self.try_fail_job(&job_id, &e);
                    if let Ok(mut tasks) = self.running_tasks.lock() {
                        tasks.remove(&job_id);
                    }
                    return;
                }
            }
        }

        let db = Arc::clone(&self.db);
        let progress = Arc::clone(&self.progress);
        let stop_flags = Arc::clone(&self.stop_flags);
        let on_progress = {
            let job_id = job_id.clone();
            Box::new(move |update: ProgressUpdate| {
                if stop_kind_of(&stop_flags, &job_id).is_some() {
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
                    if let Err(e) = db.update_job(&current) {
                        tracing::debug!(target: "core", "download: 状态写入失败 {}: {e}", current.id);
                    }
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

        if let Some(d) = stop_disposition(stop_kind_of(&self.stop_flags, &job_id)) {
            self.apply_stop_disposition(&job_id, d);
            return;
        }

        let result = self.downloader.run(&running, on_progress).await;

        if let Some(d) = stop_disposition(stop_kind_of(&self.stop_flags, &job_id)) {
            self.apply_stop_disposition(&job_id, d);
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
                            self.try_fail_job(&job_id, &e);
                        }
                    }
                    None => {
                        self.try_fail_job(
                            &job_id,
                            &format!(
                                "下载完成但未找到输出文件（yt-dlp 回报: {}）",
                                reported_path.display()
                            ),
                        );
                    }
                }
            }
            Err(err) => {
                // Prefer the stop marker over a kill/pipe race error from yt-dlp.
                match stop_disposition(stop_kind_of(&self.stop_flags, &job_id)) {
                    Some(StopDisposition::MarkCancelled) => self.try_cancel(&job_id),
                    Some(StopDisposition::ReturnSilently) => {
                        tracing::debug!(target: "core", "download: 暂停期间忽略错误 {job_id}: {err}");
                    }
                    None => self.try_fail_job(&job_id, &err),
                }
            }
        }

        if let Ok(mut tasks) = self.running_tasks.lock() {
            tasks.remove(&job_id);
        }
    }

    /// fail_job is the last write standing between a task and its Failed row;
    /// when even that fails the job would vanish from the log's perspective,
    /// so leave a debug trace instead of a bare swallow.
    fn try_fail_job(&self, job_id: &str, error: &str) {
        if let Err(e) = self.fail_job(job_id, error.to_string()) {
            tracing::debug!(target: "core", "download: 状态写入失败 {job_id}: {e}");
        }
    }

    /// Same contract as try_fail_job: cancel persists the Cancelled status, so
    /// a swallowed failure must still leave a debug trace.
    fn try_cancel(&self, job_id: &str) {
        if let Err(e) = self.cancel(job_id) {
            tracing::debug!(target: "core", "download: 状态写入失败 {job_id}: {e}");
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
        // Same rule as complete_relocation: a terminal row retires its marker.
        if let Ok(mut flags) = self.stop_flags.lock() {
            flags.remove(job_id);
        }
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
        done.file_size = std::fs::metadata(&dest)
            .ok()
            .map(|m| m.len())
            .or(done.file_size);
        done.error = None;
        if let Ok(db) = self.db.lock()
            && let Err(e) = db.update_job(&done)
        {
            tracing::debug!(target: "core", "download: 状态写入失败 {job_id}: {e}");
        }
        // Terminal writes retire the job's stop flag: a pause that raced this
        // completion otherwise leaves a marker no later path would ever clear.
        if let Ok(mut flags) = self.stop_flags.lock() {
            flags.remove(job_id);
        }
        tracing::info!(target: "core", "download: 完成 {job_id} -> {}", dest.display());
        self.emit(&done);
        Ok(())
    }

    /// Dispatch at a stop checkpoint: re-assert Failed for a cancel; return
    /// silently for a pause — its row (Paused) and work dir are already final,
    /// and calling cancel()/fail_job here would delete the `.part` files or
    /// paint a pause as a failure.
    fn apply_stop_disposition(&self, job_id: &str, disposition: StopDisposition) {
        match disposition {
            StopDisposition::MarkCancelled => self.try_cancel(job_id),
            StopDisposition::ReturnSilently => {
                tracing::debug!(target: "core", "download: 已按暂停停止 {job_id}");
            }
        }
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

pub fn cleanup_orphan_work_dirs(work_root: &Path, keep_job_ids: &[String]) -> usize {
    let Ok(entries) = std::fs::read_dir(work_root) else {
        return 0;
    };
    let mut removed = 0;
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if ent.path().is_dir() && !keep_job_ids.iter().any(|id| id == &name) {
            let _ = std::fs::remove_dir_all(ent.path());
            removed += 1;
        }
    }
    removed
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
            // Tests assume one download at a time; keep this pinned so raising
            // the production default cannot change test premises.
            concurrency: 1,
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
            thumbnail_url: None,
            duration_secs: None,
            file_size: None,
            created_at: None,
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
        // A paused job's work dir holds the `.part` data a resume needs; the
        // startup keep-list must include Paused or the next launch deletes it.
        std::fs::create_dir_all(work_root.join("keep-paused-1")).unwrap();
        std::fs::write(
            work_root.join("keep-paused-1").join("file.f30064.mp4.part"),
            b"x",
        )
        .unwrap();

        cleanup_orphan_work_dirs(
            &work_root,
            &[
                "keep-job-1".into(),
                "keep-failed-1".into(),
                "keep-paused-1".into(),
            ],
        );

        assert!(!work_root.join("orphan-a").exists());
        assert!(!work_root.join("orphan-b").exists());
        assert!(work_root.join("keep-job-1").is_dir());
        assert!(work_root.join("keep-failed-1").is_dir());
        assert!(work_root.join("keep-paused-1").is_dir());
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

    /// run_bulk's accounting contract: an op error is collected as "id: error"
    /// and never aborts the loop, and only genuine status transitions count.
    ///
    /// Value: protects=the Err arm and the counted() gate of the shared bulk
    /// helper (cancel_all/pause_all/resume_all all report through it);
    /// fails_when=the Err arm propagates (bad-1 aborts the run and ok-1 is
    /// never cancelled) or the counted gate is dropped (the Done row's no-op
    /// cancel inflates the count); why_new=bulk tests only cover happy paths;
    /// seam=none
    #[test]
    fn run_bulk_collects_op_errors_and_counts_only_transitions() {
        let dir = tempfile::tempdir().unwrap();
        let (manager, _rx) = test_manager(
            dir.path(),
            &dir.path().join("work"),
            Arc::new(MockDownloader::slow_success(50)) as Arc<dyn Downloader>,
        );

        for (id, status) in [
            ("bad-1", JobStatus::Pending),
            ("ok-1", JobStatus::Pending),
            ("done-1", JobStatus::Done),
        ] {
            let mut job = sample_job(id);
            job.status = status;
            manager.db.lock().unwrap().insert_job(&job).unwrap();
        }

        let (count, errors) = manager
            .run_bulk(
                |j| matches!(j.status, JobStatus::Pending | JobStatus::Done),
                |m, id| {
                    if id == "bad-1" {
                        return Err(AppError::Message("boom".into()));
                    }
                    m.cancel(id)
                },
                |s| *s == JobStatus::Failed,
            )
            .unwrap();

        assert_eq!(errors, vec!["bad-1: boom".to_string()]);
        assert_eq!(count, 1, "only ok-1's cancel is a real transition");
        let jobs = manager.list().unwrap();
        assert!(
            jobs.iter()
                .any(|j| j.id == "ok-1" && j.status == JobStatus::Failed),
            "the loop must continue past a failing op"
        );
        assert!(
            jobs.iter()
                .any(|j| j.id == "bad-1" && j.status == JobStatus::Pending),
            "the failing op must leave its row untouched"
        );
        assert!(
            jobs.iter()
                .any(|j| j.id == "done-1" && j.status == JobStatus::Done),
            "a no-op cancel on a Done row must not be counted nor mutate it"
        );
    }

    /// Clearing history prunes the stop markers of the rows it deletes, or
    /// each cancelled-then-cleared job would leak its flag until app exit.
    ///
    /// Value: protects=the stop_flags retain in clear_finished, and the paused
    /// row (plus its Pause marker) staying operable; fails_when=clear_finished
    /// drops the retain — "failed-1"'s Cancel marker stays in the map — or
    /// widens the delete predicate — the paused row disappears; why_new=pause
    /// markers are only removed by resume/retry, so history-clearing failed
    /// jobs was the untested leak path; seam=none
    #[test]
    fn clear_finished_keeps_files_paused_rows_and_prunes_cleared_flags() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("video.mp4");
        std::fs::write(&file_path, b"data").unwrap();

        {
            let db = Db::open(&dir.path().join("jobs.db")).unwrap();
            let mut pending = sample_job("pending-1");
            pending.status = JobStatus::Pending;
            db.insert_job(&pending).unwrap();

            let mut paused = sample_job("paused-1");
            paused.status = JobStatus::Paused;
            db.insert_job(&paused).unwrap();

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
        {
            let mut flags = manager.stop_flags.lock().unwrap();
            flags.insert("failed-1".into(), StopKind::Cancel);
            flags.insert("paused-1".into(), StopKind::Pause);
        }
        let result = manager.clear_finished().unwrap();
        assert_eq!(result.cleared, 2);
        assert!(file_path.is_file(), "local file must remain");

        let jobs = manager.list().unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().any(|j| j.id == "pending-1"));
        assert!(
            jobs.iter()
                .any(|j| j.id == "paused-1" && j.status == JobStatus::Paused),
            "a paused row must survive clearing history"
        );

        let flags = manager.stop_flags.lock().unwrap();
        assert!(
            !flags.contains_key("failed-1"),
            "the cleared row's stop marker must be pruned with it"
        );
        assert_eq!(
            flags.get("paused-1"),
            Some(&StopKind::Pause),
            "the surviving paused row's marker must stay (resume reads it)"
        );
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
    async fn retry_redownloads_when_leftover_is_only_fragments() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();

        let job_id = "job-fragment-retry";
        let work = fsutil::work_dir_for(&work_root, job_id);
        std::fs::create_dir_all(&work).unwrap();
        // What the 0.4.0 macOS bug left behind: two stream fragments, no merge.
        std::fs::write(work.join("demo.f100026.mp4"), b"video-only").unwrap();
        std::fs::write(work.join("demo.f30280.m4a"), b"audio-only").unwrap();

        let called = Arc::new(AtomicBool::new(false));
        struct FragmentRetryMock {
            called: Arc<AtomicBool>,
            scratch: tempfile::TempDir,
        }
        #[async_trait]
        impl Downloader for FragmentRetryMock {
            async fn run(
                &self,
                _job: &DownloadJob,
                _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
            ) -> Result<PathBuf, String> {
                self.called.store(true, Ordering::SeqCst);
                let path = self.scratch.path().join("demo.mp4");
                std::fs::write(&path, b"merged").map_err(|e| e.to_string())?;
                Ok(path)
            }
        }

        let db = Db::open(&save_dir.join("jobs.db")).unwrap();
        let mut failed = sample_job(job_id);
        failed.status = JobStatus::Failed;
        failed.error =
            Some("音视频流未合成完整文件（下载组件异常）。请重新安装影取后重试。".into());
        db.insert_job(&failed).unwrap();

        let (emitter, _rx) = ChannelProgressEmitter::new();
        let manager = DownloadManager::new(
            db,
            test_settings(&save_dir),
            Arc::new(FragmentRetryMock {
                called: Arc::clone(&called),
                scratch: tempfile::tempdir().unwrap(),
            }) as Arc<dyn Downloader>,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            work_root.clone(),
        )
        .unwrap();

        manager.retry(job_id).unwrap();
        wait_for_status(&manager, job_id, JobStatus::Done).await;

        assert!(
            called.load(Ordering::SeqCst),
            "a fragment-only work dir must be re-downloaded, not relocated"
        );
        assert!(save_dir.join("demo.mp4").is_file());
        assert!(!save_dir.join("demo.f100026.mp4").exists());
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

        // Concurrency is pinned to 1 in test_settings — only one should run.
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

    #[tokio::test]
    async fn enqueue_classified_flags_duplicate_active() {
        let dir = tempfile::tempdir().unwrap();
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();
        let db = Db::open(&save_dir.join("jobs.db")).unwrap();

        // Pre-seed a Pending job with the same video/page: the second enqueue must report DuplicateActive.
        let mut active = sample_job("job-active");
        active.video_id = "BV1active".into();
        active.page_index = 1;
        active.status = JobStatus::Pending;
        db.insert_job(&active).unwrap();

        let (emitter, _rx) = ChannelProgressEmitter::new();
        struct NeverDownloader;
        #[async_trait]
        impl Downloader for NeverDownloader {
            async fn run(
                &self,
                _: &DownloadJob,
                _: Box<dyn Fn(ProgressUpdate) + Send>,
            ) -> Result<PathBuf, String> {
                Err("should not run".into())
            }
        }
        let manager = DownloadManager::new(
            db,
            test_settings(&save_dir),
            Arc::new(NeverDownloader) as Arc<dyn Downloader>,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            dir.path().join("work"),
        )
        .unwrap();

        let mut fresh = sample_job("job-fresh");
        fresh.video_id = "BV1other".into();
        fresh.page_index = 1;
        assert_eq!(
            manager.enqueue_classified(&mut fresh, false).unwrap(),
            EnqueueKind::Enqueued
        );

        let mut dup = sample_job("job-dup");
        dup.video_id = "BV1active".into();
        dup.page_index = 1;
        assert_eq!(
            manager.enqueue_classified(&mut dup, false).unwrap(),
            EnqueueKind::DuplicateActive
        );

        // Single-video path message regression: must contain the exact existing notice.
        let err = manager.enqueue(dup, false).unwrap_err().to_string();
        assert!(err.contains("已在下载队列中"), "unexpected: {err}");
        assert!(err.contains("BV1active"), "unexpected: {err}");
    }

    // ---- pause / resume ----

    fn manager_for(
        dir: &Path,
        downloader: Arc<dyn Downloader>,
        stop_flags: StopFlags,
    ) -> DownloadManager {
        let db = Db::open(&dir.join("jobs.db")).unwrap();
        let (emitter, _rx) = ChannelProgressEmitter::new();
        DownloadManager::new_with_stop_flags(
            db,
            test_settings(dir),
            downloader,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            dir.join("work"),
            stop_flags,
        )
        .unwrap()
    }

    struct CountingDownloader {
        runs: Arc<Mutex<Vec<String>>>,
        sleep_ms: u64,
        succeed: bool,
        scratch: tempfile::TempDir,
    }

    impl CountingDownloader {
        fn new(sleep_ms: u64, succeed: bool) -> Self {
            Self {
                runs: Arc::new(Mutex::new(Vec::new())),
                sleep_ms,
                succeed,
                scratch: tempfile::tempdir().unwrap(),
            }
        }
    }

    #[async_trait]
    impl Downloader for CountingDownloader {
        async fn run(
            &self,
            job: &DownloadJob,
            _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
        ) -> Result<PathBuf, String> {
            if let Ok(mut runs) = self.runs.lock() {
                runs.push(job.id.clone());
            }
            if self.sleep_ms > 0 {
                sleep(Duration::from_millis(self.sleep_ms)).await;
            }
            if !self.succeed {
                return Err("mock failure".into());
            }
            let path = self.scratch.path().join(format!("{}.mp4", job.id));
            std::fs::write(&path, b"mock").map_err(|e| e.to_string())?;
            Ok(path)
        }
    }

    #[test]
    fn stop_disposition_covers_both_kinds() {
        assert_eq!(stop_disposition(None), None);
        assert_eq!(
            stop_disposition(Some(StopKind::Cancel)),
            Some(StopDisposition::MarkCancelled)
        );
        assert_eq!(
            stop_disposition(Some(StopKind::Pause)),
            Some(StopDisposition::ReturnSilently)
        );
    }

    #[test]
    fn err_cleanup_only_cancel_and_real_errors_remove_the_dir() {
        assert!(err_cleanup_removes_work_dir(None));
        assert!(err_cleanup_removes_work_dir(Some(StopKind::Cancel)));
        assert!(!err_cleanup_removes_work_dir(Some(StopKind::Pause)));
    }

    #[tokio::test]
    async fn pause_pending_job_never_starts_the_downloader() {
        let dir = tempfile::tempdir().unwrap();
        // concurrency=1 (test_settings): job-a holds the only permit, so job-b
        // stays Pending while paused. This also pins the lock-order fix — the
        // paused row must survive job-b's runner waking up later.
        let downloader = Arc::new(CountingDownloader::new(400, true));
        let manager = manager_for(
            dir.path(),
            downloader.clone(),
            Arc::new(Mutex::new(HashMap::new())),
        );

        let mut a = sample_job("job-a");
        a.video_id = "BV1a".into();
        manager.enqueue(a, false).unwrap();
        let mut b = sample_job("job-b");
        b.video_id = "BV1b".into();
        manager.enqueue(b, false).unwrap();

        sleep(Duration::from_millis(80)).await;
        let paused = manager.pause("job-b").unwrap();
        assert_eq!(paused.status, JobStatus::Paused);

        wait_for_status(&manager, "job-a", JobStatus::Done).await;
        sleep(Duration::from_millis(120)).await;
        let jobs = manager.list().unwrap();
        let b_status = jobs
            .iter()
            .find(|j| j.id == "job-b")
            .map(|j| j.status.clone());
        assert_eq!(b_status, Some(JobStatus::Paused));
        let runs = downloader.runs.lock().unwrap().clone();
        assert!(
            !runs.contains(&"job-b".to_string()),
            "downloader ran for a paused job: {runs:?}"
        );
    }

    #[tokio::test]
    async fn pause_running_job_keeps_progress_and_workdir() {
        let dir = tempfile::tempdir().unwrap();
        let downloader = Arc::new(CountingDownloader::new(600, false));
        let manager = manager_for(dir.path(), downloader, Arc::new(Mutex::new(HashMap::new())));
        let mut job = sample_job("job-p");
        job.video_id = "BV1p".into();
        manager.enqueue(job, false).unwrap();
        wait_for_status(&manager, "job-p", JobStatus::Running).await;

        {
            let db = manager.db.lock().unwrap();
            let mut j = db.get_job("job-p").unwrap();
            j.progress = 0.42;
            db.update_job(&j).unwrap();
        }
        let work = fsutil::work_dir_for(&manager.work_root, "job-p");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("demo.f30064.mp4.part"), b"x").unwrap();

        let paused = manager.pause("job-p").unwrap();
        assert_eq!(paused.status, JobStatus::Paused);
        assert_eq!(paused.progress, 0.42, "progress must survive a pause");
        assert!(paused.error.is_none(), "a pause is not a failure");
        assert!(
            work.join("demo.f30064.mp4.part").exists(),
            "the work dir (partial files) must be kept"
        );

        // Whatever abort-vs-error race plays out, the final state stays Paused.
        sleep(Duration::from_millis(200)).await;
        let jobs = manager.list().unwrap();
        let j = jobs.iter().find(|j| j.id == "job-p").unwrap();
        assert_eq!(j.status, JobStatus::Paused);
        assert!(j.error.is_none());
    }

    #[tokio::test]
    async fn pause_error_path_never_marks_failed() {
        let dir = tempfile::tempdir().unwrap();
        let downloader = Arc::new(CountingDownloader::new(300, false));
        let manager = manager_for(dir.path(), downloader, Arc::new(Mutex::new(HashMap::new())));
        let mut job = sample_job("job-e");
        job.video_id = "BV1e".into();
        manager.enqueue(job, false).unwrap();
        wait_for_status(&manager, "job-e", JobStatus::Running).await;

        // Simulate the state pause() leaves WITHOUT aborting the runner, so the
        // downloader's Err actually reaches run_job's error handling.
        manager
            .stop_flags
            .lock()
            .unwrap()
            .insert("job-e".into(), StopKind::Pause);
        {
            let db = manager.db.lock().unwrap();
            let mut j = db.get_job("job-e").unwrap();
            j.status = JobStatus::Paused;
            db.update_job(&j).unwrap();
        }

        sleep(Duration::from_millis(500)).await;
        let jobs = manager.list().unwrap();
        let j = jobs.iter().find(|j| j.id == "job-e").unwrap();
        assert_eq!(
            j.status,
            JobStatus::Paused,
            "a downloader Err after pause must not fail the job"
        );
        assert!(j.error.is_none());
    }

    #[tokio::test]
    async fn resume_keeps_progress_and_reruns_the_downloader() {
        let dir = tempfile::tempdir().unwrap();
        let downloader = Arc::new(CountingDownloader::new(300, true));
        let manager = manager_for(
            dir.path(),
            downloader.clone(),
            Arc::new(Mutex::new(HashMap::new())),
        );
        let mut job = sample_job("job-r");
        job.video_id = "BV1r".into();
        manager.enqueue(job, false).unwrap();
        wait_for_status(&manager, "job-r", JobStatus::Running).await;
        {
            let db = manager.db.lock().unwrap();
            let mut j = db.get_job("job-r").unwrap();
            j.progress = 0.37;
            db.update_job(&j).unwrap();
        }
        manager.pause("job-r").unwrap();

        let resumed = manager.resume("job-r").unwrap();
        assert_eq!(resumed.status, JobStatus::Pending);
        assert_eq!(resumed.progress, 0.37, "resume must not reset progress");
        assert!(resumed.error.is_none());

        wait_for_status(&manager, "job-r", JobStatus::Done).await;
        let runs = downloader.runs.lock().unwrap().clone();
        assert_eq!(runs, vec!["job-r".to_string(), "job-r".to_string()]);
    }

    #[tokio::test]
    async fn pause_and_resume_reject_terminal_and_missing_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            Arc::new(Mutex::new(HashMap::new())),
        );

        let mut done = sample_job("job-done");
        done.status = JobStatus::Done;
        manager.db.lock().unwrap().insert_job(&done).unwrap();
        // A finished job is returned unchanged (silent no-op), not an error —
        // the UI renders 「任务已结束，未执行暂停」 from the returned status.
        let returned = manager.pause("job-done").unwrap();
        assert_eq!(returned.status, JobStatus::Done);
        let jobs = manager.list().unwrap();
        assert_eq!(
            jobs.iter().find(|j| j.id == "job-done").unwrap().status,
            JobStatus::Done,
            "a finished job must not be overwritten to Paused"
        );

        let mut failed = sample_job("job-f");
        failed.status = JobStatus::Failed;
        failed.error = Some("原始错误".into());
        manager.db.lock().unwrap().insert_job(&failed).unwrap();
        let returned = manager.pause("job-f").unwrap();
        assert_eq!(returned.status, JobStatus::Failed);
        let jobs = manager.list().unwrap();
        let j = jobs.iter().find(|j| j.id == "job-f").unwrap();
        assert_eq!(j.status, JobStatus::Failed);
        assert_eq!(j.error.as_deref(), Some("原始错误"), "error must survive");

        // Double-pausing an already-paused job stays an error (the intended
        // backstop for a stale double click).
        let mut paused = sample_job("job-p2");
        paused.video_id = "BV1p2".into();
        paused.status = JobStatus::Paused;
        manager.db.lock().unwrap().insert_job(&paused).unwrap();
        let err = manager.pause("job-p2").unwrap_err().to_string();
        assert!(err.contains("只能暂停"), "unexpected: {err}");

        let err = manager.resume("job-done").unwrap_err().to_string();
        assert!(err.contains("只能继续"), "unexpected: {err}");
        let err = manager.pause("ghost").unwrap_err().to_string();
        assert!(err.contains("该任务已不存在"), "unexpected: {err}");
        let err = manager.resume("ghost").unwrap_err().to_string();
        assert!(err.contains("该任务已不存在"), "unexpected: {err}");
    }

    #[tokio::test]
    async fn delete_paused_removes_row_workdir_and_flag() {
        let dir = tempfile::tempdir().unwrap();
        let flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            flags.clone(),
        );

        let mut job = sample_job("job-del");
        job.status = JobStatus::Paused;
        manager.db.lock().unwrap().insert_job(&job).unwrap();
        let work = fsutil::work_dir_for(&manager.work_root, "job-del");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("demo.f30064.mp4.part"), b"x").unwrap();
        flags
            .lock()
            .unwrap()
            .insert("job-del".into(), StopKind::Pause);

        manager.delete("job-del", false).unwrap();
        assert!(
            manager
                .db
                .lock()
                .unwrap()
                .find_job("job-del")
                .unwrap()
                .is_none(),
            "row must be deleted"
        );
        assert!(!work.exists(), "paused work dir must be removed on delete");
        assert!(
            stop_kind_of(&flags, "job-del").is_none(),
            "the stop flag entry must be cleared"
        );
    }

    #[tokio::test]
    async fn cancel_all_includes_paused() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            Arc::new(Mutex::new(HashMap::new())),
        );
        for (id, status) in [
            ("job-1", JobStatus::Pending),
            ("job-2", JobStatus::Running),
            ("job-3", JobStatus::Paused),
        ] {
            let mut job = sample_job(id);
            job.video_id = format!("BV1{id}");
            job.status = status;
            manager.db.lock().unwrap().insert_job(&job).unwrap();
        }

        let result = manager.cancel_all().unwrap();
        assert_eq!(result.cancelled, 3, "errors: {:?}", result.errors);
        assert!(result.errors.is_empty());
        for job in manager.list().unwrap() {
            assert_eq!(
                job.status,
                JobStatus::Failed,
                "job {} not cancelled",
                job.id
            );
        }
    }

    #[tokio::test]
    async fn paused_job_still_blocks_duplicate_enqueue() {
        let dir = tempfile::tempdir().unwrap();
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            Arc::new(Mutex::new(HashMap::new())),
        );
        let mut active = sample_job("job-held");
        active.video_id = "BV1held".into();
        active.status = JobStatus::Paused;
        manager.db.lock().unwrap().insert_job(&active).unwrap();

        let mut dup = sample_job("job-dup2");
        dup.video_id = "BV1held".into();
        assert_eq!(
            manager.enqueue_classified(&mut dup, false).unwrap(),
            EnqueueKind::DuplicateActive,
            "a paused job must still hold the active slot"
        );
    }

    #[tokio::test]
    async fn retry_clears_stop_flags() {
        let dir = tempfile::tempdir().unwrap();
        let flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            flags.clone(),
        );
        let mut job = sample_job("job-rt");
        job.status = JobStatus::Failed;
        manager.db.lock().unwrap().insert_job(&job).unwrap();
        flags
            .lock()
            .unwrap()
            .insert("job-rt".into(), StopKind::Cancel);

        manager.retry("job-rt").unwrap();
        assert!(
            stop_kind_of(&flags, "job-rt").is_none(),
            "retry must clear the stop flag (else the new runner exits instantly)"
        );
    }

    /// A cancel that can never reach the runner Err dispatch — unknown id or an
    /// already-Done row — must retire its marker instead of leaking it.
    #[test]
    fn cancel_of_unknown_or_finished_job_leaves_no_stop_marker() {
        let dir = tempfile::tempdir().unwrap();
        let flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            flags.clone(),
        );

        assert!(manager.cancel("ghost").is_err());
        assert!(
            stop_kind_of(&flags, "ghost").is_none(),
            "a failed cancel must not leak its marker"
        );

        let mut done = sample_job("job-done-cancel");
        done.status = JobStatus::Done;
        manager.db.lock().unwrap().insert_job(&done).unwrap();
        manager.cancel("job-done-cancel").unwrap();
        assert!(
            stop_kind_of(&flags, "job-done-cancel").is_none(),
            "a no-op cancel on a done row must not leak its marker"
        );
    }

    /// A merge killed mid-flight leaves yt-dlp's `….temp.<ext>` writer file
    /// behind; the recovery shortcut must re-run instead of delivering it.
    ///
    /// Value: protects=the `.temp.` exclusion that stops a half-merged file
    /// from being relocated to the save dir as a finished download (the 0.4.0
    /// macOS class bug); fails_when=find_work_product accepts `….temp.<ext>`
    /// again — the temp file is then relocated and the downloader never runs;
    /// why_new=existing tests cover a fragment-only leftover and a normal
    /// product, but never the in-flight merge file;
    /// seam=none
    #[tokio::test]
    async fn retry_reruns_when_leftover_is_only_an_inflight_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();

        let job_id = "job-temp-merge";
        let work = fsutil::work_dir_for(&work_root, job_id);
        std::fs::create_dir_all(&work).unwrap();
        // What a pause during the ffmpeg merge leaves: `<final>.temp.<ext>`,
        // half-written. Only ffmpeg's rename makes it a real product.
        std::fs::write(work.join("demo.temp.mp4"), b"half-merged").unwrap();

        let called = Arc::new(AtomicBool::new(false));
        struct TempMergeRetryMock {
            called: Arc<AtomicBool>,
            scratch: tempfile::TempDir,
        }
        #[async_trait]
        impl Downloader for TempMergeRetryMock {
            async fn run(
                &self,
                _job: &DownloadJob,
                _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
            ) -> Result<PathBuf, String> {
                self.called.store(true, Ordering::SeqCst);
                let path = self.scratch.path().join("demo.mp4");
                std::fs::write(&path, b"complete").map_err(|e| e.to_string())?;
                Ok(path)
            }
        }

        let db = Db::open(&save_dir.join("jobs.db")).unwrap();
        let mut failed = sample_job(job_id);
        failed.status = JobStatus::Failed;
        failed.error = Some("用户暂停下载".into());
        db.insert_job(&failed).unwrap();

        let (emitter, _rx) = ChannelProgressEmitter::new();
        let manager = DownloadManager::new(
            db,
            test_settings(&save_dir),
            Arc::new(TempMergeRetryMock {
                called: Arc::clone(&called),
                scratch: tempfile::tempdir().unwrap(),
            }) as Arc<dyn Downloader>,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            work_root.clone(),
        )
        .unwrap();

        manager.retry(job_id).unwrap();
        wait_for_status(&manager, job_id, JobStatus::Done).await;

        assert!(
            called.load(Ordering::SeqCst),
            "an in-flight merge file must be re-downloaded, not relocated"
        );
        assert!(
            !save_dir.join("demo.temp.mp4").exists(),
            "the half-merged temp file must never reach the save dir"
        );
        assert_eq!(
            std::fs::read(save_dir.join("demo.mp4")).unwrap(),
            b"complete"
        );
        assert!(!work_root.join(job_id).exists());
    }

    /// A candidate product sitting next to stream fragments (a `-k` config, or
    /// a killed merge) must be re-run, not relocated. The fragment is left
    /// empty on purpose: find_work_product skips empty files, so the stale
    /// product is picked deterministically and the has_fragment_leftovers
    /// filter is the only check that can block the relocation (a non-empty
    /// fragment could be picked first by read_dir order, in which case the
    /// fragment-infix check in looks_unmerged would mask this filter).
    ///
    /// Value: protects=the has_fragment_leftovers coexist filter in run_job;
    /// fails_when=that filter is dropped — the stale demo.mp4 is relocated as
    /// a finished download and the downloader never runs; why_new=existing
    /// tests cover a fragment-only leftover and a product-only leftover, but
    /// never a product coexisting with fragments; seam=none
    #[tokio::test]
    async fn retry_reruns_when_product_coexists_with_fragments() {
        let dir = tempfile::tempdir().unwrap();
        let work_root = dir.path().join("download-work");
        let save_dir = dir.path().join("videos");
        std::fs::create_dir_all(&save_dir).unwrap();

        let job_id = "job-product-plus-fragments";
        let work = fsutil::work_dir_for(&work_root, job_id);
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("demo.mp4"), b"stale-product").unwrap();
        std::fs::write(work.join("demo.f30016.mp4"), b"").unwrap();

        let called = Arc::new(AtomicBool::new(false));
        struct ProductFragmentRetryMock {
            called: Arc<AtomicBool>,
            scratch: tempfile::TempDir,
        }
        #[async_trait]
        impl Downloader for ProductFragmentRetryMock {
            async fn run(
                &self,
                _job: &DownloadJob,
                _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
            ) -> Result<PathBuf, String> {
                self.called.store(true, Ordering::SeqCst);
                let path = self.scratch.path().join("demo.mp4");
                std::fs::write(&path, b"complete").map_err(|e| e.to_string())?;
                Ok(path)
            }
        }

        let db = Db::open(&save_dir.join("jobs.db")).unwrap();
        let mut failed = sample_job(job_id);
        failed.status = JobStatus::Failed;
        failed.error = Some("用户暂停下载".into());
        db.insert_job(&failed).unwrap();

        let (emitter, _rx) = ChannelProgressEmitter::new();
        let manager = DownloadManager::new(
            db,
            test_settings(&save_dir),
            Arc::new(ProductFragmentRetryMock {
                called: Arc::clone(&called),
                scratch: tempfile::tempdir().unwrap(),
            }) as Arc<dyn Downloader>,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            work_root.clone(),
        )
        .unwrap();

        manager.retry(job_id).unwrap();
        wait_for_status(&manager, job_id, JobStatus::Done).await;

        assert!(
            called.load(Ordering::SeqCst),
            "a product coexisting with fragments must be re-downloaded, not relocated"
        );
        assert_eq!(
            std::fs::read(save_dir.join("demo.mp4")).unwrap(),
            b"complete",
            "the stale work-dir product must never be delivered"
        );
        assert!(!save_dir.join("demo.f30016.mp4").exists());
        assert!(!work_root.join(job_id).exists());
    }

    /// A resume must reuse the exact output name the paused run was writing.
    /// A `%(timestamp>…)s`-style token is baked once and persisted with the
    /// Running write; re-baking on resume would rename the target and restart
    /// the download instead of continuing the .part files.
    ///
    /// Value: protects=the baked-template persistence in run_job (resume and
    /// retry target the same work-dir names); fails_when=the bake result is
    /// not persisted — the mock receives the raw token on the first run, and a
    /// resume re-bakes different bytes (the 1.1s gap forces a new second);
    /// why_new=every other test template is token-free, while timestamp
    /// templates are a supported, user-visible settings shape; seam=none
    #[tokio::test]
    async fn resume_reuses_the_baked_output_template() {
        let dir = tempfile::tempdir().unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        struct TemplateCapturingDownloader {
            seen: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl Downloader for TemplateCapturingDownloader {
            async fn run(
                &self,
                job: &DownloadJob,
                _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
            ) -> Result<PathBuf, String> {
                if let Ok(mut seen) = self.seen.lock() {
                    seen.push(job.output_template.clone());
                }
                sleep(Duration::from_millis(5000)).await;
                Err("unreachable".into())
            }
        }

        let manager = manager_for(
            dir.path(),
            Arc::new(TemplateCapturingDownloader {
                seen: Arc::clone(&seen),
            }),
            Arc::new(Mutex::new(HashMap::new())),
        );

        let job_id = "job-token-template";
        let mut job = sample_job(job_id);
        job.output_template = "%(timestamp>%Y-%m-%dT%H-%M-%S)s_demo [%(id)s].%(ext)s".into();
        manager.enqueue(job, false).unwrap();
        wait_for_status(&manager, job_id, JobStatus::Running).await;

        for _ in 0..200 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
        manager.pause(job_id).unwrap();

        // Cross a wall-clock second so any re-bake at resume produces different bytes.
        sleep(Duration::from_millis(1100)).await;
        manager.resume(job_id).unwrap();
        for _ in 0..200 {
            if seen.lock().unwrap().len() >= 2 {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "the resume must re-run the downloader");
        assert!(
            !seen[0].contains("timestamp"),
            "the token must be baked before the downloader sees it: {}",
            seen[0]
        );
        assert_eq!(
            seen[0], seen[1],
            "resume must reuse the baked name from the paused run"
        );
        let jobs = manager.list().unwrap();
        let row = jobs.iter().find(|j| j.id == job_id).unwrap();
        assert_eq!(
            row.output_template, seen[0],
            "the baked template must be persisted on the job row"
        );
    }

    /// Queue-level pause/resume: pause_all covers exactly Pending|Running and
    /// resume_all covers exactly Paused; counts reflect actual changes.
    ///
    /// Value: protects=the header buttons' contract — which rows each bulk
    /// action may touch and what count is reported; fails_when=pause_all drops
    /// a state or double-counts an already-paused row, or resume_all resumes/
    /// counts the wrong set; why_new=cancel_all has a test but the two new
    /// bulk methods have none; seam=none
    #[tokio::test]
    async fn pause_all_and_resume_all_count_only_actual_changes() {
        fn status_of(jobs: &[DownloadJob], id: &str) -> JobStatus {
            jobs.iter().find(|j| j.id == id).unwrap().status.clone()
        }

        let dir = tempfile::tempdir().unwrap();
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            Arc::new(Mutex::new(HashMap::new())),
        );
        for (id, status) in [
            ("job-open", JobStatus::Pending),
            ("job-held", JobStatus::Paused),
            ("job-done", JobStatus::Done),
            ("job-failed", JobStatus::Failed),
        ] {
            let mut job = sample_job(id);
            job.video_id = format!("BV{id}");
            job.status = status;
            manager.db.lock().unwrap().insert_job(&job).unwrap();
        }

        let paused = manager.pause_all().unwrap();
        assert!(paused.errors.is_empty(), "errors: {:?}", paused.errors);
        assert_eq!(
            paused.paused, 1,
            "only the pending row changed; an already-paused row is not re-counted"
        );
        let jobs = manager.list().unwrap();
        assert_eq!(status_of(&jobs, "job-open"), JobStatus::Paused);
        assert_eq!(status_of(&jobs, "job-held"), JobStatus::Paused);
        assert_eq!(status_of(&jobs, "job-done"), JobStatus::Done);
        assert_eq!(status_of(&jobs, "job-failed"), JobStatus::Failed);

        let resumed = manager.resume_all().unwrap();
        assert!(resumed.errors.is_empty(), "errors: {:?}", resumed.errors);
        assert_eq!(resumed.resumed, 2, "both paused rows must resume");
        // Resumed rows re-enter the pipeline and complete.
        wait_for_status(&manager, "job-open", JobStatus::Done).await;
        wait_for_status(&manager, "job-held", JobStatus::Done).await;
    }

    /// A stop flag landing after the runner's early checks but before it
    /// writes Running must not leave a zombie Running row (no process, no
    /// runner) behind. The runner is parked on the settings lock `run_job`
    /// takes for save_dir(); the pause flag is set in that window, with the
    /// row deliberately still Pending — exactly the interleaving pause()'s
    /// flag-set / DB-write split can produce.
    ///
    /// Value: protects=the in-critical-section stop re-check that keeps a
    /// paused row out of "下载中"; fails_when=the re-check is dropped — the
    /// runner writes Running even though a stop flag is set, and the row stays
    /// Running with nothing behind it; why_new=pause_pending_job_never_starts
    /// _the_downloader aborts the runner, so no test exercises the
    /// critical-section write path under a racing flag; seam=none
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_flag_racing_runner_start_never_writes_running() {
        let dir = tempfile::tempdir().unwrap();
        let downloader = Arc::new(CountingDownloader::new(0, true));
        let manager = manager_for(
            dir.path(),
            downloader.clone(),
            Arc::new(Mutex::new(HashMap::new())),
        );

        let mut job = sample_job("job-race");
        job.video_id = "BV1race".into();
        manager.db.lock().unwrap().insert_job(&job).unwrap();

        // Park the runner before the "write Running" critical section.
        let settings_guard = manager.settings.lock().unwrap();
        manager.spawn_runner("job-race".to_string());
        std::thread::sleep(Duration::from_millis(150));

        manager
            .stop_flags
            .lock()
            .unwrap()
            .insert("job-race".into(), StopKind::Pause);
        drop(settings_guard);
        sleep(Duration::from_millis(200)).await;

        let jobs = manager.list().unwrap();
        let j = jobs.iter().find(|j| j.id == "job-race").unwrap();
        assert_eq!(
            j.status,
            JobStatus::Pending,
            "the runner must not write Running once a stop flag is set"
        );
        assert!(
            !downloader
                .runs
                .lock()
                .unwrap()
                .contains(&"job-race".to_string()),
            "the downloader must never start for the stopped job"
        );
    }

    /// Value: protects=the terminal-state re-read in pause() — a download that
    /// finishes mid-kill must stay Done and its Pause marker must be dropped;
    /// fails_when=the locked re-read writes Paused unconditionally or skips the
    /// marker cleanup; why_new=existing tests only hit the early guard, never
    /// this locked branch; seam=none
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_never_overwrites_a_job_that_finished_mid_kill() {
        let dir = tempfile::tempdir().unwrap();
        let flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            flags.clone(),
        );

        let mut job = sample_job("job-race-done");
        job.video_id = "BV1race".into();
        job.status = JobStatus::Running;
        manager.db.lock().unwrap().insert_job(&job).unwrap();

        // Park pause() at kill_download by holding the children lock: the Pause
        // marker appearing proves the preliminary read already passed, so the
        // interleaving is deterministic — no sleeps guessing at a race window.
        let children_guard = manager.children.lock().unwrap();
        let paused_manager = manager.clone();
        let handle = std::thread::spawn(move || paused_manager.pause("job-race-done"));
        for _ in 0..400 {
            if stop_kind_of(&flags, "job-race-done") == Some(StopKind::Pause) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            stop_kind_of(&flags, "job-race-done"),
            Some(StopKind::Pause),
            "pause() must have passed its preliminary guard and set the marker"
        );

        // The download reaches Done while pause() is parked mid-kill.
        {
            let db = manager.db.lock().unwrap();
            let mut j = db.get_job("job-race-done").unwrap();
            j.status = JobStatus::Done;
            j.progress = 1.0;
            db.update_job(&j).unwrap();
        }
        drop(children_guard);

        let returned = handle.join().unwrap().unwrap();
        assert_eq!(
            returned.status,
            JobStatus::Done,
            "a finished job must not be flipped to Paused"
        );
        let jobs = manager.list().unwrap();
        assert_eq!(
            jobs.iter()
                .find(|j| j.id == "job-race-done")
                .unwrap()
                .status,
            JobStatus::Done
        );
        assert!(
            stop_kind_of(&flags, "job-race-done").is_none(),
            "the pause marker must be dropped when the job won the race"
        );
    }

    /// Value: protects=resume()'s guarded marker clear — a racing Cancel marker
    /// survives and the spawned runner re-asserts Failed (no ghost Pending);
    /// fails_when=resume clears the marker unconditionally; why_new=no test
    /// covered the resume-vs-cancel marker asymmetry; seam=none
    #[tokio::test]
    async fn resume_does_not_clobber_a_racing_cancel_marker() {
        let dir = tempfile::tempdir().unwrap();
        let flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            flags.clone(),
        );
        let mut job = sample_job("job-rv");
        job.video_id = "BV1rv".into();
        job.status = JobStatus::Paused;
        manager.db.lock().unwrap().insert_job(&job).unwrap();
        flags
            .lock()
            .unwrap()
            .insert("job-rv".into(), StopKind::Cancel);

        let resumed = manager.resume("job-rv").unwrap();
        assert_eq!(resumed.status, JobStatus::Pending);
        assert_eq!(
            stop_kind_of(&flags, "job-rv"),
            Some(StopKind::Cancel),
            "resume must not erase a Cancel marker"
        );
        // The spawned runner sees the marker and re-asserts Failed.
        wait_for_status(&manager, "job-rv", JobStatus::Failed).await;
    }

    /// Value: protects=pause()'s mid-flight row-vanish cleanup (the Pause
    /// marker must not outlive its deleted row); fails_when=the locked
    /// re-read's None arm returns without flags.remove; why_new=only the
    /// preliminary guard's missing-row path had a test; seam=none
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_row_vanishing_mid_flight_drops_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        let flags: StopFlags = Arc::new(Mutex::new(HashMap::new()));
        let manager = manager_for(
            dir.path(),
            Arc::new(CountingDownloader::new(0, true)),
            flags.clone(),
        );
        let mut job = sample_job("job-vanish");
        job.video_id = "BV1vanish".into();
        job.status = JobStatus::Running;
        manager.db.lock().unwrap().insert_job(&job).unwrap();

        // Park pause() after its marker insert (same technique as the
        // terminal-race test), then delete the row out from under it.
        let children_guard = manager.children.lock().unwrap();
        let paused_manager = manager.clone();
        let handle = std::thread::spawn(move || paused_manager.pause("job-vanish"));
        for _ in 0..400 {
            if stop_kind_of(&flags, "job-vanish") == Some(StopKind::Pause) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(stop_kind_of(&flags, "job-vanish"), Some(StopKind::Pause));
        // manager.delete would block on the children lock this test holds.
        manager.db.lock().unwrap().delete_job("job-vanish").unwrap();
        drop(children_guard);

        let err = handle.join().unwrap().unwrap_err().to_string();
        assert!(err.contains("该任务已不存在"), "unexpected: {err}");
        assert!(
            stop_kind_of(&flags, "job-vanish").is_none(),
            "the pause marker must not outlive its deleted row"
        );
    }

    /// Value: protects=the startup keep-list wiring — paused (and failed) rows
    /// keep their work dirs, done rows do not; fails_when=the keep predicate
    /// drops Paused (its `.part` files would be deleted on next launch);
    /// why_new=build_app_state needs an AppHandle, so the pure keep predicate
    /// had no test; seam=none
    #[test]
    fn work_dir_keep_ids_covers_every_non_terminal_status() {
        let mut jobs = Vec::new();
        for (id, status) in [
            ("j-pending", JobStatus::Pending),
            ("j-running", JobStatus::Running),
            ("j-paused", JobStatus::Paused),
            ("j-failed", JobStatus::Failed),
            ("j-done", JobStatus::Done),
        ] {
            let mut job = sample_job(id);
            job.status = status;
            jobs.push(job);
        }
        let kept = work_dir_keep_ids(&jobs);
        for id in ["j-pending", "j-running", "j-paused", "j-failed"] {
            assert!(
                kept.contains(&id.to_string()),
                "{id} must keep its work dir"
            );
        }
        assert!(!kept.contains(&"j-done".to_string()));
    }
}
