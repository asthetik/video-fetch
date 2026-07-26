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
    pub requires_login: bool,
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
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadJob {
    pub id: String,
    pub url: String,
    pub video_id: String,
    pub page_index: u32,
    pub format_id: String,
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
    /// Kept for settings.json compatibility; UI always picks highest listed format.
    pub default_format_preference: String,
    pub concurrency: u32,
    pub filename_template: String,
    pub prefer_bundled_tools: bool,
    pub skip_existing: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            save_dir: String::new(),
            default_format_preference: "best".into(),
            concurrency: 1,
            filename_template: "%(title)s [%(id)s].%(ext)s".into(),
            prefer_bundled_tools: true,
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
