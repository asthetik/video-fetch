use crate::error::{AppError, AppResult};
use chrono::NaiveDate;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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

#[derive(Debug, Clone, serde::Deserialize)]
pub struct UiLogEvent {
    pub level: String,
    pub category: String,
    pub message: String,
}

pub struct ActivityLog {
    logs_dir: PathBuf,
    guard: Mutex<Option<tracing_appender::non_blocking::WorkerGuard>>,
}

impl ActivityLog {
    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Flush and join the background writer; call once on app exit.
    pub fn flush(&self) {
        // Dropping the guard flushes the queue and joins the worker thread.
        let _ = self.guard.lock().ok().and_then(|mut g| g.take());
    }
}

pub fn install(
    logs_dir: PathBuf,
    max_file_size: u64,
    retention_days: u32,
) -> AppResult<ActivityLog> {
    fs::create_dir_all(&logs_dir)
        .map_err(|e| AppError::Message(format!("创建日志目录失败: {e}")))?;
    crate::fsutil::restrict_private_dir_perms(&logs_dir);
    let today = chrono::Local::now().date_naive();
    cleanup_old_logs(&logs_dir, retention_days, today)?;

    let rotator = Rotator::open(logs_dir.clone(), today, max_file_size)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(rotator);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339());
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"));

    #[cfg(debug_assertions)]
    {
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_target(false)
            .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339());
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(stderr_layer)
            .init();
    }
    #[cfg(not(debug_assertions))]
    {
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .init();
    }

    Ok(ActivityLog {
        logs_dir,
        guard: Mutex::new(Some(guard)),
    })
}

pub fn level_from_str(level: &str) -> tracing::Level {
    match level {
        "error" => tracing::Level::ERROR,
        "warn" => tracing::Level::WARN,
        "info" => tracing::Level::INFO,
        _ => tracing::Level::DEBUG,
    }
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
    for entry in
        fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?
    {
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

impl Write for Rotator {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_chunk(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

pub fn cleanup_old_logs(dir: &Path, retention_days: u32, today: NaiveDate) -> AppResult<usize> {
    let cutoff = today - chrono::Duration::days(retention_days as i64);
    let mut removed = 0;
    for entry in
        fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?
    {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some((date, _)) = parse_file_name(&name)
            && date < cutoff
        {
            fs::remove_file(entry.path())
                .map_err(|e| AppError::Message(format!("删除日志失败: {e}")))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Delete every log file except the active day, whose files are truncated in
/// place instead — the active writer keeps an open handle (Windows cannot
/// delete an open file), and truncating leaves that handle valid.
pub fn clear_all_logs(dir: &Path, today: NaiveDate) -> AppResult<usize> {
    let mut cleared = 0;
    for entry in
        fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?
    {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((date, _)) = parse_file_name(&name) else {
            continue;
        };
        if date == today {
            let file = OpenOptions::new()
                .write(true)
                .open(entry.path())
                .map_err(|e| AppError::Message(format!("打开日志失败: {e}")))?;
            file.set_len(0)
                .map_err(|e| AppError::Message(format!("清空日志失败: {e}")))?;
        } else {
            fs::remove_file(entry.path())
                .map_err(|e| AppError::Message(format!("删除日志失败: {e}")))?;
        }
        cleared += 1;
    }
    Ok(cleared)
}

pub fn list_log_files(dir: &Path) -> AppResult<Vec<LogFileInfo>> {
    let mut files = Vec::new();
    for entry in
        fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?
    {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_file_name(&name).is_none() {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| AppError::Message(format!("读取日志元数据失败: {e}")))?;
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

/// Return up to `max_lines` lines from the end of `path`, reading at most
/// `max_bytes` from the tail. Incomplete UTF-8 at the block boundary is skipped.
pub fn read_log_tail(path: &Path, max_lines: usize, max_bytes: u64) -> AppResult<Vec<String>> {
    if max_lines == 0 || max_bytes == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).map_err(|e| AppError::Message(format!("打开日志失败: {e}")))?;
    let len = file
        .metadata()
        .map_err(|e| AppError::Message(format!("读取日志元数据失败: {e}")))?
        .len();
    let block = 64 * 1024u64;
    let mut buf: Vec<u8> = Vec::new();
    let mut pos = len;
    while (buf.len() as u64) < max_bytes && pos > 0 {
        let start = pos.saturating_sub(block);
        let mut chunk = vec![0u8; (pos - start) as usize];
        file.seek(SeekFrom::Start(start))
            .map_err(|e| AppError::Message(format!("读取日志失败: {e}")))?;
        file.read_exact(&mut chunk)
            .map_err(|e| AppError::Message(format!("读取日志失败: {e}")))?;
        chunk.extend(buf);
        buf = chunk;
        pos = start;
        if buf.len() as u64 > max_bytes {
            buf.drain(..(buf.len() - max_bytes as usize));
        }
    }
    while !buf.is_empty() && std::str::from_utf8(&buf).is_err() {
        buf.remove(0);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    Ok(lines)
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
    fn clear_all_logs_deletes_old_and_truncates_today() {
        let t = dir();
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        std::fs::write(t.path().join("app.2026-08-15.log"), b"today content").unwrap();
        std::fs::write(t.path().join("app.2026-08-15.1.log"), b"today archive").unwrap();
        std::fs::write(t.path().join("app.2026-08-14.log"), b"old").unwrap();
        for name in ["app.2026-08-10.log"] {
            std::fs::write(t.path().join(name), b"old").unwrap();
        }
        let cleared = clear_all_logs(t.path(), today).unwrap();
        assert_eq!(cleared, 4);
        assert!(t.path().join("app.2026-08-15.log").exists());
        assert!(t.path().join("app.2026-08-15.1.log").exists());
        assert_eq!(
            std::fs::read(t.path().join("app.2026-08-15.log")).unwrap(),
            Vec::<u8>::new()
        );
        assert!(!t.path().join("app.2026-08-14.log").exists());
        assert!(!t.path().join("app.2026-08-10.log").exists());
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

    #[test]
    fn read_tail_respects_byte_and_line_caps() {
        let t = dir();
        let path = t.path().join("app.2026-08-15.log");
        let content = (0..100)
            .map(|i| format!("line{i:03}\n"))
            .collect::<String>();
        std::fs::write(&path, &content).unwrap();

        let lines = read_log_tail(&path, 5, 1024 * 1024).unwrap();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "line095");
        assert_eq!(lines[4], "line099");

        let small = read_log_tail(&path, 100, 7).unwrap();
        assert!(small.len() < 100);
    }

    #[test]
    fn read_tail_survives_utf8_cut_at_block_boundary() {
        let t = dir();
        let path = t.path().join("app.2026-08-15.log");
        // 多字节字符 "汉" 跨 64KB 块边界：填充到 64KB-1 再写 "\n汉\n"
        let mut content = "x".repeat(64 * 1024 - 1);
        content.push('\n');
        content.push_str("汉\n");
        std::fs::write(&path, &content).unwrap();
        let lines = read_log_tail(&path, 10, 1024 * 1024).unwrap();
        assert_eq!(lines[lines.len() - 1], "汉");
    }

    #[test]
    fn level_mapping() {
        assert_eq!(level_from_str("error"), tracing::Level::ERROR);
        assert_eq!(level_from_str("warn"), tracing::Level::WARN);
        assert_eq!(level_from_str("info"), tracing::Level::INFO);
        assert_eq!(level_from_str("debug"), tracing::Level::DEBUG);
        assert_eq!(level_from_str("别的"), tracing::Level::DEBUG);
    }
}
