use crate::error::{AppError, AppResult};
use chrono::NaiveDate;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const LOG_PREFIX: &str = "app.";
pub const DEFAULT_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
pub const TAIL_MAX_LINES: usize = 1000;
pub const TAIL_MAX_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct LogFileInfo {
    pub name: String,
    pub size: u64,
    pub modified_secs: i64,
}

fn file_name(date: NaiveDate, seq: u32) -> String {
    if seq == 0 {
        format!("{LOG_PREFIX}{date}.log")
    } else {
        format!("{LOG_PREFIX}{date}.{seq}.log")
    }
}

fn parse_file_name(name: &str) -> Option<(NaiveDate, u32)> {
    let rest = name.strip_prefix(LOG_PREFIX)?.strip_suffix(".log")?;
    let (date_str, seq_str) = match rest.rsplit_once('.') {
        Some((date, seq)) if seq.bytes().all(|b| b.is_ascii_digit()) => (date, seq),
        _ => (rest, "0"),
    };
    let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()?;
    let seq = seq_str.parse::<u32>().ok()?;
    Some((date, seq))
}

fn open_append(path: &Path) -> AppResult<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| AppError::Message(format!("打开日志文件失败: {e}")))?;
    crate::fsutil::restrict_private_file_perms(path);
    Ok(file)
}

fn scan_max_seq(dir: &Path, date: NaiveDate) -> AppResult<u32> {
    let mut max_seq: Option<u32> = None;
    for entry in fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))? {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((d, seq)) = parse_file_name(&name)
            && d == date
        {
            max_seq = Some(max_seq.map_or(seq, |m| m.max(seq)));
        }
    }
    Ok(max_seq.unwrap_or(0))
}

pub(crate) struct Rotator {
    dir: PathBuf,
    date: NaiveDate,
    seq: u32,
    max_size: u64,
    bytes: u64,
    file: File,
}

impl Rotator {
    pub(crate) fn open(dir: PathBuf, date: NaiveDate, max_size: u64) -> AppResult<Self> {
        let seq = scan_max_seq(&dir, date)?;
        let path = dir.join(file_name(date, seq));
        let file = open_append(&path)?;
        let bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            dir,
            date,
            seq,
            max_size,
            bytes,
            file,
        })
    }

    fn rotate(&mut self) -> AppResult<()> {
        let seq = scan_max_seq(&self.dir, self.date)?.max(self.seq) + 1;
        let path = self.dir.join(file_name(self.date, seq));
        self.file = open_append(&path)?;
        self.seq = seq;
        self.bytes = 0;
        Ok(())
    }

    fn write_chunk(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.max_size > 0 && self.bytes + buf.len() as u64 > self.max_size {
            self.rotate().map_err(std::io::Error::other)?;
        }
        self.file.write_all(buf)?;
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }
}

pub fn cleanup_old_logs(dir: &Path, retention_days: u32, today: NaiveDate) -> AppResult<usize> {
    let cutoff = today - chrono::Duration::days(retention_days as i64);
    let mut removed = 0;
    for entry in fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))? {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((date, _)) = parse_file_name(&name)
            && date < cutoff
        {
            fs::remove_file(entry.path()).map_err(|e| AppError::Message(format!("删除日志失败: {e}")))?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn clear_log_history(dir: &Path, today: NaiveDate) -> AppResult<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))? {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((date, _)) = parse_file_name(&name)
            && date != today
        {
            fs::remove_file(entry.path()).map_err(|e| AppError::Message(format!("删除日志失败: {e}")))?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn list_log_files(dir: &Path) -> AppResult<Vec<LogFileInfo>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))? {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_file_name(&name).is_none() {
            continue;
        }
        let meta = entry.metadata().map_err(|e| AppError::Message(format!("读取日志元数据失败: {e}")))?;
        let modified_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        files.push(LogFileInfo {
            name,
            size: meta.len(),
            modified_secs,
        });
    }
    files.sort_by(|a, b| b.name.cmp(&a.name));
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn parses_base_and_numbered_file_names() {
        let d = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        assert_eq!(parse_file_name("app.2026-08-15.log"), Some((d, 0)));
        assert_eq!(parse_file_name("app.2026-08-15.3.log"), Some((d, 3)));
        assert_eq!(parse_file_name("app.2026-08-15.x.log"), None);
        assert_eq!(parse_file_name("other.log"), None);
    }

    #[test]
    fn rotator_opens_next_numbered_file_without_rename() {
        let t = dir();
        let d = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        let mut rotator = Rotator::open(t.path().to_path_buf(), d, 10).unwrap();
        rotator.write_chunk(b"123456").unwrap();
        assert_eq!(rotator.seq, 0);
        rotator.write_chunk(b"67890").unwrap(); // 6 + 5 > 10 -> rotate
        assert_eq!(rotator.seq, 1);
        assert!(t.path().join("app.2026-08-15.log").exists());
        assert!(t.path().join("app.2026-08-15.1.log").exists());
    }

    #[test]
    fn rotator_resumes_from_highest_existing_sequence() {
        let t = dir();
        let d = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        std::fs::write(t.path().join("app.2026-08-15.3.log"), b"old").unwrap();
        let rotator = Rotator::open(t.path().to_path_buf(), d, 10_000).unwrap();
        assert_eq!(rotator.seq, 3);
        assert_eq!(rotator.bytes, 3);
    }

    #[test]
    fn cleanup_removes_files_older_than_retention_by_name() {
        let t = dir();
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        for name in [
            "app.2026-08-15.log",
            "app.2026-08-14.log",
            "app.2026-06-01.2.log",
            "app.2025-01-01.log",
            "not-a-log.txt",
        ] {
            std::fs::write(t.path().join(name), b"x").unwrap();
        }
        let removed = cleanup_old_logs(t.path(), 30, today).unwrap();
        assert_eq!(removed, 2); // 2025-01-01 and 2026-06-01
        assert!(t.path().join("app.2026-08-15.log").exists());
        assert!(t.path().join("app.2026-08-14.log").exists());
        assert!(t.path().join("not-a-log.txt").exists());
    }

    #[test]
    fn clear_history_keeps_only_today() {
        let t = dir();
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        for name in [
            "app.2026-08-15.log",
            "app.2026-08-15.1.log",
            "app.2026-08-14.log",
            "app.2026-08-10.log",
        ] {
            std::fs::write(t.path().join(name), b"x").unwrap();
        }
        let removed = clear_log_history(t.path(), today).unwrap();
        assert_eq!(removed, 2);
        assert!(t.path().join("app.2026-08-15.log").exists());
        assert!(t.path().join("app.2026-08-15.1.log").exists());
    }

    #[test]
    fn list_returns_names_sorted_desc() {
        let t = dir();
        std::fs::write(t.path().join("app.2026-08-15.log"), b"a").unwrap();
        std::fs::write(t.path().join("app.2026-08-14.log"), b"bb").unwrap();
        let files = list_log_files(t.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "app.2026-08-15.log");
        assert_eq!(files[0].size, 1);
    }
}
