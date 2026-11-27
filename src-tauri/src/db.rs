use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{AppError, AppResult};
use crate::models::{DownloadJob, JobConflict, JobStatus, VideoMeta};
use crate::resolve_cache;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  url TEXT NOT NULL,
  video_id TEXT NOT NULL,
  page_index INTEGER NOT NULL,
  format_id TEXT NOT NULL,
  audio_format TEXT,
  thumbnail_url TEXT,
  duration_secs INTEGER,
  file_size INTEGER,
  title TEXT NOT NULL,
  uploader TEXT,
  output_template TEXT NOT NULL,
  status TEXT NOT NULL,
  progress REAL NOT NULL DEFAULT 0,
  error TEXT,
  output_path TEXT,
  output_path_exact BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_dedupe ON jobs(video_id, page_index, format_id, status);
CREATE TABLE IF NOT EXISTS resolve_cache (
  cache_key TEXT PRIMARY KEY,
  meta_json TEXT NOT NULL,
  fetched_at INTEGER NOT NULL
);
"#;

/// Whether `table` currently has `column`, per SQLite's table_info pragma.
fn column_exists(conn: &Connection, table: &str, column: &str) -> AppResult<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols.iter().any(|c| c == column))
}

pub struct Db {
    conn: Connection,
}

/// One home for the user-facing "row vanished" message: get_job / update_job /
/// delete_job and download.rs's missing_job_err all speak it.
pub(crate) const MISSING_JOB_MESSAGE: &str = "该任务已不存在，请刷新列表";

/// Whether a db-open failure is file corruption (recoverable by quarantine)
/// as opposed to permission/IO trouble (which a quarantine cannot help).
fn is_corruption(e: &AppError) -> bool {
    let AppError::Rusqlite(err) = e else {
        return false;
    };
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ffi::ErrorCode::NotADatabase
                || code.code == rusqlite::ffi::ErrorCode::DatabaseCorrupt
    )
}

/// How `Db::open_with_outcome` ended: opened as-is, or recovered from a
/// quarantined corrupt file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    Opened,
    Recovered,
}

impl Db {
    /// Open, classifying the outcome for the startup notice: opened as-is,
    /// or recovered from a quarantined corrupt file.
    /// Test/harness convenience: same as `open_with_outcome`, discarding the
    /// outcome. Production goes through `open_with_outcome` so a corrupt
    /// file is quarantined and the session notices. Compiled only for test
    /// builds — the only callers are the harness and in-crate tests.
    #[cfg(any(test, feature = "test-utils"))]
    #[cfg_attr(not(feature = "test-utils"), allow(dead_code))]
    pub fn open(path: &Path) -> AppResult<Self> {
        Self::open_with_outcome(path).map(|(db, _)| db)
    }

    pub fn open_with_outcome(path: &Path) -> AppResult<(Self, OpenOutcome)> {
        match Self::try_open(path) {
            Ok(db) => Ok((db, OpenOutcome::Opened)),
            Err(e) if is_corruption(&e) => {
                // Quarantine the corrupt file for forensics; a stale WAL/SHM
                // sidecar must go too, or replaying it into the fresh file
                // would corrupt it again.
                let bad = path.with_extension("bad");
                let _ = std::fs::rename(path, &bad);
                // OsString push, not display(): a non-UTF-8 path must keep
                // its bytes or the stale sidecar survives and re-poisons.
                let mut sidecar_wal = std::ffi::OsString::from(path.as_os_str());
                sidecar_wal.push("-wal");
                let mut sidecar_shm = std::ffi::OsString::from(path.as_os_str());
                sidecar_shm.push("-shm");
                let _ = std::fs::remove_file(&sidecar_wal);
                let _ = std::fs::remove_file(&sidecar_shm);
                tracing::warn!(
                    target: "core",
                    "db: 数据库文件损坏，已重置（原件保留为 {}）",
                    bad.display()
                );
                let db = Self::try_open(path)?;
                Ok((db, OpenOutcome::Recovered))
            }
            Err(e) => Err(e),
        }
    }

    fn try_open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        // WAL keeps every status write from paying the rollback journal's
        // create+fsync+delete cycle. synchronous=NORMAL trades the per-commit
        // fsync for a checkpoint-level one: a process crash loses nothing
        // (the WAL file is intact), while a power loss can drop the last
        // acknowledged commits — startup recovery reconciles row *states*,
        // but a lost delete or enqueue cannot be reconstructed. That is a
        // deliberate throughput trade-off, not a guarantee.
        //
        // Best-effort: switching the journal rewrites the database header, so
        // a read-only or full data directory (or a VFS without shared memory)
        // must not turn the setting into a startup failure — the app starts
        // either way, on SQLite's rollback journal when WAL is unavailable.
        if let Err(e) = conn.pragma_update(None, "journal_mode", "WAL") {
            tracing::warn!(target: "core", "db: 无法切换到 WAL，继续使用回滚日志: {e}");
        }
        if let Err(e) = conn.pragma_update(None, "synchronous", "NORMAL") {
            tracing::warn!(target: "core", "db: 无法设置 synchronous=NORMAL: {e}");
        }
        conn.execute_batch(SCHEMA)?;
        // Lightweight migration for older databases. Fresh DBs already carry the
        // column from SCHEMA (a blind ALTER would fail with "duplicate column"),
        // but swallowing every error would also hide genuine failures (locked or
        // corrupt DB) that break every later statement referencing audio_format.
        // Check first and propagate unexpected errors.
        if !column_exists(&conn, "jobs", "audio_format")? {
            conn.execute("ALTER TABLE jobs ADD COLUMN audio_format TEXT", [])?;
            tracing::info!(target: "core", "db: 迁移 jobs 表（新增 audio_format）");
        }
        // History metadata columns (same idempotent pattern as audio_format).
        // Declared types must mirror SCHEMA so migrated DBs stay byte-compatible
        // with fresh ones; bare ALTER columns would carry no type affinity.
        let mut migrated: Vec<&str> = Vec::new();
        for (col, kind) in [
            ("thumbnail_url", "TEXT"),
            ("duration_secs", "INTEGER"),
            ("file_size", "INTEGER"),
            ("uploader", "TEXT"),
            ("output_path_exact", "BLOB"),
        ] {
            if !column_exists(&conn, "jobs", col)? {
                conn.execute(&format!("ALTER TABLE jobs ADD COLUMN {col} {kind}"), [])?;
                migrated.push(col);
            }
        }
        if !migrated.is_empty() {
            tracing::info!(target: "core", "db: 迁移 jobs 表（新增 {}）", migrated.join("、"));
        }
        Self::purge_legacy_resolve_cache(&conn)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self::sweep_resolve_cache(&conn, now)?;
        Ok(Self { conn })
    }

    fn purge_legacy_resolve_cache(conn: &Connection) -> AppResult<()> {
        let mut stmt = conn.prepare("SELECT cache_key FROM resolve_cache")?;
        let keys: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut removed = 0;
        for key in keys {
            if resolve_cache::is_legacy_cache_key(&key) {
                conn.execute(
                    "DELETE FROM resolve_cache WHERE cache_key = ?1",
                    params![key],
                )?;
                removed += 1;
            }
        }
        if removed > 0 {
            tracing::debug!(target: "core", "resolve_cache: 清理旧版缓存 {removed} 条");
        }
        Ok(())
    }

    /// B14: resolve_cache rows used to live forever — the 7-day TTL was only
    /// a read-time freshness check, and the only deletions were the startup
    /// legacy-key purge and the read-time corrupt-row removal. Sweep expired
    /// rows and cap the table at open, when nothing else touches it. Recency
    /// is `fetched_at` (every upsert refreshes it), so the cap trims
    /// LRU-by-write. `now` is a parameter, not an internal clock read: the
    /// exact TTL boundary is only observable with a fixed clock (see
    /// `the_ttl_boundary_is_strict`).
    fn sweep_resolve_cache(conn: &Connection, now: i64) -> AppResult<()> {
        let cutoff = now.saturating_sub(resolve_cache::RESOLVE_CACHE_TTL_SECS as i64);
        let expired = conn.execute(
            "DELETE FROM resolve_cache WHERE fetched_at < ?1",
            params![cutoff],
        )?;
        if expired > 0 {
            tracing::debug!(target: "core", "resolve_cache: 清理过期缓存 {expired} 条");
        }
        let cap = resolve_cache::RESOLVE_CACHE_MAX_ROWS as i64;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM resolve_cache", [], |r| r.get(0))?;
        if count > cap {
            conn.execute(
                "DELETE FROM resolve_cache WHERE cache_key IN (
                    SELECT cache_key FROM resolve_cache
                    ORDER BY fetched_at DESC, cache_key DESC
                    LIMIT -1 OFFSET ?1
                )",
                params![cap],
            )?;
            tracing::debug!(target: "core", "resolve_cache: 超容量修剪 {} 条", count - cap);
        }
        Ok(())
    }

    pub fn insert_job(&self, job: &DownloadJob) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO jobs (
                id, url, video_id, page_index, format_id, audio_format, title, uploader,
                output_template,
                status, progress, error, output_path, thumbnail_url, duration_secs, file_size,
                created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                COALESCE(?17, datetime('now')), datetime('now')
            )",
            params![
                job.id,
                job.url,
                job.video_id,
                job.page_index,
                job.format_id,
                job.audio_format,
                job.title,
                job.uploader,
                job.output_template,
                status_to_str(&job.status),
                job.progress,
                job.error,
                job.output_path,
                job.thumbnail_url,
                job.duration_secs.map(|v| v as i64),
                job.file_size.map(|v| v as i64),
                job.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn update_job(&self, job: &DownloadJob) -> AppResult<()> {
        let updated = self.conn.execute(
            "UPDATE jobs SET
                url = ?2,
                video_id = ?3,
                page_index = ?4,
                format_id = ?5,
                title = ?6,
                uploader = ?7,
                output_template = ?8,
                status = ?9,
                progress = ?10,
                error = ?11,
                output_path = ?12,
                audio_format = ?13,
                thumbnail_url = ?14,
                duration_secs = ?15,
                file_size = ?16,
                updated_at = datetime('now')
            WHERE id = ?1",
            params![
                job.id,
                job.url,
                job.video_id,
                job.page_index,
                job.format_id,
                job.title,
                job.uploader,
                job.output_template,
                status_to_str(&job.status),
                job.progress,
                job.error,
                job.output_path,
                job.audio_format,
                job.thumbnail_url,
                job.duration_secs.map(|v| v as i64),
                job.file_size.map(|v| v as i64),
            ],
        )?;
        if updated == 0 {
            return Err(AppError::Message(MISSING_JOB_MESSAGE.into()));
        }
        Ok(())
    }

    pub fn list_jobs(&self) -> AppResult<Vec<DownloadJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                id, url, video_id, page_index, format_id, title, output_template,
                status, progress, error, output_path, audio_format,
                thumbnail_url, duration_secs, file_size, created_at, uploader
            FROM jobs
            ORDER BY created_at DESC, id DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let status_str: String = row.get(7)?;
            Ok(DownloadJob {
                id: row.get(0)?,
                url: row.get(1)?,
                video_id: row.get(2)?,
                page_index: row.get::<_, i64>(3)? as u32,
                format_id: row.get(4)?,
                title: row.get(5)?,
                output_template: row.get(6)?,
                status: status_from_str(&status_str)?,
                progress: row.get(8)?,
                error: row.get(9)?,
                output_path: row.get(10)?,
                audio_format: row.get(11)?,
                thumbnail_url: row.get(12)?,
                duration_secs: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                file_size: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                created_at: row.get(15)?,
                uploader: row.get::<_, Option<String>>(16)?.unwrap_or_default(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn get_job(&self, id: &str) -> AppResult<DownloadJob> {
        self.find_job(id)?
            .ok_or_else(|| AppError::Message(MISSING_JOB_MESSAGE.into()))
    }

    /// Like `get_job`, but a missing row is `None` instead of an error — the
    /// pause/resume paths map it to a Chinese message for concurrent deletes.
    pub fn find_job(&self, id: &str) -> AppResult<Option<DownloadJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                id, url, video_id, page_index, format_id, title, output_template,
                status, progress, error, output_path, audio_format,
                thumbnail_url, duration_secs, file_size, created_at, uploader
            FROM jobs
            WHERE id = ?1",
        )?;
        let job = stmt
            .query_row(params![id], |row| {
                let status_str: String = row.get(7)?;
                Ok(DownloadJob {
                    id: row.get(0)?,
                    url: row.get(1)?,
                    video_id: row.get(2)?,
                    page_index: row.get::<_, i64>(3)? as u32,
                    format_id: row.get(4)?,
                    title: row.get(5)?,
                    output_template: row.get(6)?,
                    status: status_from_str(&status_str)?,
                    progress: row.get(8)?,
                    error: row.get(9)?,
                    output_path: row.get(10)?,
                    audio_format: row.get(11)?,
                    thumbnail_url: row.get(12)?,
                    duration_secs: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                    file_size: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                    created_at: row.get(15)?,
                    uploader: row.get::<_, Option<String>>(16)?.unwrap_or_default(),
                })
            })
            .optional()?;
        Ok(job)
    }

    /// Prefer active (pending/running) over done when both exist.
    pub fn find_job_conflict(
        &self,
        video_id: &str,
        page_index: u32,
        format_id: &str,
        audio_format: Option<&str>,
    ) -> AppResult<JobConflict> {
        let status: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM jobs
                 WHERE video_id = ?1 AND page_index = ?2 AND format_id = ?3
                   AND audio_format IS ?4
                   AND status IN ('pending', 'running', 'done')
                 ORDER BY CASE status
                   WHEN 'running' THEN 0
                   WHEN 'pending' THEN 1
                   WHEN 'done' THEN 2
                 END
                 LIMIT 1",
                params![video_id, page_index, format_id, audio_format],
                |row| row.get(0),
            )
            .optional()?;

        Ok(match status.as_deref() {
            Some("pending") | Some("running") => JobConflict::Active,
            Some("done") => JobConflict::Done,
            _ => JobConflict::None,
        })
    }

    /// Any pending/running/paused job for this video page of the same media
    /// kind: video requests only conflict with other video jobs, audio
    /// requests only with other audio jobs. A paused job still holds the
    /// active slot until it is resumed or cancelled.
    pub fn has_active_job(
        &self,
        video_id: &str,
        page_index: u32,
        audio_format: Option<&str>,
    ) -> AppResult<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM jobs
             WHERE video_id = ?1 AND page_index = ?2 AND audio_format IS ?3
               AND status IN ('pending', 'running', 'paused')",
            params![video_id, page_index, audio_format],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Output paths from completed jobs for this video page (any format).
    pub fn find_done_output_paths(
        &self,
        video_id: &str,
        page_index: u32,
    ) -> AppResult<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT output_path FROM jobs
             WHERE video_id = ?1 AND page_index = ?2 AND status = 'done'
               AND output_path IS NOT NULL AND output_path != ''",
        )?;
        let rows = stmt.query_map(params![video_id, page_index], |row| row.get(0))?;
        let mut paths = Vec::new();
        for path in rows {
            paths.push(path?);
        }
        Ok(paths)
    }

    /// Record the exact OS-encoded bytes of a delivered path whose display
    /// string (the only form the webview can carry) cannot represent it.
    /// `None` clears a stale record when a retry lands on a UTF-8 path.
    pub fn set_output_path_exact(&self, id: &str, bytes: Option<&[u8]>) -> AppResult<()> {
        let updated = self.conn.execute(
            "UPDATE jobs SET output_path_exact = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, bytes],
        )?;
        if updated == 0 {
            return Err(AppError::Message(MISSING_JOB_MESSAGE.into()));
        }
        Ok(())
    }

    /// The exact bytes recorded for the row whose display path equals
    /// `display`, when one was recorded.
    /// The exact bytes recorded for THIS row. Delete must resolve by id, not
    /// by the melted display string: two different invalid names can melt to
    /// the same display, and a display-keyed lookup could then delete the
    /// other row's file.
    pub fn exact_output_path_for_job(&self, id: &str) -> AppResult<Option<Vec<u8>>> {
        let mut stmt = self.conn.prepare(
            "SELECT output_path_exact FROM jobs
             WHERE id = ?1 AND output_path_exact IS NOT NULL",
        )?;
        let bytes = stmt
            .query_row(params![id], |row| row.get::<_, Vec<u8>>(0))
            .optional()?;
        Ok(bytes)
    }

    pub fn exact_output_path_for(&self, display: &str) -> AppResult<Option<Vec<u8>>> {
        let mut stmt = self.conn.prepare(
            "SELECT output_path_exact FROM jobs
             WHERE output_path = ?1 AND output_path_exact IS NOT NULL
             LIMIT 1",
        )?;
        let bytes = stmt
            .query_row(params![display], |row| row.get::<_, Vec<u8>>(0))
            .optional()?;
        Ok(bytes)
    }

    pub fn delete_job(&self, id: &str) -> AppResult<()> {
        let deleted = self
            .conn
            .execute("DELETE FROM jobs WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(AppError::Message(MISSING_JOB_MESSAGE.into()));
        }
        Ok(())
    }

    /// Delete all done/failed rows and return their ids (the manager prunes
    /// those ids' stop markers in the same operation). One statement via
    /// SQLite RETURNING, so the predicate lives in a single place. Does not
    /// touch pending/running/paused rows or any files on disk.
    pub fn delete_finished_jobs(&self) -> AppResult<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("DELETE FROM jobs WHERE status IN ('done', 'failed') RETURNING id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    /// Startup reconciliation: a row left Running cannot have a live runner
    /// after a relaunch, so surface it as Paused (progress and the work dir
    /// are kept; resume picks it up). Returns how many rows changed.
    /// Pause every row whose runner died with the app: a Running row freezes
    /// mid-download, and a Pending row's queued runner died with the process
    /// too — both must land in a resumable state instead of waiting forever.
    pub fn mark_running_and_pending_as_paused(&self) -> AppResult<u64> {
        let changed = self.conn.execute(
            "UPDATE jobs SET status = 'paused', updated_at = datetime('now')
             WHERE status IN ('running', 'pending')",
            [],
        )?;
        Ok(changed as u64)
    }

    pub fn get_resolve_cache(&self, key: &str) -> AppResult<Option<(VideoMeta, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT meta_json, fetched_at FROM resolve_cache WHERE cache_key = ?1")?;
        let row = stmt
            .query_row(params![key], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .optional()?;
        match row {
            None => Ok(None),
            Some((meta_json, fetched_at)) => match serde_json::from_str(&meta_json) {
                Ok(meta) => Ok(Some((meta, fetched_at))),
                Err(_) => {
                    let _ = self.conn.execute(
                        "DELETE FROM resolve_cache WHERE cache_key = ?1",
                        params![key],
                    );
                    Ok(None)
                }
            },
        }
    }

    pub fn upsert_resolve_cache(
        &self,
        key: &str,
        meta: &VideoMeta,
        fetched_at: i64,
    ) -> AppResult<()> {
        let meta_json =
            serde_json::to_string(meta).map_err(|e| AppError::Message(e.to_string()))?;
        self.conn.execute(
            "INSERT INTO resolve_cache (cache_key, meta_json, fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(cache_key) DO UPDATE SET
               meta_json = excluded.meta_json,
               fetched_at = excluded.fetched_at",
            params![key, meta_json, fetched_at],
        )?;
        Ok(())
    }
}

fn status_to_str(status: &JobStatus) -> &'static str {
    match status {
        JobStatus::Pending => "pending",
        JobStatus::Running => "running",
        JobStatus::Done => "done",
        JobStatus::Failed => "failed",
        JobStatus::Paused => "paused",
    }
}

fn status_from_str(s: &str) -> rusqlite::Result<JobStatus> {
    match s {
        "pending" => Ok(JobStatus::Pending),
        "running" => Ok(JobStatus::Running),
        "done" => Ok(JobStatus::Done),
        "failed" => Ok(JobStatus::Failed),
        "paused" => Ok(JobStatus::Paused),
        _ => Err(rusqlite::Error::InvalidParameterName(format!(
            "invalid job status: {s}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// WAL keeps every status write from paying the rollback journal's
    /// create+fsync+delete cycle; all access is serialized behind one
    /// Mutex<Db>, so the journal choice is the only write-amplification knob.
    ///
    /// Value: protects=Db::open's journal/sync pragmas; fails_when=the
    /// connection keeps the default rollback journal; why_new=the hot path
    /// (progress ticks, batch enqueue) was unbounded write amplification;
    /// seam=none
    /// Switching the journal rewrites the database header: a read-only or
    /// full data directory must not turn the upgrade into a startup failure.
    /// The database opens, keeps SQLite's rollback journal, and the app
    /// starts.
    ///
    /// Value: protects=Db::open's best-effort journal pragma; fails_when=the
    /// pragma error is propagated and every launch dies before the window
    /// opens; why_new=#269 made WAL a startup hard requirement;
    /// seam=unix dir permissions
    #[cfg(unix)]
    #[test]
    fn a_read_only_data_dir_still_opens_with_the_rollback_journal() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("t.db");
        // Create the schema while the directory is writable, then put the
        // database back on the rollback journal so the WAL switch has work.
        drop(Db::open_with_outcome(&db_path).unwrap());
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "journal_mode", "DELETE").unwrap();
        }
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = Db::open_with_outcome(&db_path);

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let db = result
            .expect("a read-only data dir must not fail startup")
            .0;
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            mode.to_lowercase(),
            "delete",
            "falls back to the rollback journal"
        );
    }

    #[test]
    fn db_opens_in_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("t.db")).unwrap().0;
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        let sync: i64 = db
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "synchronous must be NORMAL");
    }

    use super::*;
    use crate::models::{JobConflict, JobStatus, VideoMeta};

    #[test]
    fn job_status_roundtrips_through_the_string_column() {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Done,
            JobStatus::Failed,
            JobStatus::Paused,
        ] {
            let s = status_to_str(&status);
            assert_eq!(
                status_from_str(s).unwrap(),
                status,
                "roundtrip for {status:?}"
            );
        }
    }

    fn sample_video_meta() -> VideoMeta {
        VideoMeta {
            id: "BV1xx".into(),
            title: "t".into(),
            uploader: None,
            thumbnail: None,
            duration_secs: None,
            webpage_url: "https://www.bilibili.com/video/BV1xx".into(),
            pages: vec![],
            formats: vec![],
            audio_formats: vec![],
            platform: "bilibili".into(),
        }
    }

    #[test]
    fn migrates_old_db_without_audio_format_column() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        // Simulate a pre-0.3.1 database: jobs table without audio_format.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE jobs (
                    id TEXT PRIMARY KEY,
                    url TEXT NOT NULL,
                    video_id TEXT NOT NULL,
                    page_index INTEGER NOT NULL,
                    format_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    output_template TEXT NOT NULL,
                    status TEXT NOT NULL,
                    progress REAL NOT NULL DEFAULT 0,
                    error TEXT,
                    output_path TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            )
            .unwrap();
        }

        let db = Db::open_with_outcome(&path).unwrap().0;
        assert!(column_exists(&db.conn, "jobs", "audio_format").unwrap());
        // The migrated column must be usable end to end.
        let mut job = sample_job_for_db("migrated-job");
        job.audio_format = Some("m4a".into());
        db.insert_job(&job).unwrap();
        let loaded = db
            .list_jobs()
            .unwrap()
            .into_iter()
            .find(|j| j.id == "migrated-job")
            .unwrap();
        assert_eq!(loaded.audio_format.as_deref(), Some("m4a"));
    }

    #[test]
    fn open_purges_legacy_resolve_cache_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open_with_outcome(&path).unwrap().0;
        let meta = sample_video_meta();
        // Fresh timestamps for every row: the B14 TTL sweep must not be the
        // reason the legacy rows disappear — only the legacy-key predicate
        // explains their removal, so this test keeps pinning that purge.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        db.upsert_resolve_cache("bilibili:bv1xx", &meta, now)
            .unwrap();
        db.upsert_resolve_cache("bilibili:bv1xx:guest", &meta, now)
            .unwrap();
        db.upsert_resolve_cache("bilibili:bv1xx:guest_g2", &meta, now)
            .unwrap();
        db.upsert_resolve_cache("bilibili:bv1xx:guest_g3", &meta, now)
            .unwrap();
        drop(db);

        let db2 = Db::open_with_outcome(&path).unwrap().0;
        assert!(db2.get_resolve_cache("bilibili:bv1xx").unwrap().is_none());
        assert!(
            db2.get_resolve_cache("bilibili:bv1xx:guest")
                .unwrap()
                .is_none()
        );
        assert!(
            db2.get_resolve_cache("bilibili:bv1xx:guest_g2")
                .unwrap()
                .is_none()
        );
        assert!(
            db2.get_resolve_cache("bilibili:bv1xx:guest_g3")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn resolve_cache_roundtrip_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let meta = sample_video_meta();
        db.upsert_resolve_cache("bilibili:bv1xx:guest_g3", &meta, 100)
            .unwrap();
        let (got, at) = db
            .get_resolve_cache("bilibili:bv1xx:guest_g3")
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "BV1xx");
        assert_eq!(at, 100);
        let mut meta2 = meta.clone();
        meta2.title = "t2".into();
        db.upsert_resolve_cache("bilibili:bv1xx:guest_g3", &meta2, 200)
            .unwrap();
        let (got2, at2) = db
            .get_resolve_cache("bilibili:bv1xx:guest_g3")
            .unwrap()
            .unwrap();
        assert_eq!(got2.title, "t2");
        assert_eq!(at2, 200);
    }

    #[test]
    fn resolve_cache_corrupt_json_treated_as_miss() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        db.conn
            .execute(
                "INSERT INTO resolve_cache (cache_key, meta_json, fetched_at) VALUES (?1, ?2, ?3)",
                params!["bilibili:bv1xx:guest_g3", "{not json", 100_i64],
            )
            .unwrap();
        assert!(
            db.get_resolve_cache("bilibili:bv1xx:guest_g3")
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_resolve_cache("bilibili:bv1xx:guest")
                .unwrap()
                .is_none()
        );
    }

    /// B14: resolve_cache rows used to live forever — the TTL was only a
    /// read-time freshness check, so nothing ever deleted an expired row.
    ///
    /// Value: protects=try_open's expired-row sweep; fails_when=a row older
    /// than RESOLVE_CACHE_TTL_SECS survives a reopen (fresh rows must stay);
    /// why_new=the table only ever grew across launches (B14);
    /// seam=none
    #[test]
    fn reopening_sweeps_expired_resolve_cache_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        // Anchor on the real clock: the sweep's cutoff is computed from
        // SystemTime at open, so a hardcoded timestamp would drift in or out
        // of the TTL window as wall-clock time passes. Rows sit well inside
        // their side of the boundary (±60s): the reopen sweep re-reads the
        // clock, so an exact-boundary row cannot be observed reliably here —
        // a second ticking over between the two opens moves the cutoff past
        // it (this exact flake bit the Linux CI runner).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let meta = sample_video_meta();
        {
            let db = Db::open_with_outcome(&path).unwrap().0;
            db.upsert_resolve_cache(
                "bilibili:old:guest_g3",
                &meta,
                now - resolve_cache::RESOLVE_CACHE_TTL_SECS as i64 - 60,
            )
            .unwrap();
            db.upsert_resolve_cache("bilibili:fresh:guest_g3", &meta, now)
                .unwrap();
        }
        let db = Db::open_with_outcome(&path).unwrap().0;
        assert!(
            db.get_resolve_cache("bilibili:old:guest_g3")
                .unwrap()
                .is_none(),
            "an expired row must be swept at open"
        );
        assert!(
            db.get_resolve_cache("bilibili:fresh:guest_g3")
                .unwrap()
                .is_some(),
            "a fresh row must survive the sweep"
        );
    }

    /// B14: the TTL predicate is strict (`<`): a row exactly at the cutoff
    /// stays, and at that same fixed clock `is_fresh`'s `<=` judges it fresh
    /// — the sweep's delete boundary sits one second beyond the read's fresh
    /// boundary, so nothing survives the sweep while reading stale. Pinned
    /// through the sweep's `now` seam with a fixed clock: a
    /// reopen-based test cannot observe the exact boundary because the sweep
    /// recomputes the clock at open time.
    ///
    /// Value: protects=the strict `<` in sweep_resolve_cache; fails_when=the
    /// predicate becomes `<=` and the cutoff row is deleted;
    /// why_new=fable review asked for the boundary; the first version pinned
    /// it across a reopen and flaked on Linux when the wall clock ticked;
    /// seam=sweep_resolve_cache's now parameter
    #[test]
    fn the_ttl_boundary_is_strict() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let now = 1_800_000_000_i64;
        let ttl = resolve_cache::RESOLVE_CACHE_TTL_SECS as i64;
        let meta = sample_video_meta();
        db.upsert_resolve_cache("bilibili:edge:guest_g3", &meta, now - ttl)
            .unwrap();
        db.upsert_resolve_cache("bilibili:old:guest_g3", &meta, now - ttl - 1)
            .unwrap();
        db.upsert_resolve_cache("bilibili:fresh:guest_g3", &meta, now)
            .unwrap();

        Db::sweep_resolve_cache(&db.conn, now).unwrap();

        assert!(
            db.get_resolve_cache("bilibili:edge:guest_g3")
                .unwrap()
                .is_some(),
            "a row exactly at the cutoff must survive the strict < sweep"
        );
        assert!(
            db.get_resolve_cache("bilibili:old:guest_g3")
                .unwrap()
                .is_none(),
            "a row one second past the cutoff must be swept"
        );
        assert!(
            db.get_resolve_cache("bilibili:fresh:guest_g3")
                .unwrap()
                .is_some(),
            "a fresh row must survive the sweep"
        );
    }

    /// B14: the table is capped at RESOLVE_CACHE_MAX_ROWS at open — beyond
    /// the TTL, an unbounded table was itself the risk (one serialized
    /// VideoMeta per row, two rows per resolve).
    ///
    /// Value: protects=the startup cap trim; fails_when=more than
    /// RESOLVE_CACHE_MAX_ROWS rows survive a reopen, or the trim keeps
    /// stale rows over fresh ones (recency is fetched_at, LRU-by-write);
    /// why_new=the table only ever grew across launches (B14);
    /// seam=none
    #[test]
    fn reopening_trims_resolve_cache_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        let cap = resolve_cache::RESOLVE_CACHE_MAX_ROWS as i64;
        let meta = sample_video_meta();
        // Anchor on the real clock and keep every row in the past, well
        // inside the TTL window: the trim must be the only thing deleting
        // rows here — the TTL sweep must not eat into the count and mask a
        // broken cap.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        {
            let db = Db::open_with_outcome(&path).unwrap().0;
            for i in 0..cap + 5 {
                db.upsert_resolve_cache(
                    &format!("bilibili:v{i}:guest_g3"),
                    &meta,
                    now - 60 - (cap + 5) + i,
                )
                .unwrap();
            }
        }
        let db = Db::open_with_outcome(&path).unwrap().0;
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM resolve_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, cap, "the cap must hold after the startup trim");
        assert!(
            db.get_resolve_cache("bilibili:v0:guest_g3")
                .unwrap()
                .is_none(),
            "the oldest rows must go first"
        );
        assert!(
            db.get_resolve_cache(&format!("bilibili:v{}:guest_g3", cap + 4))
                .unwrap()
                .is_some(),
            "the newest rows must survive the trim"
        );
        // ==cap: the trim must be a no-op at exactly the cap — an off-by-one
        // in the comparison or the OFFSET would eat a row here.
        let dir2 = tempfile::tempdir().unwrap();
        let path2 = dir2.path().join("jobs.db");
        {
            let db = Db::open_with_outcome(&path2).unwrap().0;
            for i in 0..cap {
                db.upsert_resolve_cache(
                    &format!("bilibili:e{i}:guest_g3"),
                    &meta,
                    now - 60 - (cap + 5) + i,
                )
                .unwrap();
            }
        }
        let db = Db::open_with_outcome(&path2).unwrap().0;
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM resolve_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, cap, "exactly-cap tables must not be trimmed");
        assert!(
            db.get_resolve_cache("bilibili:e0:guest_g3")
                .unwrap()
                .is_some(),
            "no row may go at exactly the cap"
        );
    }

    fn sample_job(format_id: &str) -> DownloadJob {
        DownloadJob {
            id: "job-1".into(),
            url: "https://www.bilibili.com/video/BV1xx".into(),
            video_id: "BV1xx".into(),
            page_index: 1,
            format_id: format_id.into(),
            audio_format: None,
            title: "demo".into(),
            uploader: String::new(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            status: JobStatus::Done,
            progress: 1.0,
            error: None,
            output_path: Some("virtual/demo.mp4".into()),
            thumbnail_url: None,
            duration_secs: None,
            file_size: None,
            created_at: None,
        }
    }

    fn sample_job_for_db(id: &str) -> DownloadJob {
        DownloadJob {
            id: id.into(),
            url: "https://www.bilibili.com/video/BV1xx".into(),
            video_id: "BV1xx".into(),
            page_index: 1,
            format_id: "80".into(),
            audio_format: None,
            title: "demo".into(),
            uploader: String::new(),
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

    #[test]
    fn find_job_conflict_detects_done_job() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        db.insert_job(&sample_job("80")).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "80", None).unwrap(),
            JobConflict::Done
        );
    }

    #[test]
    fn find_job_conflict_distinguishes_audio_containers() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let mut m4a = sample_job("bestaudio");
        m4a.audio_format = Some("m4a".into());
        db.insert_job(&m4a).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "bestaudio", Some("m4a"))
                .unwrap(),
            JobConflict::Done
        );
        // Same tier, different container: not the same file.
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "bestaudio", Some("mp3"))
                .unwrap(),
            JobConflict::None
        );
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "bestaudio", None).unwrap(),
            JobConflict::None
        );
    }

    #[test]
    fn has_active_job_distinguishes_video_and_audio() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;

        let mut audio = sample_job_for_db("audio-1");
        audio.audio_format = Some("m4a".into());
        db.insert_job(&audio).unwrap();

        // A pending audio job blocks another audio request, but not a video one.
        assert!(db.has_active_job("BV1xx", 1, Some("m4a")).unwrap());
        assert!(!db.has_active_job("BV1xx", 1, None).unwrap());

        let mut video = sample_job_for_db("video-1");
        video.audio_format = None;
        db.insert_job(&video).unwrap();

        assert!(db.has_active_job("BV1xx", 1, None).unwrap());
        assert!(db.has_active_job("BV1xx", 1, Some("m4a")).unwrap());
    }

    #[test]
    fn find_job_conflict_none_for_different_format() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        db.insert_job(&sample_job("80")).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "64", None).unwrap(),
            JobConflict::None
        );
    }

    #[test]
    fn find_job_conflict_prefers_active_over_done() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        db.insert_job(&sample_job("80")).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "80", None).unwrap(),
            JobConflict::Done
        );

        let mut running = sample_job("80");
        running.id = "job-2".into();
        running.status = JobStatus::Running;
        running.progress = 0.3;
        running.output_path = None;
        db.insert_job(&running).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "80", None).unwrap(),
            JobConflict::Active
        );
    }

    #[test]
    fn insert_update_and_list_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let mut job = sample_job("80");
        job.status = JobStatus::Pending;
        job.progress = 0.0;
        db.insert_job(&job).unwrap();

        job.status = JobStatus::Running;
        job.progress = 0.5;
        db.update_job(&job).unwrap();

        let jobs = db.list_jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, JobStatus::Running);
        assert!((jobs[0].progress - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn delete_finished_jobs_removes_done_and_failed_keeps_pending_running_paused() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;

        let mut pending = sample_job_for_db("p1");
        pending.status = JobStatus::Pending;
        db.insert_job(&pending).unwrap();

        let mut running = sample_job_for_db("r1");
        running.status = JobStatus::Running;
        db.insert_job(&running).unwrap();

        // A paused row's work dir holds the partial files resume needs; clearing
        // history must never delete it (the paused status is a new row state).
        let mut paused = sample_job_for_db("pa1");
        paused.status = JobStatus::Paused;
        db.insert_job(&paused).unwrap();

        let mut done = sample_job_for_db("d1");
        done.status = JobStatus::Done;
        done.output_path = Some("/tmp/keep-me.mp4".into());
        db.insert_job(&done).unwrap();

        let mut failed = sample_job_for_db("f1");
        failed.status = JobStatus::Failed;
        failed.error = Some("用户取消下载".into());
        db.insert_job(&failed).unwrap();

        let mut cleared = db.delete_finished_jobs().unwrap();
        cleared.sort();
        assert_eq!(cleared, vec!["d1".to_string(), "f1".to_string()]);

        let jobs = db.list_jobs().unwrap();
        assert_eq!(jobs.len(), 3);
        assert!(jobs.iter().any(|j| j.id == "p1"));
        assert!(jobs.iter().any(|j| j.id == "r1"));
        assert!(jobs.iter().any(|j| j.id == "pa1"));
        assert!(!jobs.iter().any(|j| j.id == "d1" || j.id == "f1"));
    }

    #[test]
    fn job_metadata_columns_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let mut job = sample_job_for_db("meta-job");
        job.thumbnail_url = Some("https://i0.hdslb.com/bfs/archive/cover.jpg".into());
        job.duration_secs = Some(269);
        job.file_size = Some(299_892_736);
        db.insert_job(&job).unwrap();

        let listed = db.list_jobs().unwrap();
        let got = listed.iter().find(|j| j.id == "meta-job").unwrap();
        assert_eq!(
            got.thumbnail_url.as_deref(),
            Some("https://i0.hdslb.com/bfs/archive/cover.jpg")
        );
        assert_eq!(got.duration_secs, Some(269));
        assert_eq!(got.file_size, Some(299_892_736));
        // created_at is set by SQLite on insert and must be exposed.
        assert!(got.created_at.as_deref().unwrap_or("").len() >= 19);
    }

    #[test]
    fn legacy_db_without_metadata_columns_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("jobs.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE jobs (
                    id TEXT PRIMARY KEY, url TEXT NOT NULL, video_id TEXT NOT NULL,
                    page_index INTEGER NOT NULL, format_id TEXT NOT NULL,
                    audio_format TEXT, title TEXT NOT NULL, output_template TEXT NOT NULL,
                    status TEXT NOT NULL, progress REAL NOT NULL DEFAULT 0,
                    error TEXT, output_path TEXT,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                );",
            )
            .unwrap();
        }
        let db = Db::open_with_outcome(&db_path).unwrap().0; // migration runs here
        let mut job = sample_job_for_db("legacy-meta-job");
        job.thumbnail_url = None;
        db.insert_job(&job).unwrap();
        assert!(db.get_job("legacy-meta-job").is_ok());
    }

    fn column_decl_type(conn: &Connection, column: &str) -> Option<String> {
        let mut stmt = conn.prepare("PRAGMA table_info(jobs)").unwrap();
        let rows: Vec<(String, Option<String>)> = stmt
            .query_map([], |row| Ok((row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.into_iter()
            .find(|(name, _)| name == column)
            .and_then(|(_, decl)| decl)
    }

    #[test]
    fn migrated_metadata_columns_carry_declared_types() {
        // A migrated DB must expose the same declared types as a fresh SCHEMA
        // install; bare ALTER TABLE ADD COLUMN would leave them untyped.
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("jobs.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE jobs (
                    id TEXT PRIMARY KEY, url TEXT NOT NULL, video_id TEXT NOT NULL,
                    page_index INTEGER NOT NULL, format_id TEXT NOT NULL,
                    audio_format TEXT, title TEXT NOT NULL, output_template TEXT NOT NULL,
                    status TEXT NOT NULL, progress REAL NOT NULL DEFAULT 0,
                    error TEXT, output_path TEXT,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
                );",
            )
            .unwrap();
        }
        let db = Db::open_with_outcome(&db_path).unwrap().0;

        for (col, kind) in [
            ("thumbnail_url", "TEXT"),
            ("duration_secs", "INTEGER"),
            ("file_size", "INTEGER"),
            ("uploader", "TEXT"),
            ("output_path_exact", "BLOB"),
        ] {
            assert_eq!(
                column_decl_type(&db.conn, col).as_deref(),
                Some(kind),
                "migrated column {col} must declare {kind}"
            );
        }
    }

    /// The exact path record must store the raw OS bytes and find them again
    /// by the display string the webview can carry; clearing it must not
    /// leave a stale path behind for a retry that landed on a UTF-8 name.
    #[test]
    fn exact_output_path_roundtrips_raw_bytes_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let mut job = sample_job_for_db("exact-1");
        let raw = b"/save/bad-\xff-name.mp4";
        let display = String::from_utf8_lossy(raw).into_owned();
        job.output_path = Some(display.clone());
        db.insert_job(&job).unwrap();

        db.set_output_path_exact("exact-1", Some(raw)).unwrap();
        assert_eq!(
            db.exact_output_path_for(&display).unwrap().as_deref(),
            Some(&raw[..])
        );

        db.set_output_path_exact("exact-1", None).unwrap();
        assert_eq!(db.exact_output_path_for(&display).unwrap(), None);
    }

    #[test]
    fn exact_output_path_write_reports_a_vanished_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        assert!(db.set_output_path_exact("missing", Some(b"x")).is_err());
    }

    #[test]
    fn job_roundtrips_uploader_through_the_db() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let mut job = sample_job_for_db("up-1");
        job.uploader = "某UP主".into();
        db.insert_job(&job).unwrap();
        assert_eq!(db.find_job("up-1").unwrap().unwrap().uploader, "某UP主");
        assert_eq!(db.list_jobs().unwrap()[0].uploader, "某UP主");
        // Updates must not wipe the stored uploader.
        let mut loaded = db.get_job("up-1").unwrap();
        loaded.progress = 0.5;
        db.update_job(&loaded).unwrap();
        assert_eq!(db.get_job("up-1").unwrap().uploader, "某UP主");
    }

    #[test]
    fn same_second_rows_order_by_id_desc() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        // Same-second created_at with insertion order opposite to id order:
        // without a tiebreaker SQLite walks insertion (rowid) order and the
        // later insert sorts first, which is arbitrary once rows are
        // restored or updated. id DESC is the deterministic answer. Real
        // ids are job-{19-digit nanos}-{seq}, so same-second ids share the
        // nanos prefix and lexicographic DESC equals creation order.
        for id in ["job-1764000000000000000-1", "job-1764000000000000000-2"] {
            let mut job = sample_job_for_db(id);
            job.created_at = Some("2026-01-01 00:00:00".into());
            db.insert_job(&job).unwrap();
        }
        let ids: Vec<String> = db
            .list_jobs()
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect();
        assert_eq!(
            ids,
            vec!["job-1764000000000000000-2", "job-1764000000000000000-1",]
        );
    }

    #[test]
    fn a_corrupt_database_is_quarantined_and_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        std::fs::write(&path, b"definitely not a sqlite database header at all").unwrap();
        // The stale WAL sidecar must be removed with the quarantine, or it
        // would poison the fresh file.
        std::fs::write(
            PathBuf::from(format!("{}-wal", path.display())),
            b"stale wal bytes",
        )
        .unwrap();

        let (db, outcome) = Db::open_with_outcome(&path).unwrap();
        assert_eq!(outcome, OpenOutcome::Recovered);
        // with_extension("bad") replaces ".db": the quarantine file is
        // jobs.bad. The stale WAL sidecar is removed before the fresh open
        // — not directly observable (the fresh connection recreates a
        // -wal), but a leftover stale WAL would be replayed into the fresh
        // file and corrupt it, failing the insert below.
        let quarantined = path.with_extension("bad");
        assert!(quarantined.exists(), "the corrupt file must be quarantined");
        // The fresh database actually works.
        let mut job = sample_job_for_db("fresh-1");
        job.created_at = None;
        db.insert_job(&job).unwrap();
        assert_eq!(db.get_job("fresh-1").unwrap().id, "fresh-1");
    }

    #[cfg(unix)]
    #[test]
    fn a_permission_error_is_not_treated_as_corruption() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        std::fs::write(&path, b"real sqlite would live here").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let Err(err) = Db::open_with_outcome(&path) else {
            panic!("a permission failure must not open");
        };
        assert!(
            !is_corruption(&err),
            "a permission failure must surface loudly, not reset the database"
        );
        assert!(path.exists());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn insert_preserves_an_explicit_created_at() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        let mut job = sample_job_for_db("ts-1");
        job.created_at = Some("2026-01-02 03:04:05".into());
        db.insert_job(&job).unwrap();
        assert_eq!(
            db.get_job("ts-1").unwrap().created_at.as_deref(),
            Some("2026-01-02 03:04:05"),
            "a restored row keeps its original timestamp; the graveyard \
             promises timestamps survive the undo"
        );
    }

    #[test]
    fn legacy_null_uploader_reads_back_as_empty_string() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with_outcome(&dir.path().join("jobs.db"))
            .unwrap()
            .0;
        // Rows written before the uploader column existed carry NULL in it
        // (a fresh DB builds the column via CREATE TABLE; only upgrades run
        // the ALTER, both leave the cell nullable). Insert via raw SQL
        // (bypassing insert_job, which binds a String and writes '') to
        // reproduce that state. The sibling optional columns carry
        // distinctive NON-null values so a mapper index swap between them and
        // `uploader` — exactly what NULL-masking would hide — fails loudly.
        db.conn
            .execute(
                "INSERT INTO jobs (
                    id, url, video_id, page_index, format_id, title, uploader,
                    thumbnail_url, audio_format,
                    output_template, status, progress,
                    created_at, updated_at
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, NULL,
                    ?7, ?8,
                    ?9, ?10, ?11,
                    datetime('now'), datetime('now')
                )",
                params![
                    "legacy-null-up",
                    "https://www.bilibili.com/video/BV1xx",
                    "BV1xx",
                    1_i64,
                    "80",
                    "demo",
                    "https://i0.hdslb.com/thumb.jpg",
                    "30280",
                    "%(title)s [%(id)s].%(ext)s",
                    "pending",
                    0.0_f64,
                ],
            )
            .unwrap();

        // Public read paths must surface the NULL uploader as "" and leave
        // every other column intact.
        let found = db.find_job("legacy-null-up").unwrap().unwrap();
        assert_eq!(found.uploader, "");
        assert_eq!(found.url, "https://www.bilibili.com/video/BV1xx");
        assert_eq!(found.video_id, "BV1xx");
        assert_eq!(found.page_index, 1);
        assert_eq!(found.format_id, "80");
        assert_eq!(found.title, "demo");
        assert_eq!(found.output_template, "%(title)s [%(id)s].%(ext)s");
        assert_eq!(found.status, JobStatus::Pending);
        assert!((found.progress - 0.0).abs() < f64::EPSILON);

        let listed = db.list_jobs().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "legacy-null-up");
        assert_eq!(listed[0].uploader, "");
        assert_eq!(
            listed[0].thumbnail_url.as_deref(),
            Some("https://i0.hdslb.com/thumb.jpg"),
            "list_jobs' mapper must not swap uploader with its neighbour column"
        );
        assert_eq!(listed[0].audio_format.as_deref(), Some("30280"));

        assert_eq!(
            found.thumbnail_url.as_deref(),
            Some("https://i0.hdslb.com/thumb.jpg")
        );
        assert_eq!(found.audio_format.as_deref(), Some("30280"));
    }
}
