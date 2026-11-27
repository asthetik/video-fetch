use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Paused,
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
    /// Same video/page is pending, running or paused (paused still blocks duplicates).
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
    /// Source duration in seconds from resolve; stored on jobs for history.
    #[serde(default)]
    pub duration_secs: Option<u64>,
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
    /// Uploader name captured at enqueue time; the filename prediction for
    /// `%(uploader)s` templates must use the same value as the conflict check.
    #[serde(default)]
    pub uploader: String,
    pub output_template: String,
    pub status: JobStatus,
    pub progress: f64,
    pub error: Option<String>,
    pub output_path: Option<String>,
    /// Cover image URL captured at enqueue time (None for legacy rows).
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    /// Source duration in seconds captured at enqueue time.
    #[serde(default)]
    pub duration_secs: Option<u64>,
    /// Output file size in bytes, filled when the job finishes.
    #[serde(default)]
    pub file_size: Option<u64>,
    /// Row creation time (SQLite UTC datetime), exposed for history grouping.
    #[serde(default)]
    pub created_at: Option<String>,
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
            concurrency: 3,
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
pub struct PauseAllResult {
    pub paused: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResumeAllResult {
    pub resumed: u32,
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
    /// Cover image URL stored with the job for the history page.
    #[serde(default)]
    pub cover: Option<String>,
    /// Source duration in seconds stored with the job for the history page.
    #[serde(default)]
    pub duration_secs: Option<u64>,
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

#[cfg(test)]
mod wire_contract_tests {
    use super::*;
    use crate::activity_log::LogFileInfo;
    use crate::commands::{EngineVersions, ResolveMetaEvent};

    /// B7 wire contract: every wire-facing type is serialized here and
    /// compared against the golden fixture at src/types.contract.json — the
    /// same fixture the TS-side contract test (src/typesContract.test.ts)
    /// validates the TS mirrors against. Covers the snake_case domain
    /// structs, the camelCase command-result/event shapes (the
    /// `rename_all = "camelCase"` family — exactly the manual-sync drift
    /// class), and the string enums' value sets. A field rename, a new
    /// field, or a dropped field on either side turns one of the tests red.
    /// Update the fixture deliberately if the wire format genuinely
    /// changed.
    fn contract() -> serde_json::Value {
        serde_json::from_str(include_str!("../../src/types.contract.json"))
            .expect("src/types.contract.json must be valid JSON")
    }

    fn assert_matches_fixture<T: Serialize>(key: &str, sample: &T) {
        let actual = serde_json::to_value(sample).expect("wire sample must serialize");
        assert_eq!(
            actual,
            contract()[key],
            "`{key}` wire shape drifted from src/types.contract.json; \
             update the fixture deliberately if the wire format changed"
        );
    }

    #[test]
    fn wire_contracts_match_the_golden_fixture() {
        let page = PageItem {
            index: 1,
            title: "第一分P".into(),
            page_id: "p1".into(),
        };
        let format = FormatOption {
            format_id: "80".into(),
            label: "1080P 高清".into(),
            height: Some(1080),
            fps: Some(30),
            tbr: Some(2000.5),
            hires: false,
        };
        let fmts = vec![format.clone()];
        assert_matches_fixture("PageItem", &page);
        assert_matches_fixture("FormatOption", &format);

        let meta = VideoMeta {
            id: "BV1xx411c7mD".into(),
            title: "示例视频标题".into(),
            uploader: Some("示例UP主".into()),
            thumbnail: Some("https://i0.hdslb.com/bfs/archive/sample.jpg".into()),
            duration_secs: Some(300),
            webpage_url: "https://www.bilibili.com/video/BV1xx411c7mD".into(),
            pages: vec![page],
            formats: fmts.clone(),
            audio_formats: Vec::new(),
            platform: "bilibili".into(),
        };
        assert_matches_fixture("VideoMeta", &meta);

        let job = DownloadJob {
            id: "job-1764000000000000000-1".into(),
            url: "https://www.bilibili.com/video/BV1xx411c7mD".into(),
            video_id: "BV1xx411c7mD".into(),
            page_index: 1,
            format_id: "80".into(),
            audio_format: None,
            title: "示例视频标题".into(),
            uploader: "示例UP主".into(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            status: JobStatus::Done,
            progress: 1.0,
            error: None,
            output_path: Some("/downloads/示例视频标题 [BV1xx411c7mD].mp4".into()),
            thumbnail_url: Some("https://i0.hdslb.com/bfs/archive/sample.jpg".into()),
            duration_secs: Some(300),
            file_size: Some(104857600),
            created_at: Some("2026-01-01 00:00:00".into()),
        };
        assert_matches_fixture("DownloadJob", &job);

        assert_matches_fixture(
            "AppSettings",
            &AppSettings {
                save_dir: "/downloads".into(),
                concurrency: 2,
                filename_template: "%(title)s [%(id)s].%(ext)s".into(),
                skip_existing: true,
            },
        );
        assert_matches_fixture(
            "DownloadConflict",
            &DownloadConflict {
                downloading: true,
                exists: false,
                file_exists: false,
            },
        );

        let space_item = SpaceVideoItem {
            bvid: "BV1xx411c7mD".into(),
            title: "空间视频".into(),
            duration_secs: 300,
            play: Some(12345),
            pubdate: 1700000000,
            cover: Some("https://i0.hdslb.com/bfs/archive/space.jpg".into()),
        };
        assert_matches_fixture("SpaceVideoItem", &space_item);
        assert_matches_fixture(
            "SpacePage",
            &SpacePage {
                items: vec![space_item],
                total: 10,
                degraded: false,
                has_more: true,
            },
        );
        assert_matches_fixture(
            "SpaceInfo",
            &SpaceInfo {
                name: "某UP主".into(),
            },
        );

        // Tagged enum: each variant's `kind` literal must match the fixture
        // entries (the TS union mirrors them one-for-one).
        let kinds = vec![
            serde_json::to_value(UrlKind::Video).unwrap(),
            serde_json::to_value(UrlKind::Space { mid: 12345 }).unwrap(),
            serde_json::to_value(UrlKind::InvalidSpace).unwrap(),
            serde_json::to_value(UrlKind::Unknown).unwrap(),
        ];
        let fixture_kinds = contract()["UrlKind"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            serde_json::Value::Array(kinds),
            serde_json::Value::Array(fixture_kinds),
            "UrlKind variants drifted from the fixture"
        );
    }

    #[test]
    fn camelcase_command_shapes_match_the_golden_fixture() {
        assert_matches_fixture(
            "EngineVersions",
            &EngineVersions {
                yt_dlp: "2026.11.23".into(),
                ffmpeg: "9.0.2".into(),
            },
        );
        assert_matches_fixture(
            "CancelAllResult",
            &CancelAllResult {
                cancelled: 2,
                errors: vec!["一个失败原因".into()],
            },
        );
        assert_matches_fixture(
            "PauseAllResult",
            &PauseAllResult {
                paused: 2,
                errors: vec!["一个失败原因".into()],
            },
        );
        assert_matches_fixture(
            "ResumeAllResult",
            &ResumeAllResult {
                resumed: 2,
                errors: vec!["一个失败原因".into()],
            },
        );
        assert_matches_fixture(
            "BatchEnqueueResult",
            &BatchEnqueueResult {
                enqueued: 3,
                skipped_existing: 1,
                skipped_active: 0,
                failed: vec![BatchEnqueueFailed {
                    bvid: "BV1bad".into(),
                    error: "无效的视频 ID".into(),
                }],
            },
        );
    }

    #[test]
    fn event_and_enum_shapes_match_the_golden_fixture() {
        assert_matches_fixture(
            "ResolveMetaEvent",
            &ResolveMetaEvent {
                request_id: 7,
                meta: VideoMeta {
                    id: "BV1xx411c7mD".into(),
                    title: "示例视频标题".into(),
                    uploader: Some("示例UP主".into()),
                    thumbnail: Some("https://i0.hdslb.com/bfs/archive/sample.jpg".into()),
                    duration_secs: Some(300),
                    webpage_url: "https://www.bilibili.com/video/BV1xx411c7mD".into(),
                    pages: Vec::new(),
                    formats: Vec::new(),
                    audio_formats: Vec::new(),
                    platform: "bilibili".into(),
                },
            },
        );
        assert_matches_fixture(
            "LogFileInfo",
            &LogFileInfo {
                name: "app.log".into(),
                size: 104857600,
                modified_secs: 1764000000,
            },
        );

        // String enums: the value set itself is part of the wire contract.
        let status_values: Vec<serde_json::Value> = [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::Done,
            JobStatus::Failed,
            JobStatus::Paused,
        ]
        .iter()
        .map(|v| serde_json::to_value(v).unwrap())
        .collect();
        assert_eq!(
            status_values,
            contract()["JobStatus"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            "JobStatus value set drifted from the fixture"
        );
        let auth_values: Vec<serde_json::Value> = [
            AuthStatus::LoggedOut,
            AuthStatus::LoggedIn,
            AuthStatus::PossiblyExpired,
        ]
        .iter()
        .map(|v| serde_json::to_value(v).unwrap())
        .collect();
        assert_eq!(
            auth_values,
            contract()["AuthStatus"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            "AuthStatus value set drifted from the fixture"
        );
    }
}
