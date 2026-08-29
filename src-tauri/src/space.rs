use crate::error::{AppError, AppResult};
use crate::models::{SpacePage, SpaceVideoItem};
use crate::wbi::{self, WbiKeys};
use serde_json::Value;

pub const SPACE_ARC_SEARCH_URL: &str = "https://api.bilibili.com/x/space/wbi/arc/search";
pub const SPACE_ACC_INFO_URL: &str = "https://api.bilibili.com/x/space/wbi/acc/info";
pub const SPACE_PAGE_SIZE: u32 = 50;

/// arc/search sort parameter; the fallback mode only supports the default Pubdate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceOrder {
    Pubdate,
    Click,
}

impl SpaceOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            SpaceOrder::Pubdate => "pubdate",
            SpaceOrder::Click => "click",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pubdate" => Some(SpaceOrder::Pubdate),
            "click" => Some(SpaceOrder::Click),
            _ => None,
        }
    }
}

pub fn signed_params(mid: u64, pn: u32, keyword: &str, order: SpaceOrder) -> Vec<(String, String)> {
    let keyword = keyword.trim();
    let mut params = vec![
        ("mid".to_string(), mid.to_string()),
        ("ps".to_string(), SPACE_PAGE_SIZE.to_string()),
        ("tid".to_string(), "0".to_string()),
        ("pn".to_string(), pn.to_string()),
        ("order".to_string(), order.as_str().to_string()),
    ];
    if !keyword.is_empty() {
        params.push(("keyword".to_string(), keyword.to_string()));
    }
    params
}

/// GET arc/search with wbi signature. Any non-zero `code` (incl. -412 risk
/// control) becomes Err so the caller can decide on the yt-dlp fallback.
pub async fn fetch_arc_search(
    client: &reqwest::Client,
    keys: &WbiKeys,
    mid: u64,
    pn: u32,
    keyword: &str,
    order: SpaceOrder,
    cookie_header: Option<&str>,
) -> AppResult<SpacePage> {
    let params = signed_params(mid, pn, keyword, order);
    let (wts, w_rid) = wbi::sign(keys, &params);
    let mut req = client
        .get(SPACE_ARC_SEARCH_URL)
        .header("Referer", format!("https://space.bilibili.com/{mid}/"))
        .query(&params)
        .query(&[("wts", wts.as_str()), ("w_rid", w_rid.as_str())]);
    if let Some(c) = cookie_header {
        req = req.header("Cookie", c);
    }
    let body: Value = req
        .send()
        .await
        .map_err(|e| AppError::Message(format!("空间列表请求失败: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Message(format!("空间列表 JSON 解析失败: {e}")))?;
    let mut page = parse_arc_search(&body)?;
    page.has_more = u64::from(pn) * u64::from(SPACE_PAGE_SIZE) < page.total;
    Ok(page)
}

/// Non-zero `code` (incl. risk-control -412) -> Err; the caller decides on fallback.
pub fn parse_arc_search(v: &Value) -> AppResult<SpacePage> {
    let code = v.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let msg = v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        // Keep the numeric code (e.g. -412 risk control) in the message for triage.
        return Err(AppError::Message(format!("空间列表 code={code}: {msg}")));
    }
    let data = v
        .get("data")
        .ok_or_else(|| AppError::Message("空间列表响应缺少 data".into()))?;
    let total = data
        .pointer("/page/count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut items = Vec::new();
    if let Some(list) = data.pointer("/list/vlist").and_then(Value::as_array) {
        for entry in list {
            let Some(bvid) = entry
                .get("bvid")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            items.push(SpaceVideoItem {
                bvid: bvid.to_string(),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(bvid)
                    .to_string(),
                duration_secs: parse_length(
                    entry.get("length").and_then(Value::as_str).unwrap_or(""),
                ),
                play: entry.get("play").and_then(Value::as_u64),
                pubdate: entry.get("pubdate").and_then(Value::as_i64).unwrap_or(0),
                cover: entry
                    .get("pic")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(normalize_cover),
            });
        }
    }
    Ok(SpacePage {
        items,
        total,
        degraded: false,
        has_more: false,
    })
}

/// acc/info's `data.name`; code != 0 -> Err (the command layer maps it to an empty-name default).
pub fn parse_acc_info(v: &Value) -> AppResult<String> {
    let code = v.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        let msg = v
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(AppError::Message(format!("空间信息 code={code}: {msg}")));
    }
    Ok(v.pointer("/data/name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string())
}

/// `length` is "mm:ss" or "hh:mm:ss"; a parse failure -> 0 (the frontend renders 0 as a dash).
fn parse_length(s: &str) -> u64 {
    let mut secs: u64 = 0;
    for part in s.split(':') {
        let n: u64 = part.parse().unwrap_or(0);
        secs = secs * 60 + n;
    }
    secs
}

/// Bilibili covers are often protocol-relative (`//i0.hdslb.com/...`); complete to https.
fn normalize_cover(pic: &str) -> String {
    if let Some(rest) = pic.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        pic.to_string()
    }
}

/// Bvid shape: "BV" + 10 ASCII alphanumerics. Batch items cross the IPC
/// boundary; an invalid bvid becomes a per-item failure instead of a garbage URL.
pub fn is_valid_bvid(bvid: &str) -> bool {
    let Some(rest) = bvid.strip_prefix("BV") else {
        return false;
    };
    rest.len() == 10 && rest.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// yt-dlp flat-playlist fallback. `start`/`end` are 1-based inclusive bounds
/// for the requested page. The flat playlist cannot filter or sort, so the
/// caller must gate this behind [`fallback_allowed`].
pub async fn fetch_via_ytdlp(
    ytdlp_path: &std::path::Path,
    mid: u64,
    start: u64,
    end: u64,
    cookies_path: Option<&std::path::Path>,
) -> AppResult<SpacePage> {
    let mut cmd = crate::ytdlp::base_command(ytdlp_path, cookies_path);
    cmd.arg("--flat-playlist")
        .arg("-J")
        .arg("--playlist-items")
        .arg(format!("{start}:{end}"))
        .arg(format!("https://space.bilibili.com/{mid}/video"));
    let out = cmd
        .output()
        .await
        .map_err(|e| AppError::Message(format!("无法启动 yt-dlp: {e}")))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr);
        return Err(AppError::Message(format!(
            "yt-dlp 空间列表获取失败: {}",
            detail.trim()
        )));
    }
    let json: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| AppError::Message(format!("yt-dlp 空间列表 JSON 解析失败: {e}")))?;
    parse_flat_playlist(&json, start, end)
}

/// Flat playlists have no filter/sort support: play count is unavailable,
/// and missing date/duration stay 0 so the UI renders them as "—".
///
/// With `--playlist-items` slicing yt-dlp omits `playlist_count`, so the real
/// total is unknowable: a full page reports the window end as a floor and
/// `has_more` assumes more exist; a short page means the playlist ended.
pub fn parse_flat_playlist(v: &Value, start: u64, end: u64) -> AppResult<SpacePage> {
    let page_size = end - start + 1;
    let mut items = Vec::new();
    if let Some(entries) = v.get("entries").and_then(Value::as_array) {
        for entry in entries {
            let Some(bvid) = entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            items.push(SpaceVideoItem {
                bvid: bvid.to_string(),
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(bvid)
                    .to_string(),
                duration_secs: entry.get("duration").and_then(Value::as_u64).unwrap_or(0),
                play: None,
                pubdate: entry.get("timestamp").and_then(Value::as_i64).unwrap_or(0),
                cover: entry
                    .get("thumbnails")
                    .and_then(Value::as_array)
                    .and_then(|ts| ts.last())
                    .and_then(|t| t.get("url"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string()),
            });
        }
    }
    let known_total = v.get("playlist_count").and_then(Value::as_u64);
    let full_page = items.len() as u64 >= page_size;
    let total = known_total.unwrap_or(if full_page {
        end
    } else {
        start - 1 + items.len() as u64
    });
    // Without a known count the playlist continues iff the window came back full.
    let has_more = match known_total {
        Some(count) => end < count,
        None => full_page,
    };
    Ok(SpacePage {
        items,
        total,
        degraded: true,
        has_more,
    })
}

/// The fallback only serves the default view (no keyword, default sort) — a flat
/// playlist cannot filter or sort, so anything else must fail explicitly instead of
/// silently returning wrong data.
pub fn fallback_allowed(keyword: &str, order: SpaceOrder) -> bool {
    keyword.trim().is_empty() && order == SpaceOrder::Pubdate
}

/// The rejection message must name the remedy that actually applies: an empty
/// keyword means the sort toggle (only Pubdate degrades) is what blocks the
/// fallback; otherwise it is the search term.
pub fn degraded_reject_error(keyword: &str) -> AppError {
    if keyword.trim().is_empty() {
        AppError::Message(
            "空间列表接口暂不可用，降级模式不支持排序，请切回「最新发布」后重试".into(),
        )
    } else {
        AppError::Message("空间列表接口暂不可用，降级模式不支持搜索，请清除搜索条件后重试".into())
    }
}

/// GET acc/info and return `data.name`; a non-zero `code` becomes Err (the
/// caller maps it to the empty-name UI default).
pub async fn fetch_acc_info(
    client: &reqwest::Client,
    keys: &WbiKeys,
    mid: u64,
    cookie_header: Option<&str>,
) -> AppResult<String> {
    let params = vec![("mid".to_string(), mid.to_string())];
    let (wts, w_rid) = wbi::sign(keys, &params);
    let mut req = client
        .get(SPACE_ACC_INFO_URL)
        .header("Referer", format!("https://space.bilibili.com/{mid}/"))
        .query(&params)
        .query(&[("wts", wts.as_str()), ("w_rid", w_rid.as_str())]);
    if let Some(c) = cookie_header {
        req = req.header("Cookie", c);
    }
    let body: Value = req
        .send()
        .await
        .map_err(|e| AppError::Message(format!("空间信息请求失败: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Message(format!("空间信息 JSON 解析失败: {e}")))?;
    parse_acc_info(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn space_order_parse_and_str() {
        assert_eq!(SpaceOrder::parse("pubdate"), Some(SpaceOrder::Pubdate));
        assert_eq!(SpaceOrder::parse("click"), Some(SpaceOrder::Click));
        assert_eq!(SpaceOrder::parse("nope"), None);
        assert_eq!(SpaceOrder::Pubdate.as_str(), "pubdate");
    }

    #[test]
    fn signed_params_shape() {
        let params = signed_params(470995011, 2, "刑法", SpaceOrder::Click);
        assert!(params.contains(&("mid".into(), "470995011".into())));
        assert!(params.contains(&("ps".into(), "50".into())));
        assert!(params.contains(&("pn".into(), "2".into())));
        assert!(params.contains(&("order".into(), "click".into())));
        assert!(params.contains(&("keyword".into(), "刑法".into())));
        let no_kw = signed_params(1, 1, "  ", SpaceOrder::Pubdate);
        assert!(!no_kw.iter().any(|(k, _)| k == "keyword"));
    }

    #[test]
    fn parse_arc_search_normalizes_items() {
        let body = json!({
            "code": 0,
            "data": {
                "page": { "count": 2 },
                "list": { "vlist": [
                    {
                        "bvid": "BV1xx411c7mD", "title": "刑法第1讲",
                        "length": "05:12", "play": 32000,
                        "pubdate": 1700000000, "pic": "//i0.hdslb.com/a.jpg"
                    },
                    {
                        "bvid": "BV1yy411c7mE", "title": "刑法第2讲",
                        "length": "1:05:12", "play": 0, "pubdate": 0, "pic": ""
                    }
                ]}
            }
        });
        let page = parse_arc_search(&body).unwrap();
        assert!(!page.degraded);
        assert_eq!(page.total, 2);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].duration_secs, 312);
        assert_eq!(page.items[0].play, Some(32000));
        assert_eq!(
            page.items[0].cover.as_deref(),
            Some("https://i0.hdslb.com/a.jpg")
        );
        assert_eq!(page.items[1].duration_secs, 3912);
        assert_eq!(page.items[1].play, Some(0));
        assert_eq!(page.items[1].cover, None);
    }

    #[test]
    fn parse_arc_search_rejects_nonzero_code() {
        let body = json!({ "code": -412, "message": "请求被拦截" });
        let err = parse_arc_search(&body).unwrap_err().to_string();
        assert!(
            err.contains("-412"),
            "risk-control code must surface: {err}"
        );
    }

    #[test]
    fn parse_acc_info_extracts_name() {
        let body = json!({ "code": 0, "data": { "name": "蔡雅奇刑法" } });
        assert_eq!(parse_acc_info(&body).unwrap(), "蔡雅奇刑法");
        let bad = json!({ "code": -400, "message": "啥都木有" });
        assert!(parse_acc_info(&bad).is_err());
    }

    #[test]
    fn parse_flat_playlist_marks_degraded() {
        let body = json!({
            "playlist_count": 1200,
            "entries": [
                { "id": "BV1xx411c7mD", "title": "刑法第1讲", "duration": 312,
                  "timestamp": 1700000000,
                  "thumbnails": [ { "url": "https://i0.hdslb.com/s.jpg" } ] },
                { "id": "BV1yy411c7mE", "title": "刑法第2讲" }
            ]
        });
        let page = parse_flat_playlist(&body, 1, 50).unwrap();
        assert!(page.degraded);
        assert!(page.has_more);
        assert_eq!(page.total, 1200);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].play, None);
        assert_eq!(page.items[0].duration_secs, 312);
        assert_eq!(page.items[0].pubdate, 1700000000);
        assert_eq!(page.items[1].duration_secs, 0);
        assert_eq!(page.items[1].pubdate, 0);
        assert_eq!(page.items[1].cover, None);
    }

    #[test]
    fn parse_flat_playlist_without_count_keeps_paging_while_full() {
        let full_page = json!({ "entries": [
            { "id": "BV1xx411c7mD", "title": "a" },
            { "id": "BV1yy411c7mE", "title": "b" },
        ]});
        let page = parse_flat_playlist(&full_page, 51, 52).unwrap();
        assert_eq!(page.total, 52);
        assert!(page.has_more);

        let short_page = json!({ "entries": [
            { "id": "BV1zz411c7mF", "title": "c" },
        ]});
        let page = parse_flat_playlist(&short_page, 53, 54).unwrap();
        assert_eq!(page.total, 53);
        assert!(!page.has_more);
    }

    #[test]
    fn is_valid_bvid_shape() {
        assert!(is_valid_bvid("BV1xx411c7mD"));
        assert!(!is_valid_bvid("BV1xx411c7m"));
        assert!(!is_valid_bvid("BV1xx411c7mDX"));
        assert!(!is_valid_bvid("av170001"));
        assert!(!is_valid_bvid("BV../../evil"));
        assert!(!is_valid_bvid(""));
    }

    #[test]
    fn fallback_only_serves_default_view() {
        assert!(fallback_allowed("", SpaceOrder::Pubdate));
        assert!(!fallback_allowed("刑法", SpaceOrder::Pubdate));
        assert!(!fallback_allowed("", SpaceOrder::Click));
    }

    #[test]
    fn degraded_reject_error_names_the_matching_remedy() {
        assert!(
            degraded_reject_error("刑法")
                .to_string()
                .contains("清除搜索条件")
        );
        assert!(
            degraded_reject_error("  ")
                .to_string()
                .contains("切回「最新发布」")
        );
    }
}
