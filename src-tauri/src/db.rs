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
  title TEXT NOT NULL,
  output_template TEXT NOT NULL,
  status TEXT NOT NULL,
  progress REAL NOT NULL DEFAULT 0,
  error TEXT,
  output_path TEXT,
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

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> AppResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Self::purge_legacy_resolve_cache(&conn)?;
        Ok(Self { conn })
    }

    fn purge_legacy_resolve_cache(conn: &Connection) -> AppResult<()> {
        let mut stmt = conn.prepare("SELECT cache_key FROM resolve_cache")?;
        let keys: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for key in keys {
            if resolve_cache::is_legacy_cache_key(&key) {
                conn.execute(
                    "DELETE FROM resolve_cache WHERE cache_key = ?1",
                    params![key],
                )?;
            }
        }
        Ok(())
    }

    pub fn insert_job(&self, job: &DownloadJob) -> AppResult<()> {
        self.conn.execute(
            "INSERT INTO jobs (
                id, url, video_id, page_index, format_id, title, output_template,
                status, progress, error, output_path, created_at, updated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, datetime('now'), datetime('now')
            )",
            params![
                job.id,
                job.url,
                job.video_id,
                job.page_index,
                job.format_id,
                job.title,
                job.output_template,
                status_to_str(&job.status),
                job.progress,
                job.error,
                job.output_path,
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
                output_template = ?7,
                status = ?8,
                progress = ?9,
                error = ?10,
                output_path = ?11,
                updated_at = datetime('now')
            WHERE id = ?1",
            params![
                job.id,
                job.url,
                job.video_id,
                job.page_index,
                job.format_id,
                job.title,
                job.output_template,
                status_to_str(&job.status),
                job.progress,
                job.error,
                job.output_path,
            ],
        )?;
        if updated == 0 {
            return Err(AppError::Message(format!("job not found: {}", job.id)));
        }
        Ok(())
    }

    pub fn list_jobs(&self) -> AppResult<Vec<DownloadJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                id, url, video_id, page_index, format_id, title, output_template,
                status, progress, error, output_path
            FROM jobs
            ORDER BY created_at DESC",
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
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    pub fn get_job(&self, id: &str) -> AppResult<DownloadJob> {
        let mut stmt = self.conn.prepare(
            "SELECT
                id, url, video_id, page_index, format_id, title, output_template,
                status, progress, error, output_path
            FROM jobs
            WHERE id = ?1",
        )?;
        let job = stmt.query_row(params![id], |row| {
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
            })
        })?;
        Ok(job)
    }

    /// Prefer active (pending/running) over done when both exist.
    pub fn find_job_conflict(
        &self,
        video_id: &str,
        page_index: u32,
        format_id: &str,
    ) -> AppResult<JobConflict> {
        let status: Option<String> = self
            .conn
            .query_row(
                "SELECT status FROM jobs
                 WHERE video_id = ?1 AND page_index = ?2 AND format_id = ?3
                   AND status IN ('pending', 'running', 'done')
                 ORDER BY CASE status
                   WHEN 'running' THEN 0
                   WHEN 'pending' THEN 1
                   WHEN 'done' THEN 2
                 END
                 LIMIT 1",
                params![video_id, page_index, format_id],
                |row| row.get(0),
            )
            .optional()?;

        Ok(match status.as_deref() {
            Some("pending") | Some("running") => JobConflict::Active,
            Some("done") => JobConflict::Done,
            _ => JobConflict::None,
        })
    }

    /// Any pending/running job for this video page, regardless of format.
    pub fn has_active_job(&self, video_id: &str, page_index: u32) -> AppResult<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM jobs
             WHERE video_id = ?1 AND page_index = ?2
               AND status IN ('pending', 'running')",
            params![video_id, page_index],
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

    pub fn delete_job(&self, id: &str) -> AppResult<()> {
        let deleted = self
            .conn
            .execute("DELETE FROM jobs WHERE id = ?1", params![id])?;
        if deleted == 0 {
            return Err(AppError::Message(format!("job not found: {id}")));
        }
        Ok(())
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
    }
}

fn status_from_str(s: &str) -> rusqlite::Result<JobStatus> {
    match s {
        "pending" => Ok(JobStatus::Pending),
        "running" => Ok(JobStatus::Running),
        "done" => Ok(JobStatus::Done),
        "failed" => Ok(JobStatus::Failed),
        _ => Err(rusqlite::Error::InvalidParameterName(format!(
            "invalid job status: {s}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{JobConflict, JobStatus, VideoMeta};

    fn sample_video_meta() -> VideoMeta {
        VideoMeta {
            id: "BV1xx".into(),
            title: "t".into(),
            uploader: None,
            thumbnail: None,
            webpage_url: "https://www.bilibili.com/video/BV1xx".into(),
            pages: vec![],
            formats: vec![],
            platform: "bilibili".into(),
        }
    }

    #[test]
    fn open_purges_legacy_resolve_cache_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open(&path).unwrap();
        let meta = sample_video_meta();
        db.upsert_resolve_cache("bilibili:bv1xx", &meta, 100)
            .unwrap();
        db.upsert_resolve_cache("bilibili:bv1xx:guest", &meta, 100)
            .unwrap();
        drop(db);

        let db2 = Db::open(&path).unwrap();
        assert!(db2.get_resolve_cache("bilibili:bv1xx").unwrap().is_none());
        assert!(
            db2.get_resolve_cache("bilibili:bv1xx:guest")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn resolve_cache_roundtrip_and_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        let meta = sample_video_meta();
        db.upsert_resolve_cache("bilibili:bv1xx:guest", &meta, 100)
            .unwrap();
        let (got, at) = db
            .get_resolve_cache("bilibili:bv1xx:guest")
            .unwrap()
            .unwrap();
        assert_eq!(got.id, "BV1xx");
        assert_eq!(at, 100);
        let mut meta2 = meta.clone();
        meta2.title = "t2".into();
        db.upsert_resolve_cache("bilibili:bv1xx:guest", &meta2, 200)
            .unwrap();
        let (got2, at2) = db
            .get_resolve_cache("bilibili:bv1xx:guest")
            .unwrap()
            .unwrap();
        assert_eq!(got2.title, "t2");
        assert_eq!(at2, 200);
    }

    #[test]
    fn resolve_cache_corrupt_json_treated_as_miss() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        db.conn
            .execute(
                "INSERT INTO resolve_cache (cache_key, meta_json, fetched_at) VALUES (?1, ?2, ?3)",
                params!["bilibili:bv1xx:guest", "{not json", 100_i64],
            )
            .unwrap();
        assert!(
            db.get_resolve_cache("bilibili:bv1xx:guest")
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_resolve_cache("bilibili:bv1xx:guest")
                .unwrap()
                .is_none()
        );
    }

    fn sample_job(format_id: &str) -> DownloadJob {
        DownloadJob {
            id: "job-1".into(),
            url: "https://www.bilibili.com/video/BV1xx".into(),
            video_id: "BV1xx".into(),
            page_index: 1,
            format_id: format_id.into(),
            title: "demo".into(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            status: JobStatus::Done,
            progress: 1.0,
            error: None,
            output_path: Some("/tmp/demo.mp4".into()),
        }
    }

    #[test]
    fn find_job_conflict_detects_done_job() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        db.insert_job(&sample_job("80")).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "80").unwrap(),
            JobConflict::Done
        );
    }

    #[test]
    fn find_job_conflict_none_for_different_format() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        db.insert_job(&sample_job("80")).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "64").unwrap(),
            JobConflict::None
        );
    }

    #[test]
    fn find_job_conflict_prefers_active_over_done() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
        db.insert_job(&sample_job("80")).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "80").unwrap(),
            JobConflict::Done
        );

        let mut running = sample_job("80");
        running.id = "job-2".into();
        running.status = JobStatus::Running;
        running.progress = 0.3;
        running.output_path = None;
        db.insert_job(&running).unwrap();
        assert_eq!(
            db.find_job_conflict("BV1xx", 1, "80").unwrap(),
            JobConflict::Active
        );
    }

    #[test]
    fn insert_update_and_list_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("jobs.db")).unwrap();
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
}
