use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::error::{AppError, AppResult};
use crate::models::{FormatOption, PageItem, VideoMeta};
use crate::platform::detect_platform;

#[derive(Debug, Clone)]
pub struct YtDlpConfig {
    pub yt_dlp_path: PathBuf,
    pub ffmpeg_path: Option<PathBuf>,
}

pub fn parse_progress_line(line: &str) -> Option<f64> {
    let line = line.trim();
    // yt-dlp ≥2024 often prints `[download] …%` progress on stdout when piped.
    if !line.starts_with("[download]") {
        return None;
    }
    line.split_whitespace().find_map(|part| {
        part.strip_suffix('%')
            .and_then(|num_str| num_str.parse::<f64>().ok())
    })
}

/// True when a stdout line is a progress/status line rather than `--print` filepath output.
fn is_ytdlp_status_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('[') || parse_progress_line(trimmed).is_some()
}

#[cfg(test)]
pub fn video_meta_from_yt_dlp_json(v: &serde_json::Value) -> AppResult<VideoMeta> {
    map_video_meta(v, false)
}

pub fn video_meta_from_yt_dlp_json_with_cookies(
    v: &serde_json::Value,
    has_cookies: bool,
) -> AppResult<VideoMeta> {
    map_video_meta(v, has_cookies)
}

fn map_video_meta(v: &serde_json::Value, has_cookies: bool) -> AppResult<VideoMeta> {
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| AppError::Message("yt-dlp JSON missing id".into()))?
        .to_string();

    let title = v
        .get("title")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let webpage_url = v
        .get("webpage_url")
        .or_else(|| v.get("original_url"))
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();

    let platform = detect_platform(&webpage_url)
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".into());

    let uploader = v
        .get("uploader")
        .and_then(|x| x.as_str())
        .map(str::to_string);

    let pages = parse_pages(v, &title, &id);

    // Playlist / multi-P JSON often has empty top-level formats; use first entry.
    let media_source = media_source_value(v);
    let mut thumbnail = v
        .get("thumbnail")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    if thumbnail.is_none() {
        thumbnail = media_source
            .get("thumbnail")
            .and_then(|x| x.as_str())
            .map(str::to_string);
    }
    let formats = parse_formats(media_source, has_cookies);

    Ok(VideoMeta {
        id,
        title,
        uploader,
        thumbnail,
        webpage_url,
        pages,
        formats,
        platform,
    })
}

/// Prefer top-level formats; fall back to the first playlist entry when empty.
fn media_source_value(v: &serde_json::Value) -> &serde_json::Value {
    let top_has_formats = v
        .get("formats")
        .and_then(|f| f.as_array())
        .is_some_and(|a| !a.is_empty());
    if top_has_formats {
        return v;
    }
    v.get("entries")
        .and_then(|e| e.as_array())
        .and_then(|arr| arr.first())
        .unwrap_or(v)
}

fn parse_pages(v: &serde_json::Value, default_title: &str, default_id: &str) -> Vec<PageItem> {
    if let Some(entries) = v.get("entries").and_then(|e| e.as_array())
        && !entries.is_empty()
    {
        return entries
            .iter()
            .enumerate()
            .map(|(i, entry)| PageItem {
                index: page_index_from_entry(entry, i),
                title: entry
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or(default_title)
                    .to_string(),
                page_id: entry
                    .get("id")
                    .and_then(|id| id.as_str())
                    .unwrap_or(default_id)
                    .to_string(),
            })
            .collect();
    }

    vec![PageItem {
        index: 1,
        title: default_title.to_string(),
        page_id: default_id.to_string(),
    }]
}

/// Bilibili / yt-dlp page numbers are 1-based (`?p=` / `playlist_index`).
fn page_index_from_entry(entry: &serde_json::Value, enumerate_idx: usize) -> u32 {
    if let Some(idx) = entry
        .get("playlist_index")
        .and_then(|x| x.as_u64())
        .map(|x| x as u32)
        && idx >= 1
    {
        return idx;
    }
    if let Some(url) = entry
        .get("webpage_url")
        .or_else(|| entry.get("url"))
        .and_then(|u| u.as_str())
        && let Some(p) = page_query_param(url)
    {
        return p;
    }
    (enumerate_idx as u32) + 1
}

fn page_query_param(url: &str) -> Option<u32> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        let mut kv = pair.splitn(2, '=');
        let key = kv.next()?;
        let val = kv.next().unwrap_or("");
        if key == "p" {
            return val.parse().ok().filter(|n| *n >= 1);
        }
    }
    None
}

fn parse_formats(v: &serde_json::Value, _has_cookies: bool) -> Vec<FormatOption> {
    let Some(formats) = v.get("formats").and_then(|f| f.as_array()) else {
        return Vec::new();
    };

    let mut out: Vec<FormatOption> = formats
        .iter()
        .filter_map(|f| {
            if is_audio_only_format(f) {
                return None;
            }

            let format_id = f.get("format_id")?.as_str()?.to_string();
            let height = f.get("height").and_then(|h| h.as_u64()).map(|h| h as u32);
            let fps = f
                .get("fps")
                .and_then(|fp| fp.as_f64())
                .map(|fp| fp.round() as u32);
            let vcodec = f.get("vcodec").and_then(|c| c.as_str());
            let tbr = f.get("tbr").and_then(|t| t.as_f64());
            let dynamic_range = f
                .get("dynamic_range")
                .and_then(|d| d.as_str())
                .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("sdr"));

            let label = build_format_label(height, fps, vcodec, tbr, dynamic_range);
            // Formats returned by yt-dlp for the current cookie state are downloadable
            // as-is; do not mark them "需登录" by resolution heuristics.
            let requires_login = false;

            Some(FormatOption {
                format_id,
                label,
                height,
                fps,
                tbr,
                requires_login,
            })
        })
        .collect();
    sort_formats_by_quality(&mut out);
    out
}

/// Highest resolution first, then highest bitrate within the same height.
fn sort_formats_by_quality(formats: &mut [FormatOption]) {
    formats.sort_by(|a, b| {
        let height = b.height.unwrap_or(0).cmp(&a.height.unwrap_or(0));
        if height != std::cmp::Ordering::Equal {
            return height;
        }
        b.tbr
            .unwrap_or(0.0)
            .partial_cmp(&a.tbr.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Drop audio-only DASH tracks from the quality picker (vcodec none / no height).
fn is_audio_only_format(f: &serde_json::Value) -> bool {
    let vcodec = f.get("vcodec").and_then(|v| v.as_str());
    if vcodec == Some("none") {
        return true;
    }
    let height_none = f.get("height").and_then(|h| h.as_u64()).is_none();
    // height-none heuristic: no height and no real video codec
    height_none && vcodec.map(|v| v.is_empty()).unwrap_or(true)
}

/// Merge selected video format with best audio for Bilibili DASH.
pub fn dash_format_selector(format_id: &str) -> String {
    if format_id.contains('+') || format_id.contains('/') {
        format_id.to_string()
    } else {
        format!("{format_id}+bestaudio/best")
    }
}

fn resolution_label(height: Option<u32>, fps: Option<u32>) -> String {
    match (height, fps) {
        (Some(h), Some(f)) if f > 30 => format!("{h}p{f}"),
        (Some(h), _) => format!("{h}p"),
        (_, Some(f)) => format!("{f}fps"),
        _ => "未知清晰度".into(),
    }
}

fn codec_short(vcodec: Option<&str>) -> Option<&'static str> {
    let v = vcodec?.to_ascii_lowercase();
    if v.is_empty() || v == "none" {
        return None;
    }
    if v.starts_with("av01") || v.starts_with("av1") {
        return Some("AV1");
    }
    if v.starts_with("hev1") || v.starts_with("hvc1") || v.contains("hevc") {
        return Some("HEVC");
    }
    if v.starts_with("avc1") || v.contains("h264") {
        return Some("AVC");
    }
    if v.starts_with("vp09") || v.starts_with("vp9") {
        return Some("VP9");
    }
    None
}

fn build_format_label(
    height: Option<u32>,
    fps: Option<u32>,
    vcodec: Option<&str>,
    tbr: Option<f64>,
    dynamic_range: Option<&str>,
) -> String {
    let mut parts = vec![resolution_label(height, fps)];
    if let Some(codec) = codec_short(vcodec) {
        parts.push(codec.to_string());
    }
    if let Some(dr) = dynamic_range {
        parts.push(dr.to_ascii_uppercase());
    }
    if let Some(rate) = tbr.filter(|r| *r > 0.0) {
        parts.push(format!("{}kbps", rate.round() as u32));
    }
    parts.join(" · ")
}

fn missing_yt_dlp_message() -> String {
    if cfg!(debug_assertions) {
        "缺少 yt-dlp，请安装或运行 python3 scripts/fetch_sidecars.py".into()
    } else {
        "安装包缺少下载组件（yt-dlp）。请重新安装影取。".into()
    }
}

pub async fn resolve_meta(
    cfg: &YtDlpConfig,
    url: &str,
    cookies_path: Option<&Path>,
) -> AppResult<VideoMeta> {
    if !cfg.yt_dlp_path.exists() {
        return Err(AppError::Message(missing_yt_dlp_message()));
    }

    let mut cmd = tokio::process::Command::new(&cfg.yt_dlp_path);
    // Keep playlist entries so Bilibili multi-P `pages` can be mapped.
    cmd.arg("-J");

    if let Some(path) = cfg.ffmpeg_path.as_ref() {
        cmd.arg("--ffmpeg-location").arg(path);
    }

    let has_cookies = cookies_path.is_some_and(|p| p.exists());
    if let Some(cookies) = cookies_path.filter(|p| p.exists()) {
        cmd.arg("--cookies").arg(cookies);
    }

    cmd.arg(url);

    let output = cmd
        .output()
        .await
        .map_err(|e| AppError::Message(format!("无法启动 yt-dlp: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Message(format!("yt-dlp 解析失败: {stderr}")));
    }

    let v: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| AppError::Message(format!("yt-dlp 输出不是有效 JSON: {e}")))?;

    video_meta_from_yt_dlp_json_with_cookies(&v, has_cookies)
}

/// Inputs for a single yt-dlp download spawn.
pub struct DownloadRequest<'a> {
    pub cfg: &'a YtDlpConfig,
    pub job_id: &'a str,
    pub url: &'a str,
    pub format_id: &'a str,
    pub output_template: &'a str,
    pub output_dir: &'a Path,
    pub cookies_path: Option<&'a Path>,
    pub children: &'a Arc<Mutex<HashMap<String, tokio::process::Child>>>,
}

/// Spawn yt-dlp to download a single video and stream progress from stdout/stderr.
pub async fn download(
    req: DownloadRequest<'_>,
    on_progress: impl Fn(f64) + Send,
) -> AppResult<PathBuf> {
    let DownloadRequest {
        cfg,
        job_id,
        url,
        format_id,
        output_template,
        output_dir,
        cookies_path,
        children,
    } = req;

    if !cfg.yt_dlp_path.exists() {
        return Err(AppError::Message(missing_yt_dlp_message()));
    }

    #[cfg(not(debug_assertions))]
    {
        if cfg.ffmpeg_path.is_none() {
            return Err(AppError::Message(
                "安装包缺少下载组件（ffmpeg）。请重新安装影取。".into(),
            ));
        }
    }

    let output_spec = output_dir.join(output_template);
    let selector = dash_format_selector(format_id);
    let mut cmd = Command::new(&cfg.yt_dlp_path);
    cmd.arg("-f")
        .arg(&selector)
        .arg("-o")
        .arg(&output_spec)
        .arg("--no-playlist")
        .arg("--newline")
        .arg("--progress")
        .arg("--print")
        .arg("after_move:filepath");

    if let Some(path) = cfg.ffmpeg_path.as_ref() {
        cmd.arg("--ffmpeg-location").arg(path);
    }

    if let Some(cookies) = cookies_path.filter(|p| p.exists()) {
        cmd.arg("--cookies").arg(cookies);
    }

    cmd.arg(url);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Message(format!("无法启动 yt-dlp: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Message("yt-dlp stdout unavailable".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Message("yt-dlp stderr unavailable".into()))?;

    {
        let mut guard = children
            .lock()
            .map_err(|e| AppError::Message(format!("download registry lock poisoned: {e}")))?;
        guard.insert(job_id.to_string(), child);
    }

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut final_path: Option<PathBuf> = None;
    let mut stderr_tail = String::new();
    let mut stdout_done = false;
    let mut stderr_done = false;

    loop {
        if stdout_done && stderr_done {
            break;
        }
        tokio::select! {
            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(line)) => {
                        // Recent yt-dlp prints download progress on stdout when piped.
                        if let Some(p) = parse_progress_line(&line) {
                            on_progress(p);
                        } else if !is_ytdlp_status_line(&line) {
                            let trimmed = line.trim();
                            if !trimmed.is_empty() {
                                final_path = Some(PathBuf::from(trimmed));
                            }
                        }
                    }
                    Ok(None) => stdout_done = true,
                    Err(e) => {
                        remove_child(children, job_id);
                        return Err(AppError::Message(format!("读取 yt-dlp 输出失败: {e}")));
                    }
                }
            }
            line = stderr_lines.next_line(), if !stderr_done => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(p) = parse_progress_line(&line) {
                            on_progress(p);
                        }
                        if stderr_tail.len() > 8192 {
                            let split = stderr_tail.len().saturating_sub(4096);
                            stderr_tail = stderr_tail.split_off(split);
                        }
                        stderr_tail.push_str(&line);
                        stderr_tail.push('\n');
                    }
                    Ok(None) => stderr_done = true,
                    Err(e) => {
                        remove_child(children, job_id);
                        return Err(AppError::Message(format!("读取 yt-dlp 进度失败: {e}")));
                    }
                }
            }
        }
    }

    let mut child = take_child(children, job_id)?;
    let status = child
        .wait()
        .await
        .map_err(|e| AppError::Message(format!("等待 yt-dlp 结束失败: {e}")))?;

    if !status.success() {
        let detail = stderr_tail.trim();
        let msg = if detail.is_empty() {
            "yt-dlp 下载失败".to_string()
        } else {
            format!("yt-dlp 下载失败: {detail}")
        };
        return Err(AppError::Message(msg));
    }

    final_path.ok_or_else(|| AppError::Message("yt-dlp 未返回输出路径".into()))
}

fn remove_child(children: &Arc<Mutex<HashMap<String, tokio::process::Child>>>, job_id: &str) {
    if let Ok(mut guard) = children.lock() {
        guard.remove(job_id);
    }
}

fn take_child(
    children: &Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    job_id: &str,
) -> AppResult<tokio::process::Child> {
    children
        .lock()
        .map_err(|e| AppError::Message(format!("download registry lock poisoned: {e}")))?
        .remove(job_id)
        .ok_or_else(|| AppError::Message("yt-dlp 进程已结束".into()))
}

pub fn kill_download(
    children: &Arc<Mutex<HashMap<String, tokio::process::Child>>>,
    job_id: &str,
) -> bool {
    let Ok(mut guard) = children.lock() else {
        return false;
    };
    if let Some(mut child) = guard.remove(job_id) {
        let _ = child.start_kill();
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_percent_progress() {
        let line = "[download]  45.3% of  10.00MiB at  1.00MiB/s ETA 00:05";
        assert_eq!(parse_progress_line(line), Some(45.3));
    }

    #[test]
    fn parses_complete_progress() {
        let line = "[download] 100% of  10.00MiB in 00:10";
        assert_eq!(parse_progress_line(line), Some(100.0));
    }

    #[test]
    fn ignores_non_download_lines() {
        assert_eq!(parse_progress_line("[info] downloading"), None);
    }

    #[test]
    fn status_line_detection_keeps_filepath_print() {
        assert!(is_ytdlp_status_line(
            "[download]  12.5% of   10.00MiB at    1.00MiB/s ETA 00:08"
        ));
        assert!(is_ytdlp_status_line(
            "[Merger] Merging formats into \"a.mp4\""
        ));
        assert!(!is_ytdlp_status_line("/tmp/video.mp4"));
    }

    #[test]
    fn maps_minimal_json() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "uploader": "up",
            "thumbnail": "https://example.com/t.jpg",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [
                {"format_id": "80", "format_note": "1080P", "height": 1080, "fps": 30}
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        assert_eq!(meta.id, "BV1xx");
        assert_eq!(meta.platform, "bilibili");
        assert!(!meta.formats.is_empty());
        assert_eq!(meta.formats[0].format_id, "80");
        assert_eq!(meta.formats[0].label, "1080p");
    }

    #[test]
    fn format_labels_distinguish_codec_and_bitrate() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [
                {"format_id": "80", "height": 1080, "fps": 30, "vcodec": "avc1.640032", "tbr": 2500.4},
                {"format_id": "112", "height": 1080, "fps": 30, "vcodec": "hev1.1.6.L120.90", "tbr": 1800.0},
                {"format_id": "116", "height": 1080, "fps": 60, "vcodec": "avc1.640032", "tbr": 4000.0}
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        // Same height: highest tbr first (default pick = formats[0]).
        assert_eq!(meta.formats[0].format_id, "116");
        assert_eq!(meta.formats[0].label, "1080p60 · AVC · 4000kbps");
        assert_eq!(meta.formats[0].tbr, Some(4000.0));
        assert_eq!(meta.formats[1].label, "1080p · AVC · 2500kbps");
        assert_eq!(meta.formats[2].label, "1080p · HEVC · 1800kbps");
    }

    #[test]
    fn formats_sorted_by_height_then_bitrate_desc() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [
                {"format_id": "a", "height": 1080, "fps": 30, "vcodec": "avc1", "tbr": 4441.0},
                {"format_id": "b", "height": 720, "fps": 30, "vcodec": "avc1", "tbr": 9000.0},
                {"format_id": "c", "height": 1080, "fps": 30, "vcodec": "avc1", "tbr": 7706.0}
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        assert_eq!(meta.formats[0].format_id, "c");
        assert_eq!(meta.formats[1].format_id, "a");
        assert_eq!(meta.formats[2].format_id, "b");
    }

    #[test]
    fn formats_never_marked_requires_login_when_listed() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [
                {"format_id": "80", "format_note": "1080P", "height": 1080, "fps": 30},
                {"format_id": "64", "format_note": "720P", "height": 720, "fps": 30},
                {"format_id": "74", "format_note": "60fps", "height": 720, "fps": 60}
            ]
        });
        let meta = video_meta_from_yt_dlp_json_with_cookies(&v, false).unwrap();
        assert!(meta.formats.iter().all(|f| !f.requires_login));
    }

    #[test]
    fn no_requires_login_when_cookies_present() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [
                {"format_id": "80", "format_note": "1080P", "height": 1080, "fps": 30}
            ]
        });
        let meta = video_meta_from_yt_dlp_json_with_cookies(&v, true).unwrap();
        assert!(!meta.formats[0].requires_login);
    }

    #[test]
    fn default_single_page_when_no_entries() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": []
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        assert_eq!(meta.pages.len(), 1);
        assert_eq!(meta.pages[0].index, 1);
        assert_eq!(meta.pages[0].page_id, "BV1xx");
    }

    #[test]
    fn maps_multi_entry_playlist_with_one_based_indexes() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "合集",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [],
            "entries": [
                {
                    "id": "BV1xx",
                    "title": "P1 开场",
                    "playlist_index": 1,
                    "webpage_url": "https://www.bilibili.com/video/BV1xx?p=1",
                    "thumbnail": "https://example.com/p1.jpg",
                    "formats": [
                        {"format_id": "80", "format_note": "1080P", "height": 1080, "fps": 30, "vcodec": "avc1"},
                        {"format_id": "30280", "format_note": "audio", "height": null, "vcodec": "none", "acodec": "mp4a"}
                    ]
                },
                {
                    "id": "BV1xx",
                    "title": "P2 正片",
                    "playlist_index": 2,
                    "webpage_url": "https://www.bilibili.com/video/BV1xx?p=2",
                    "formats": [
                        {"format_id": "64", "format_note": "720P", "height": 720, "fps": 30, "vcodec": "avc1"}
                    ]
                }
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        assert_eq!(meta.pages.len(), 2);
        assert_eq!(meta.pages[0].index, 1);
        assert_eq!(meta.pages[0].title, "P1 开场");
        assert_eq!(meta.pages[1].index, 2);
        assert_eq!(
            meta.thumbnail.as_deref(),
            Some("https://example.com/p1.jpg")
        );
        assert_eq!(meta.formats.len(), 1);
        assert_eq!(meta.formats[0].format_id, "80");
    }

    #[test]
    fn page_index_falls_back_to_webpage_url_query() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "合集",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "entries": [
                {
                    "id": "a",
                    "title": "second",
                    "webpage_url": "https://www.bilibili.com/video/BV1xx?p=3"
                }
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        assert_eq!(meta.pages[0].index, 3);
    }

    #[test]
    fn filters_audio_only_formats() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "demo",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [
                {"format_id": "80", "format_note": "1080P", "height": 1080, "fps": 30, "vcodec": "avc1"},
                {"format_id": "30280", "vcodec": "none", "acodec": "mp4a", "height": null},
                {"format_id": "audio2", "acodec": "mp4a"}
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        assert_eq!(meta.formats.len(), 1);
        assert_eq!(meta.formats[0].format_id, "80");
    }

    #[test]
    fn dash_format_selector_merges_bestaudio() {
        assert_eq!(dash_format_selector("80"), "80+bestaudio/best");
        assert_eq!(dash_format_selector("80+bestaudio"), "80+bestaudio");
        assert_eq!(dash_format_selector("bestvideo/best"), "bestvideo/best");
    }
}
