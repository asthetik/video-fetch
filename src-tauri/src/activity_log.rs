use crate::error::{AppError, AppResult};
use chrono::NaiveDate;
use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Current-generation log file names (size-only rotation, at most 2 files):
/// `app.log` is the active file; once full it is rotated over `app.old.log`.
pub const ACTIVE_LOG_NAME: &str = "app.log";
pub const OLD_LOG_NAME: &str = "app.old.log";
/// Prefix of legacy date-named files (`app.YYYY-MM-DD[.seq].log`).
pub const LOG_PREFIX: &str = "app.";
pub const DEFAULT_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;
pub const DEFAULT_RETENTION_DAYS: u32 = 30;
pub const TAIL_MAX_LINES: usize = 1000;
pub const TAIL_MAX_BYTES: u64 = 256 * 1024;
pub const MAX_LOG_MESSAGE_CHARS: usize = 2000;

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
    /// Logging disabled fallback: keeps the directory path so the viewer
    /// commands can still answer (they will simply report missing files).
    pub fn disabled(logs_dir: PathBuf) -> Self {
        Self {
            logs_dir,
            guard: Mutex::new(None),
        }
    }

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

    let rotator = Rotator::open(logs_dir.clone(), max_file_size)?;
    let (non_blocking, guard) = tracing_appender::non_blocking(rotator);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::LocalTime::rfc_3339());
    // Release builds pin the filter to info (no verbose mode); debug builds
    // keep RUST_LOG as a developer escape hatch.
    #[cfg(debug_assertions)]
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    #[cfg(not(debug_assertions))]
    let filter = tracing_subscriber::EnvFilter::new("info");

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

/// Replace `http(s)://...` runs with `<url>` so error text from yt-dlp never
/// leaks full URLs (tracking params included) into the activity log.
pub fn redact_urls(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if input[i..].starts_with("http://") || input[i..].starts_with("https://") {
            result.push_str("<url>");
            while let Some(&(_, ch)) = chars.peek() {
                if ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>') {
                    break;
                }
                chars.next();
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Redact URLs and escape line breaks so external text (yt-dlp stderr, titles,
/// UI events) cannot forge additional log lines.
pub fn clean_log_message(input: &str) -> String {
    let redacted = redact_urls(input);
    if redacted.contains('\r') || redacted.contains('\n') {
        redacted.replace('\r', "\\r").replace('\n', "\\n")
    } else {
        redacted
    }
}

/// Parse a legacy date-named log file. The fixed current-generation names
/// (`app.log`/`app.old.log`) are not handled here; callers compare them
/// explicitly. Used only for legacy cleanup/listing/clearing.
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

/// Size-only rotating log writer:
///
/// ```text
/// app.log ──reaches max_size──►  1. remove app.old.log (best effort; Windows
///                                    rename does not guarantee overwriting)
///                                 2. close the current handle (Windows cannot
///                                    rename an open file)
///                                 3. rename app.log → app.old.log
///                                 4. open_append app.log (fresh, starts empty)
/// ```
///
/// Overwriting `app.old.log` intentionally discards the previous generation
/// (disk usage stays bounded at ≤ 2 × max_size).
pub(crate) struct Rotator {
    dir: PathBuf,
    max_size: u64,
    file: Option<File>,
}

impl Rotator {
    pub(crate) fn open(dir: PathBuf, max_size: u64) -> AppResult<Self> {
        let file = open_append(&dir.join(ACTIVE_LOG_NAME))?;
        Ok(Self {
            dir,
            max_size,
            file: Some(file),
        })
    }

    fn rotate(&mut self) -> AppResult<()> {
        let old = self.dir.join(OLD_LOG_NAME);
        let active = self.dir.join(ACTIVE_LOG_NAME);
        // 1. Remove the previous generation first (ignore NotFound).
        let _ = fs::remove_file(&old);
        // 2. Close the handle before renaming. If the rename fails, reopen the
        //    active file and fall back to append mode: only this rotation is
        //    lost, later writes will trigger it again.
        self.file = None;
        if let Err(e) = fs::rename(&active, &old) {
            self.file = Some(open_append(&active)?);
            return Err(AppError::Message(format!("滚动日志文件失败: {e}")));
        }
        // 4. Open the fresh active file.
        self.file = Some(open_append(&active)?);
        Ok(())
    }

    fn write_chunk(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Trust the on-disk length rather than a manual counter: external
        // truncation (clear_all_logs) cannot desynchronize it, and one stat
        // per event batch is negligible.
        let size = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map(|m| m.len())
            .unwrap_or(0);
        if self.max_size > 0 && size + buf.len() as u64 > self.max_size {
            self.rotate().map_err(std::io::Error::other)?;
        }
        let Some(file) = self.file.as_mut() else {
            return Err(std::io::Error::other("日志文件句柄不可用"));
        };
        file.write_all(buf)?;
        Ok(buf.len())
    }
}

impl Write for Rotator {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_chunk(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
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

fn truncate_file(path: &Path) -> AppResult<()> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| AppError::Message(format!("打开日志失败: {e}")))?;
    file.set_len(0)
        .map_err(|e| AppError::Message(format!("清空日志失败: {e}")))?;
    Ok(())
}

/// Three file kinds, three treatments: the active `app.log` is truncated in
/// place (the writer keeps an open handle, and Windows cannot delete an open
/// file, so truncation is the safe option); `app.old.log` is removed; legacy
/// date-named files are truncated when from today, removed otherwise.
pub fn clear_all_logs(dir: &Path, today: NaiveDate) -> AppResult<usize> {
    let mut cleared = 0;
    for entry in
        fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?
    {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ACTIVE_LOG_NAME {
            truncate_file(&entry.path())?;
            cleared += 1;
            continue;
        }
        if name == OLD_LOG_NAME {
            fs::remove_file(entry.path())
                .map_err(|e| AppError::Message(format!("删除日志失败: {e}")))?;
            cleared += 1;
            continue;
        }
        let Some((date, _)) = parse_file_name(&name) else {
            continue;
        };
        if date == today {
            truncate_file(&entry.path())?;
        } else {
            fs::remove_file(entry.path())
                .map_err(|e| AppError::Message(format!("删除日志失败: {e}")))?;
        }
        cleared += 1;
    }
    Ok(cleared)
}

/// List the log directory: fixed current-generation names (explicit
/// whitelist) plus legacy date-named files. Sorted by modification time,
/// newest first: one rule covers both the old/new coexistence window and the
/// new-files-only steady state.
pub fn list_log_files(dir: &Path) -> AppResult<Vec<LogFileInfo>> {
    let mut files = Vec::new();
    for entry in
        fs::read_dir(dir).map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?
    {
        let entry = entry.map_err(|e| AppError::Message(format!("读取日志目录失败: {e}")))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_file_name(&name).is_none() && name != ACTIVE_LOG_NAME && name != OLD_LOG_NAME {
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
    files.sort_by_key(|f| std::cmp::Reverse(f.modified_secs));
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
    fn rotator_rotates_app_log_into_app_old_log_on_size() {
        let t = dir();
        let mut rotator = Rotator::open(t.path().to_path_buf(), 10).unwrap();
        rotator.write_chunk(b"123456").unwrap();
        rotator.write_chunk(b"67890").unwrap(); // 6 + 5 > 10 → rotate
        assert_eq!(
            std::fs::read_to_string(t.path().join("app.old.log")).unwrap(),
            "123456"
        );
        assert_eq!(
            std::fs::read_to_string(t.path().join("app.log")).unwrap(),
            "67890"
        );
    }

    #[test]
    fn rotate_overwrites_existing_app_old_log() {
        let t = dir();
        std::fs::write(t.path().join("app.old.log"), b"stale generation").unwrap();
        std::fs::write(t.path().join("app.log"), b"first gen").unwrap();
        // open() appends to the existing app.log (9 bytes); this write triggers rotation.
        let mut rotator = Rotator::open(t.path().to_path_buf(), 10).unwrap();
        rotator.write_chunk(b"second gen").unwrap(); // 9 + 10 > 10 → rotate over the old generation
        assert_eq!(
            std::fs::read_to_string(t.path().join("app.old.log")).unwrap(),
            "first gen"
        );
        assert_eq!(
            std::fs::read_to_string(t.path().join("app.log")).unwrap(),
            "second gen"
        );
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
        // Legacy behavior regression: today's file truncated, older files removed.
        let t = dir();
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        std::fs::write(t.path().join("app.2026-08-15.log"), b"today content").unwrap();
        std::fs::write(t.path().join("app.2026-08-10.log"), b"old").unwrap();
        let cleared = clear_all_logs(t.path(), today).unwrap();
        assert_eq!(cleared, 2);
        assert_eq!(
            std::fs::read(t.path().join("app.2026-08-15.log")).unwrap(),
            Vec::<u8>::new()
        );
        assert!(!t.path().join("app.2026-08-10.log").exists());
    }

    #[test]
    fn list_recognizes_new_names_and_legacy_sorted_by_mtime() {
        let t = dir();
        // Three files with increasing mtimes: legacy < app.old.log < app.log.
        // Order must be by modification time, newest first, regardless of
        // name format (old/new coexistence window).
        for (name, mtime) in [
            ("app.2026-08-20.log", 1000u64),
            ("app.old.log", 2000),
            ("app.log", 3000),
            ("not-a-log.txt", 4000),
        ] {
            let path = t.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            let f = OpenOptions::new().write(true).open(&path).unwrap();
            let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(mtime);
            f.set_times(std::fs::FileTimes::new().set_modified(at))
                .unwrap();
        }
        let files = list_log_files(t.path()).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["app.log", "app.old.log", "app.2026-08-20.log"]);
    }

    #[test]
    fn clear_all_logs_truncates_app_log_removes_app_old_and_legacy() {
        let t = dir();
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        std::fs::write(t.path().join("app.log"), b"active").unwrap();
        std::fs::write(t.path().join("app.old.log"), b"old gen").unwrap();
        std::fs::write(t.path().join("app.2026-08-15.log"), b"today legacy").unwrap();
        std::fs::write(t.path().join("app.2026-08-10.log"), b"older legacy").unwrap();
        let cleared = clear_all_logs(t.path(), today).unwrap();
        assert_eq!(cleared, 4);
        // Active file is truncated, not removed (keeps the writer's handle valid; Windows-safe).
        assert!(t.path().join("app.log").exists());
        assert_eq!(
            std::fs::read(t.path().join("app.log")).unwrap(),
            Vec::<u8>::new()
        );
        assert!(!t.path().join("app.old.log").exists());
        // Legacy behavior regression assertions: today truncated, older removed.
        assert_eq!(
            std::fs::read(t.path().join("app.2026-08-15.log")).unwrap(),
            Vec::<u8>::new()
        );
        assert!(!t.path().join("app.2026-08-10.log").exists());
    }

    /// Regression pin: the fixed new names are not date files; the date-based
    /// cleaner must skip them.
    #[test]
    fn cleanup_skips_new_names() {
        let t = dir();
        let today = NaiveDate::from_ymd_opt(2026, 8, 15).unwrap();
        std::fs::write(t.path().join("app.log"), b"x").unwrap();
        std::fs::write(t.path().join("app.old.log"), b"x").unwrap();
        std::fs::write(t.path().join("app.2025-01-01.log"), b"x").unwrap();
        let removed = cleanup_old_logs(t.path(), 30, today).unwrap();
        assert_eq!(removed, 1);
        assert!(t.path().join("app.log").exists());
        assert!(t.path().join("app.old.log").exists());
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
        // Total length 65537; the block boundary lands at byte 1, splitting the leading
        // 3-byte multibyte character in half and forcing the reader to skip the incomplete prefix.
        let content = format!("汉{}x\n", "x".repeat(64 * 1024 - 4));
        std::fs::write(&path, &content).unwrap();
        let lines = read_log_tail(&path, 10, 1024 * 1024).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], content.trim_end());
    }

    #[test]
    fn level_mapping() {
        assert_eq!(level_from_str("error"), tracing::Level::ERROR);
        assert_eq!(level_from_str("warn"), tracing::Level::WARN);
        assert_eq!(level_from_str("info"), tracing::Level::INFO);
        assert_eq!(level_from_str("debug"), tracing::Level::DEBUG);
        assert_eq!(level_from_str("别的"), tracing::Level::DEBUG);
    }

    #[test]
    fn redact_urls_hides_urls() {
        assert_eq!(
            redact_urls("a https://www.bilibili.com/video/BV1xx?vd_source=abc b"),
            "a <url> b"
        );
        assert_eq!(redact_urls("http://x"), "<url>");
        assert_eq!(redact_urls("无 url 文本"), "无 url 文本");
    }

    #[test]
    fn clean_log_message_escapes_newlines() {
        assert_eq!(clean_log_message("a\nhttps://x\nb\r"), "a\\n<url>\\nb\\r");
        assert_eq!(clean_log_message("plain text"), "plain text");
    }
}
