use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::error::{AppError, AppResult};
use crate::models::{FormatOption, PageItem, VideoMeta};
use crate::platform::{canonicalize_video_url, detect_platform};

#[derive(Debug, Clone)]
pub struct YtDlpConfig {
    pub yt_dlp_path: PathBuf,
    pub ffmpeg_path: Option<PathBuf>,
}

/// Structured download progress from yt-dlp (percent is 0.0..=100.0).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressUpdate {
    pub percent: f64,
    pub speed: Option<f64>,
    pub eta: Option<u64>,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct YtDlpProgressJson {
    status: Option<String>,
    downloaded_bytes: Option<f64>,
    total_bytes: Option<f64>,
    total_bytes_estimate: Option<f64>,
    speed: Option<f64>,
    eta: Option<f64>,
    #[serde(rename = "_percent")]
    percent: Option<f64>,
}

fn bytes_opt(v: Option<f64>) -> Option<u64> {
    v.filter(|n| n.is_finite() && *n >= 0.0).map(|n| n as u64)
}

fn parse_progress_json(line: &str) -> Option<ProgressUpdate> {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return None;
    }
    let parsed: YtDlpProgressJson = serde_json::from_str(trimmed).ok()?;
    let downloaded = bytes_opt(parsed.downloaded_bytes);
    let total = bytes_opt(parsed.total_bytes).or_else(|| bytes_opt(parsed.total_bytes_estimate));
    let speed = parsed.speed.filter(|n| n.is_finite() && *n > 0.0);
    let eta = parsed
        .eta
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| n.round() as u64);

    let percent = if parsed.status.as_deref() == Some("finished") {
        100.0
    } else if let (Some(done), Some(all)) = (downloaded, total) {
        if all > 0 {
            ((done as f64) * 100.0 / (all as f64)).clamp(0.0, 100.0)
        } else {
            0.0
        }
    } else {
        let p = parsed.percent?;
        p.clamp(0.0, 100.0)
    };

    Some(ProgressUpdate {
        percent,
        speed,
        eta,
        downloaded_bytes: downloaded,
        total_bytes: total,
    })
}

/// Legacy `[download] 45.3% of …` text progress (percent only).
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

/// Prefer JSON progress lines; fall back to legacy text percent parsing.
pub fn parse_progress_update(line: &str) -> Option<ProgressUpdate> {
    if let Some(update) = parse_progress_json(line) {
        return Some(update);
    }
    parse_progress_line(line).map(|percent| ProgressUpdate {
        percent,
        ..Default::default()
    })
}

/// True when a stdout line is a progress/status line rather than `--print` filepath output.
fn is_ytdlp_status_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('[') || trimmed.starts_with('{')
}

pub fn video_meta_from_yt_dlp_json(v: &serde_json::Value) -> AppResult<VideoMeta> {
    map_video_meta(v)
}

fn map_video_meta(v: &serde_json::Value) -> AppResult<VideoMeta> {
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

    let mut thumbnail = v
        .get("thumbnail")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    if thumbnail.is_none() {
        let media_source = media_source_value(v);
        thumbnail = media_source
            .get("thumbnail")
            .and_then(|x| x.as_str())
            .map(str::to_string);
    }
    let formats = finalize_formats_for_pages(collect_formats_from_playlist(v), pages.len());

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

/// Gather video formats from the playlist JSON: top-level plus every entry that
/// already carries a `formats` list (not only the first page).
fn collect_formats_from_playlist(v: &serde_json::Value) -> Vec<FormatOption> {
    let mut out = parse_formats(v);
    if let Some(entries) = v.get("entries").and_then(|e| e.as_array()) {
        for entry in entries {
            out.extend(parse_formats(entry));
        }
    }
    if out.is_empty() {
        out = parse_formats(media_source_value(v));
    }
    out
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

fn parse_formats(v: &serde_json::Value) -> Vec<FormatOption> {
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

            Some(FormatOption {
                format_id,
                label,
                height,
                fps,
                tbr,
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

/// Prefix for multi-P height preferences (`vh1080` → max 1080p per part).
pub const HEIGHT_FORMAT_PREFIX: &str = "vh";

/// Cap on pages probed when the anthology is large.
const MULTI_PAGE_FORMAT_SAMPLES: usize = 8;

/// Probe every page when the anthology has at most this many parts (avoids gaps
/// like skipping P8 when count is 9 and the sample cap is 8).
const MULTI_PAGE_FULL_PROBE_AT_MOST: usize = 16;

pub fn parse_height_format_id(format_id: &str) -> Option<u32> {
    format_id
        .strip_prefix(HEIGHT_FORMAT_PREFIX)?
        .parse()
        .ok()
        .filter(|h| *h > 0)
}

/// Multi-P: collapse to unique heights seen in `formats` (one `vh{N}` option each).
/// Single-P: return `formats` unchanged (keep codec/bitrate detail).
pub fn finalize_formats_for_pages(
    formats: Vec<FormatOption>,
    page_count: usize,
) -> Vec<FormatOption> {
    if page_count <= 1 {
        return formats;
    }
    multi_page_quality_from_observed(&formats)
}

fn distinct_heights(formats: &[FormatOption]) -> Vec<u32> {
    use std::collections::BTreeSet;
    formats
        .iter()
        .filter_map(|f| f.height.filter(|h| *h > 0))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn multi_page_quality_from_observed(observed: &[FormatOption]) -> Vec<FormatOption> {
    use std::collections::BTreeMap;

    // height -> max fps (label only)
    let mut by_height: BTreeMap<u32, Option<u32>> = BTreeMap::new();
    for f in observed {
        let Some(h) = f.height.filter(|h| *h > 0) else {
            continue;
        };
        let entry = by_height.entry(h).or_insert(None);
        *entry = match (*entry, f.fps) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }

    by_height
        .into_iter()
        .rev()
        .map(|(h, fps)| FormatOption {
            format_id: format!("{HEIGHT_FORMAT_PREFIX}{h}"),
            label: resolution_label(Some(h), fps),
            height: Some(h),
            fps,
            tbr: None,
        })
        .collect()
}

/// 1-based page indexes to probe for formats.
///
/// Returns every page when `page_count <= max_samples`; otherwise first, last,
/// and evenly spaced intermediates (at most `max_samples` indexes).
pub fn sample_page_indices(page_count: u32, max_samples: usize) -> Vec<u32> {
    if page_count == 0 || max_samples == 0 {
        return Vec::new();
    }
    if page_count as usize <= max_samples {
        return (1..=page_count).collect();
    }
    use std::collections::BTreeSet;
    let mut set = BTreeSet::new();
    let last_i = (max_samples - 1) as u32;
    for i in 0..max_samples {
        let idx = 1 + (i as u32) * (page_count - 1) / last_i;
        set.insert(idx.clamp(1, page_count));
    }
    set.into_iter().collect()
}

fn page_url_for_index(base: &str, page_index: u32) -> String {
    let Ok(mut u) = url::Url::parse(base) else {
        return if page_index <= 1 {
            base.to_string()
        } else {
            format!("{base}?p={page_index}")
        };
    };
    let pairs: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| k != "p")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    {
        let mut qp = u.query_pairs_mut();
        qp.clear();
        for (k, v) in &pairs {
            qp.append_pair(k, v);
        }
        if page_index > 1 {
            qp.append_pair("p", &page_index.to_string());
        }
    }
    u.to_string()
}

/// Build a yt-dlp `-f` selector for Bilibili DASH.
///
/// Height preferences (`vh1080`) pick the best stream at or below that height on
/// each part. Exact numeric ids (usually from a single-P resolve) are preferred
/// first, then `bestvideo+bestaudio` / `best` — needed because multi-P DASH ids
/// are per-cid and P1's id is often missing elsewhere.
pub fn dash_format_selector(format_id: &str) -> String {
    if format_id.contains('+') || format_id.contains('/') {
        return format_id.to_string();
    }
    if let Some(height) = parse_height_format_id(format_id) {
        return format!("bestvideo[height<={height}]+bestaudio/best");
    }
    format!("{format_id}+bestaudio/bestvideo+bestaudio/best")
}

pub(crate) fn resolution_label(height: Option<u32>, fps: Option<u32>) -> String {
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

/// On Windows, spawn without a console window (CREATE_NO_WINDOW).
/// Otherwise GUI launches of yt-dlp briefly show a black terminal window.
#[cfg_attr(not(windows), allow(unused_variables))]
fn hide_windows_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
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

    let url = canonicalize_video_url(url);

    let mut cmd = tokio::process::Command::new(&cfg.yt_dlp_path);
    hide_windows_console(&mut cmd);
    // Keep playlist entries so Bilibili multi-P `pages` can be mapped.
    cmd.arg("-J");

    if let Some(path) = cfg.ffmpeg_path.as_ref() {
        cmd.arg("--ffmpeg-location").arg(path);
    }

    if let Some(cookies) = cookies_path.filter(|p| p.exists()) {
        cmd.arg("--cookies").arg(cookies);
    }

    cmd.arg(&url);

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

    let mut meta = video_meta_from_yt_dlp_json(&v)?;
    if meta.pages.len() > 1 {
        enrich_multi_page_formats_by_sampling(cfg, cookies_path, &mut meta).await;
    }
    Ok(meta)
}

/// Probe additional parts when playlist `-J` only filled formats for early entries.
///
/// Skips probing when the playlist JSON already exposed ≥2 distinct heights.
/// Anthologies with ≤`MULTI_PAGE_FULL_PROBE_AT_MOST` parts probe every page;
/// larger ones probe at most `MULTI_PAGE_FORMAT_SAMPLES` (skipping page 1 when
/// its formats are already present).
async fn enrich_multi_page_formats_by_sampling(
    cfg: &YtDlpConfig,
    cookies_path: Option<&Path>,
    meta: &mut VideoMeta,
) {
    let known_heights = distinct_heights(&meta.formats);
    if known_heights.len() >= 2 {
        return;
    }

    let page_count = meta.pages.len();
    let max_samples = if page_count <= MULTI_PAGE_FULL_PROBE_AT_MOST {
        page_count
    } else {
        MULTI_PAGE_FORMAT_SAMPLES
    };
    let mut indices = sample_page_indices(page_count as u32, max_samples);
    if !known_heights.is_empty() {
        indices.retain(|&i| i != 1);
    }
    if indices.is_empty() {
        return;
    }

    let base = if meta.webpage_url.is_empty() {
        return;
    } else {
        meta.webpage_url.clone()
    };

    let mut observed = meta.formats.clone();
    let mut handles = Vec::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(4));
    for index in indices {
        let page_url = page_url_for_index(&base, index);
        let cfg = cfg.clone();
        let cookies = cookies_path.map(Path::to_path_buf);
        let permit = Arc::clone(&sem);
        handles.push(tokio::spawn(async move {
            let _permit = permit.acquire_owned().await.ok()?;
            let formats = fetch_formats_for_url(&cfg, &page_url, cookies.as_deref()).await;
            Some((index, page_url, formats))
        }));
    }

    let mut failed = 0usize;
    for handle in handles {
        match handle.await {
            Ok(Some((_, _, Some(formats)))) => observed.extend(formats),
            Ok(Some((index, page_url, None))) => {
                failed += 1;
                eprintln!("videofetch: format probe failed for page {index} ({page_url})");
            }
            Ok(None) | Err(_) => {
                failed += 1;
                eprintln!("videofetch: format probe task failed");
            }
        }
    }
    if failed > 0 {
        eprintln!(
            "videofetch: {failed} multi-P format probe(s) failed; quality list may be incomplete"
        );
    }

    meta.formats = finalize_formats_for_pages(observed, meta.pages.len());
}

async fn fetch_formats_for_url(
    cfg: &YtDlpConfig,
    url: &str,
    cookies_path: Option<&Path>,
) -> Option<Vec<FormatOption>> {
    let mut cmd = tokio::process::Command::new(&cfg.yt_dlp_path);
    hide_windows_console(&mut cmd);
    cmd.arg("-J").arg("--no-playlist");
    if let Some(path) = cfg.ffmpeg_path.as_ref() {
        cmd.arg("--ffmpeg-location").arg(path);
    }
    if let Some(cookies) = cookies_path.filter(|p| p.exists()) {
        cmd.arg("--cookies").arg(cookies);
    }
    cmd.arg(url);

    let output = cmd.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(parse_formats(&v))
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
    pub cancelled: &'a Arc<Mutex<HashMap<String, bool>>>,
}

fn job_is_cancelled(cancelled: &Arc<Mutex<HashMap<String, bool>>>, job_id: &str) -> bool {
    cancelled
        .lock()
        .ok()
        .and_then(|m| m.get(job_id).copied())
        .unwrap_or(false)
}

/// Spawn yt-dlp to download a single video and stream progress from stdout/stderr.
pub async fn download(
    req: DownloadRequest<'_>,
    on_progress: impl Fn(ProgressUpdate) + Send,
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
        cancelled,
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

    let url = canonicalize_video_url(url);

    let output_spec = output_dir.join(output_template);
    let selector = dash_format_selector(format_id);
    let mut cmd = Command::new(&cfg.yt_dlp_path);
    hide_windows_console(&mut cmd);
    cmd.arg("-f").arg(&selector);
    // Height-preference selectors (`vh{N}` -> bestvideo[height<=N]) pick among
    // multiple codecs, so sort by resolution then bitrate to keep the default
    // download on the highest-bitrate stream. Exact selectors need no sorting.
    if parse_height_format_id(format_id).is_some() {
        cmd.arg("--format-sort").arg("res,tbr");
    }
    cmd
        .arg("-o")
        .arg(&output_spec)
        .arg("--no-playlist")
        .arg("--newline")
        .arg("--progress")
        // Emit one JSON object per progress tick (public fields under `progress`).
        .arg("--progress-template")
        .arg("download:%(progress)j")
        // Prefer UTF-8 so non-ASCII titles don't garble `--print` paths on Windows.
        .arg("--encoding")
        .arg("utf-8")
        .arg("--print")
        .arg("after_move:filepath");

    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.env("PYTHONUTF8", "1");

    if let Some(path) = cfg.ffmpeg_path.as_ref() {
        cmd.arg("--ffmpeg-location").arg(path);
    }

    if let Some(cookies) = cookies_path.filter(|p| p.exists()) {
        cmd.arg("--cookies").arg(cookies);
    }

    cmd.arg(&url);
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

    // Cancel may have raced before the child was registered; honor it now.
    if job_is_cancelled(cancelled, job_id) {
        kill_download(children, job_id);
        return Err(AppError::Message("用户取消下载".into()));
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
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                if job_is_cancelled(cancelled, job_id) {
                    kill_download(children, job_id);
                    return Err(AppError::Message("用户取消下载".into()));
                }
            }
            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(line)) => {
                        // JSON template + legacy text progress both appear on stdout when piped.
                        if let Some(update) = parse_progress_update(&line) {
                            on_progress(update);
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
                        // Legacy only: avoid double-firing when JSON also lands on stderr.
                        if let Some(percent) = parse_progress_line(&line) {
                            on_progress(ProgressUpdate {
                                percent,
                                ..Default::default()
                            });
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
        kill_child_process_tree(&mut child);
        return true;
    }
    false
}

/// Terminate yt-dlp and any children (e.g. ffmpeg). On Windows, `Child::start_kill`
/// only kills the root process and leaves downloads running in the tree.
fn kill_child_process_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    {
        if let Some(pid) = child.id() {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            // std::process::Command — same CREATE_NO_WINDOW flag as hide_windows_console.
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .status();
            return;
        }
    }
    let _ = child.start_kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_percent_progress() {
        let line = "[download]  45.3% of  10.00MiB at  1.00MiB/s ETA 00:05";
        assert_eq!(parse_progress_line(line), Some(45.3));
        assert_eq!(
            parse_progress_update(line),
            Some(ProgressUpdate {
                percent: 45.3,
                ..Default::default()
            })
        );
    }

    #[test]
    fn parses_complete_progress() {
        let line = "[download] 100% of  10.00MiB in 00:10";
        assert_eq!(parse_progress_line(line), Some(100.0));
    }

    #[test]
    fn ignores_non_download_lines() {
        assert_eq!(parse_progress_line("[info] downloading"), None);
        assert_eq!(parse_progress_update("[info] downloading"), None);
    }

    #[test]
    fn parses_json_progress() {
        let line = r#"{"status":"downloading","downloaded_bytes":4530000,"total_bytes":10000000,"speed":1048576.0,"eta":5.2}"#;
        let update = parse_progress_update(line).expect("json progress");
        assert!((update.percent - 45.3).abs() < 0.01);
        assert_eq!(update.speed, Some(1048576.0));
        assert_eq!(update.eta, Some(5));
        assert_eq!(update.downloaded_bytes, Some(4_530_000));
        assert_eq!(update.total_bytes, Some(10_000_000));
    }

    #[test]
    fn parses_finished_json_progress_as_100() {
        let line = r#"{"status":"finished","downloaded_bytes":1000,"total_bytes":1000,"speed":null,"eta":null}"#;
        let update = parse_progress_update(line).expect("finished");
        assert_eq!(update.percent, 100.0);
        assert_eq!(update.speed, None);
        assert_eq!(update.eta, None);
    }

    #[test]
    fn parses_json_progress_with_total_bytes_estimate() {
        let line = r#"{"status":"downloading","downloaded_bytes":2500,"total_bytes":null,"total_bytes_estimate":10000,"speed":100.0,"eta":75}"#;
        let update = parse_progress_update(line).expect("estimate");
        assert_eq!(update.percent, 25.0);
        assert_eq!(update.total_bytes, Some(10_000));
        assert_eq!(update.eta, Some(75));
    }

    #[test]
    fn parses_json_progress_with_percent_only() {
        let line = r#"{"status":"downloading","_percent":12.5,"speed":null,"eta":null}"#;
        let update = parse_progress_update(line).expect("_percent");
        assert_eq!(update.percent, 12.5);
        assert_eq!(update.downloaded_bytes, None);
        assert_eq!(update.total_bytes, None);
    }

    #[test]
    fn ignores_malformed_json_progress() {
        assert_eq!(parse_progress_update("{not-json"), None);
        assert_eq!(
            parse_progress_update(r#"{"status":"downloading","speed":1.0}"#),
            None
        );
    }

    #[test]
    fn status_line_detection_keeps_filepath_print() {
        assert!(is_ytdlp_status_line(
            "[download]  12.5% of   10.00MiB at    1.00MiB/s ETA 00:08"
        ));
        assert!(is_ytdlp_status_line(
            "[Merger] Merging formats into \"a.mp4\""
        ));
        assert!(is_ytdlp_status_line(
            r#"{"status":"downloading","downloaded_bytes":1,"total_bytes":10}"#
        ));
        assert!(!is_ytdlp_status_line("virtual/out/video.mp4"));
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
        let ids: Vec<_> = meta.formats.iter().map(|f| f.format_id.as_str()).collect();
        assert_eq!(ids, vec!["vh1080", "vh720"]);
        assert!(meta.formats[0].label.starts_with("1080p"));
    }

    #[test]
    fn multi_page_unions_heights_from_all_entries() {
        let v = serde_json::json!({
            "id": "BV1xx",
            "title": "合集",
            "webpage_url": "https://www.bilibili.com/video/BV1xx",
            "formats": [],
            "entries": [
                {
                    "id": "BV1xx_p1",
                    "title": "P1",
                    "playlist_index": 1,
                    "formats": [
                        {"format_id": "30032", "height": 480, "fps": 30, "vcodec": "avc1", "tbr": 300.0}
                    ]
                },
                {
                    "id": "BV1xx_p2",
                    "title": "P2",
                    "playlist_index": 2,
                    "formats": [
                        {"format_id": "30112", "height": 1080, "fps": 30, "vcodec": "avc1", "tbr": 2000.0},
                        {"format_id": "30116", "height": 2160, "fps": 60, "vcodec": "avc1", "tbr": 8000.0}
                    ]
                }
            ]
        });
        let meta = video_meta_from_yt_dlp_json(&v).unwrap();
        let ids: Vec<_> = meta.formats.iter().map(|f| f.format_id.as_str()).collect();
        assert_eq!(ids, vec!["vh2160", "vh1080", "vh480"]);
        assert_eq!(meta.formats[0].label, "2160p60");
    }

    #[test]
    fn sample_page_indices_includes_ends_and_respects_cap() {
        assert_eq!(sample_page_indices(3, 8), vec![1, 2, 3]);
        assert_eq!(sample_page_indices(9, 9), (1..=9).collect::<Vec<_>>());
        assert_eq!(sample_page_indices(9, 16), (1..=9).collect::<Vec<_>>());
        let sampled = sample_page_indices(200, 8);
        assert_eq!(sampled.first(), Some(&1));
        assert_eq!(sampled.last(), Some(&200));
        assert!(sampled.len() <= 8);
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
    fn dash_format_selector_merges_bestaudio_with_bestvideo_fallback() {
        assert_eq!(
            dash_format_selector("80"),
            "80+bestaudio/bestvideo+bestaudio/best"
        );
        assert_eq!(
            dash_format_selector("30112"),
            "30112+bestaudio/bestvideo+bestaudio/best"
        );
        assert_eq!(dash_format_selector("80+bestaudio"), "80+bestaudio");
        assert_eq!(dash_format_selector("bestvideo/best"), "bestvideo/best");
    }

    #[test]
    fn dash_format_selector_uses_height_cap_for_vh_prefs() {
        assert_eq!(
            dash_format_selector("vh1080"),
            "bestvideo[height<=1080]+bestaudio/best"
        );
        assert_eq!(
            dash_format_selector("vh720"),
            "bestvideo[height<=720]+bestaudio/best"
        );
        assert_eq!(parse_height_format_id("vh1080"), Some(1080));
        assert_eq!(parse_height_format_id("80"), None);
    }
}
