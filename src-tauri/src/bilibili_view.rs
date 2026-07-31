use serde_json::Value;

use crate::error::{AppError, AppResult};
use crate::models::{PageItem, VideoMeta};
use crate::platform::canonicalize_video_url;
use crate::resolve_cache;
use crate::ytdlp;

const VIEW_URL: &str = "https://api.bilibili.com/x/web-interface/view";
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const REFERER: &str = "https://www.bilibili.com/";
const VIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// Fetch title / pages / thumbnail from Bilibili view API (no WBI).
///
/// Caller should pass an already-canonicalized URL when possible.
pub async fn resolve_view(url: &str) -> AppResult<VideoMeta> {
    let url = canonicalize_video_url(url);
    let bvid = resolve_cache::extract_bilibili_id(&url)
        .ok_or_else(|| AppError::Message("无法从链接解析 BV 号，跳过 view 快路径".into()))?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(VIEW_TIMEOUT)
        .timeout(VIEW_TIMEOUT)
        .build()
        .map_err(|e| AppError::Message(format!("创建 HTTP 客户端失败: {e}")))?;

    let resp = client
        .get(VIEW_URL)
        .query(&[("bvid", bvid.as_str())])
        .header("Referer", REFERER)
        .send()
        .await
        .map_err(|e| AppError::Message(format!("view API 请求失败: {e}")))?;

    if !resp.status().is_success() {
        return Err(AppError::Message(format!(
            "view API HTTP {}",
            resp.status()
        )));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Message(format!("view API JSON 解析失败: {e}")))?;

    let code = body.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    if code != 0 {
        let message = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        return Err(AppError::Message(format!(
            "view API code {code}: {message}"
        )));
    }

    let data = body
        .get("data")
        .ok_or_else(|| AppError::Message("view API 无 data".into()))?;
    map_view_json(data, &url)
}

/// Map view JSON `data` object into VideoMeta with empty formats.
pub fn map_view_json(data: &Value, webpage_url: &str) -> AppResult<VideoMeta> {
    let id = data
        .get("bvid")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Message("view API 缺少 bvid".into()))?
        .to_string();
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(id.as_str())
        .to_string();
    let uploader = data
        .pointer("/owner/name")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let thumbnail = data.get("pic").and_then(|v| v.as_str()).map(str::to_string);
    let pages_val = data
        .get("pages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::Message("view API 无分 P".into()))?;
    if pages_val.is_empty() {
        return Err(AppError::Message("view API 无分 P".into()));
    }

    let pages: Vec<PageItem> = pages_val
        .iter()
        .enumerate()
        .filter_map(|(i, p)| {
            let cid = p.get("cid")?.as_u64()?;
            let index = p
                .get("page")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32)
                .unwrap_or((i + 1) as u32);
            let part = p
                .get("part")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("P{index}"));
            Some(PageItem {
                index,
                title: part,
                page_id: cid.to_string(),
            })
        })
        .collect();

    if pages.is_empty() {
        return Err(AppError::Message("view API 分 P 无效".into()));
    }

    Ok(VideoMeta {
        id,
        title,
        uploader,
        thumbnail,
        webpage_url: webpage_url.to_string(),
        pages,
        formats: Vec::new(),
        platform: "bilibili".into(),
    })
}

/// Prefer view title/pages/thumb; take formats from yt-dlp; re-finalize for page count.
pub fn merge_view_with_formats(view: VideoMeta, ytdlp_meta: VideoMeta) -> VideoMeta {
    let mut merged = VideoMeta {
        id: if view.id.is_empty() {
            ytdlp_meta.id
        } else {
            view.id
        },
        title: if view.title.is_empty() {
            ytdlp_meta.title
        } else {
            view.title
        },
        uploader: view.uploader.or(ytdlp_meta.uploader),
        thumbnail: view.thumbnail.or(ytdlp_meta.thumbnail),
        webpage_url: if view.webpage_url.is_empty() {
            ytdlp_meta.webpage_url
        } else {
            view.webpage_url
        },
        pages: if view.pages.is_empty() {
            ytdlp_meta.pages
        } else {
            view.pages
        },
        formats: ytdlp_meta.formats,
        platform: if view.platform.is_empty() {
            ytdlp_meta.platform
        } else {
            view.platform
        },
    };
    let page_count = merged.pages.len();
    merged.formats =
        ytdlp::finalize_formats_for_pages(std::mem::take(&mut merged.formats), page_count);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_view_json_multi_page() {
        let data = json!({
            "bvid": "BV1xx411c7mD",
            "title": "Test Video",
            "pic": "https://example.com/t.jpg",
            "owner": { "name": "UP" },
            "pages": [
                { "cid": 111, "page": 1, "part": "Intro" },
                { "cid": 222, "page": 2, "part": "Main" }
            ]
        });
        let meta = map_view_json(&data, "https://www.bilibili.com/video/BV1xx411c7mD").unwrap();
        assert_eq!(meta.id, "BV1xx411c7mD");
        assert_eq!(meta.title, "Test Video");
        assert_eq!(meta.uploader.as_deref(), Some("UP"));
        assert_eq!(meta.pages.len(), 2);
        assert_eq!(meta.pages[1].page_id, "222");
        assert_eq!(meta.pages[1].title, "Main");
        assert!(meta.formats.is_empty());
    }

    #[test]
    fn merge_prefers_view_and_finalizes_multi_page_heights() {
        let view = VideoMeta {
            id: "BV1".into(),
            title: "From View".into(),
            uploader: Some("V".into()),
            thumbnail: Some("v.jpg".into()),
            webpage_url: "https://www.bilibili.com/video/BV1".into(),
            pages: vec![
                PageItem {
                    index: 1,
                    title: "P1".into(),
                    page_id: "cid1".into(),
                },
                PageItem {
                    index: 2,
                    title: "P2".into(),
                    page_id: "cid2".into(),
                },
            ],
            formats: vec![],
            platform: "bilibili".into(),
        };
        let ytdlp_meta = VideoMeta {
            id: "BV1".into(),
            title: "From Yt".into(),
            uploader: Some("Y".into()),
            thumbnail: None,
            webpage_url: "https://www.bilibili.com/video/BV1".into(),
            pages: vec![PageItem {
                index: 1,
                title: "Other".into(),
                page_id: "x".into(),
            }],
            formats: vec![
                crate::models::FormatOption {
                    format_id: "80".into(),
                    label: "1080p".into(),
                    height: Some(1080),
                    fps: Some(30),
                    tbr: Some(1000.0),
                    requires_login: false,
                },
                crate::models::FormatOption {
                    format_id: "64".into(),
                    label: "720p".into(),
                    height: Some(720),
                    fps: None,
                    tbr: Some(800.0),
                    requires_login: false,
                },
            ],
            platform: "bilibili".into(),
        };
        let merged = merge_view_with_formats(view, ytdlp_meta);
        assert_eq!(merged.title, "From View");
        assert_eq!(merged.pages.len(), 2);
        assert_eq!(merged.pages[0].title, "P1");
        assert_eq!(merged.formats.len(), 2);
        assert_eq!(merged.formats[0].format_id, "vh1080");
        assert_eq!(merged.formats[1].format_id, "vh720");
    }
}
