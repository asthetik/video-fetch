use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobConflict {
    None,
    Active,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct DownloadConflict {
    /// Same video/page/format is pending or running.
    pub downloading: bool,
    /// A completed job record exists for the same video/page/format.
    pub exists: bool,
    /// A matching output file is already on disk (even if the job record was removed).
    pub file_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageItem {
    pub index: u32,
    pub title: String,
    pub page_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatOption {
    pub format_id: String,
    pub label: String,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    /// Approximate total bitrate (kbps) from yt-dlp `tbr`; used for sort/default.
    #[serde(default)]
    pub tbr: Option<f64>,
    /// True when this audio stream is lossless (FLAC). Structured so the UI
    /// doesn't gate FLAC on label text.
    #[serde(default)]
    pub hires: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMeta {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub thumbnail: Option<String>,
    pub webpage_url: String,
    pub pages: Vec<PageItem>,
    pub formats: Vec<FormatOption>,
    /// Audio-only formats for the app's audio-only mode; empty when unavailable.
    #[serde(default)]
    pub audio_formats: Vec<FormatOption>,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceVideoItem {
    pub bvid: String,
    pub title: String,
    pub duration_secs: u64,
    pub play: Option<u64>,
    pub pubdate: i64,
    pub cover: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpacePage {
    pub items: Vec<SpaceVideoItem>,
    pub total: u64,
    pub degraded: bool,
    /// Whether another page exists after this one. The flat-playlist fallback
    /// cannot know the real total, so the UI must key "load more" off this
    /// flag instead of comparing `items.len()` against `total`.
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub name: String,
}

/// Classification of a user-pasted URL; drives the home page's video/space split.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UrlKind {
    Video,
    Space { mid: u64 },
    // Host is space.bilibili.com but the mid segment is missing/non-numeric.
    InvalidSpace,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJob {
    pub id: String,
    pub url: String,
    pub video_id: String,
    pub page_index: u32,
    pub format_id: String,
    /// Some("m4a" | "mp3" | "flac") for audio-only jobs; None = video.
    #[serde(default)]
    pub audio_format: Option<String>,
    pub title: String,
    pub output_template: String,
    pub status: JobStatus,
    pub progress: f64,
    pub error: Option<String>,
    pub output_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    pub save_dir: String,
    pub concurrency: u32,
    pub filename_template: String,
    pub skip_existing: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            concurrency: 1,
            filename_template: "%(title)s [%(id)s].%(ext)s".into(),
            skip_existing: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    LoggedOut,
    LoggedIn,
    PossiblyExpired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CancelAllResult {
    pub cancelled: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClearFinishedResult {
    pub cleared: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BatchEnqueueItem {
    pub bvid: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchEnqueueFailed {
    pub bvid: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BatchEnqueueResult {
    pub enqueued: u32,
    pub skipped_existing: u32,
    pub skipped_active: u32,
    pub failed: Vec<BatchEnqueueFailed>,
}

#[cfg(test)]
mod url_kind_tests {
    use super::*;

    #[test]
    fn url_kind_serializes_tagged() {
        let v = serde_json::to_value(UrlKind::Space { mid: 470995011 }).unwrap();
        assert_eq!(v["kind"], "space");
        assert_eq!(v["mid"], 470995011);
        assert_eq!(
            serde_json::to_value(UrlKind::Video).unwrap()["kind"],
            "video"
        );
        assert_eq!(
            serde_json::to_value(UrlKind::Unknown).unwrap()["kind"],
            "unknown"
        );
    }
}
