use std::collections::HashMap;
use std::sync::Mutex;

use url::Url;

use crate::models::{self, UrlKind, VideoMeta};
use crate::{bilibili_view, playurl, resolve_cache, wbi};

use super::{FastResolve, ResolveCtx, VideoPlatform};

/// Cap on concurrent playurl requests during multi-P sampling so bursts of API
/// calls don't trip Bilibili risk control.
const PLAYURL_SAMPLE_CONCURRENCY: usize = 4;

/// The bilibili implementation of the resolve orchestration seam. Bilibili-
/// owned state — the wbi key cache and the per-mid space-name cache — lives
/// here, not in the generic orchestrator context (B21).
pub struct BilibiliPlatform {
    pub(crate) wbi_keys: wbi::WbiKeyCache,
    pub(crate) space_info_cache: Mutex<HashMap<u64, (String, std::time::Instant)>>,
}

impl BilibiliPlatform {
    pub fn new() -> Self {
        Self {
            wbi_keys: wbi::WbiKeyCache::new(),
            space_info_cache: Mutex::new(HashMap::new()),
        }
    }

    /// The wbi cache for the space commands (a bilibili capability, not part
    /// of the generic seam).
    pub fn wbi(&self) -> &wbi::WbiKeyCache {
        &self.wbi_keys
    }

    /// Cached space display name when one was read inside the TTL window.
    pub fn cached_space_name(&self, mid: u64, ttl: std::time::Duration) -> Option<String> {
        let cache = self.space_info_cache.lock().ok()?;
        let (name, at) = cache.get(&mid)?;
        (at.elapsed() < ttl).then(|| name.clone())
    }

    /// Insert into the per-mid space-name cache, evicting the oldest entry
    /// when the cap is reached. The cache is keyed by visited space mid and
    /// otherwise only ever grows; entries carry their read time, so the
    /// oldest is the natural eviction victim. Updating an existing mid
    /// refreshes in place without evicting anyone. `now` is a parameter so
    /// the eviction order is deterministically testable (the sweep's
    /// now-seam precedent).
    pub fn remember_space_name(&self, mid: u64, name: &str, now: std::time::Instant) {
        const CAP: usize = 64;
        if let Ok(mut cache) = self.space_info_cache.lock() {
            if cache.len() >= CAP
                && !cache.contains_key(&mid)
                && let Some(oldest) = cache
                    .iter()
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(mid, _)| *mid)
            {
                cache.remove(&oldest);
            }
            cache.insert(mid, (name.to_string(), now));
        }
    }
}

impl Default for BilibiliPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl VideoPlatform for BilibiliPlatform {
    fn classify_url(&self, url: &str) -> UrlKind {
        if let Some(mid) = parse_space_mid(url) {
            return UrlKind::Space { mid };
        }
        if let Ok(parsed) = Url::parse(url.trim())
            && parsed.host_str() == Some("space.bilibili.com")
        {
            return UrlKind::InvalidSpace;
        }
        let canonical = canonicalize_video_url(url);
        if crate::platform::detect_platform(&canonical).is_some() {
            UrlKind::Video
        } else {
            UrlKind::Unknown
        }
    }

    fn cache_scope(&self, has_cookies: bool) -> &'static str {
        resolve_cache::cache_scope(has_cookies)
    }

    fn cache_lookup_keys(&self, url: &str, scope: &str) -> Vec<String> {
        resolve_cache::lookup_cache_keys(url, scope)
    }

    fn cache_store_keys(&self, url: &str, video_id: &str, scope: &str) -> Vec<String> {
        resolve_cache::store_cache_keys(url, video_id, scope)
    }

    fn merge_fallback(&self, partial: VideoMeta, fallback: VideoMeta) -> VideoMeta {
        bilibili_view::merge_view_with_formats(partial, fallback)
    }

    fn log_subject(&self, url: &str) -> Option<String> {
        resolve_cache::extract_bilibili_id(url)
    }

    async fn fast_metadata(
        &self,
        ctx: &ResolveCtx<'_>,
        url: &str,
        emit_partial: &(dyn Fn(VideoMeta) -> bool + Send + Sync),
        emit_formats_progress: &(dyn Fn(VideoMeta) + Send + Sync),
    ) -> Option<FastResolve> {
        // Fast path: single-part videos resolve via view -> playurl; failures
        // fall back to yt-dlp (orchestrator-driven). The gate on emit_partial
        // is the pre-B21 `Ok(partial) if is_current()` arm: superseded here
        // means the playurl block never runs.
        let view = match bilibili_view::resolve_view(url, ctx.client).await {
            Ok(partial) => {
                if !emit_partial(partial.clone()) {
                    return None;
                }
                partial
            }
            Err(_) => return None,
        };
        let bvid = resolve_cache::extract_bilibili_id(url);
        let keys = match self
            .wbi_keys
            .get_or_fetch(ctx.client, ctx.cookie_header)
            .await
        {
            Ok(keys) => Some(keys),
            Err(e) => {
                tracing::warn!(target: "core", "resolve: wbi keys 失败，回退 yt-dlp: {e}");
                None
            }
        };
        let formats = match (bvid, keys) {
            (Some(bvid), Some(keys)) if view.pages.len() == 1 => {
                let cid = view.pages[0].page_id.clone();
                match playurl::fetch_formats(
                    ctx.client,
                    &keys,
                    &bvid,
                    &cid,
                    ctx.cookie_header,
                    false,
                )
                .await
                {
                    Ok((video, audio)) => Some((video, audio)),
                    Err(e) => {
                        tracing::warn!(target: "core", "resolve: playurl 失败，回退 yt-dlp: {e}");
                        self.wbi_keys.invalidate();
                        None
                    }
                }
            }
            (Some(bvid), Some(keys)) => {
                let result = multi_page_playurl_formats(
                    ctx.client,
                    &keys,
                    &bvid,
                    &view.pages,
                    ctx.cookie_header,
                    |video, audio| {
                        emit_formats_progress(models::VideoMeta {
                            formats: video,
                            audio_formats: audio,
                            ..view.clone()
                        });
                    },
                )
                .await;
                if result.is_none() {
                    self.wbi_keys.invalidate();
                }
                result
            }
            _ => None,
        };
        Some(FastResolve {
            meta: view,
            formats,
        })
    }
}

/// Multi-page sampling: query all pages when <=16 parts, otherwise 8 sampled
/// pages, at most `PLAYURL_SAMPLE_CONCURRENCY` at a time. Each completed page
/// invokes `on_progress` with the cumulative result in completion order (the
/// frontend replaces the whole list, so it stays idempotent).
pub(crate) async fn multi_page_playurl_formats(
    client: &reqwest::Client,
    keys: &wbi::WbiKeys,
    bvid: &str,
    pages: &[models::PageItem],
    cookie_header: Option<&str>,
    on_progress: impl Fn(Vec<models::FormatOption>, Vec<models::FormatOption>),
) -> Option<(Vec<models::FormatOption>, Vec<models::FormatOption>)> {
    let max_samples = if pages.len() <= 16 { pages.len() } else { 8 };
    let indices = crate::ytdlp::sample_page_indices(pages.len() as u32, max_samples);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(PLAYURL_SAMPLE_CONCURRENCY));
    let mut tasks = tokio::task::JoinSet::new();
    for index in indices {
        let Some(page) = pages.get(index.saturating_sub(1) as usize) else {
            continue;
        };
        let cid = page.page_id.clone();
        let bvid = bvid.to_string();
        let cookie_header = cookie_header.map(str::to_string);
        let client = client.clone();
        let keys = keys.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("playurl sample semaphore closed");
            playurl::fetch_formats(&client, &keys, &bvid, &cid, cookie_header.as_deref(), true)
                .await
                .ok()
        });
    }
    let mut observed_video: Vec<models::FormatOption> = Vec::new();
    let mut observed_audio: Vec<models::FormatOption> = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let Ok(Some((video, audio))) = task else {
            continue;
        };
        playurl::merge_multi_page_options(&mut observed_video, video);
        playurl::merge_multi_page_options(&mut observed_audio, audio);
        on_progress(observed_video.clone(), observed_audio.clone());
    }
    (!observed_video.is_empty() || !observed_audio.is_empty())
        .then_some((observed_video, observed_audio))
}

pub fn is_bilibili_url(raw: &str) -> bool {
    let Ok(u) = Url::parse(raw.trim()) else {
        return false;
    };
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    matches!(
        host.as_str(),
        "www.bilibili.com" | "bilibili.com" | "m.bilibili.com" | "b23.tv" | "www.b23.tv"
    )
}

/// Extract the numeric mid from a space.bilibili.com URL, tolerating query
/// strings (spm_id_from=…) and sub-paths (/video, /uploads/video, /search).
pub fn parse_space_mid(url: &str) -> Option<u64> {
    let parsed = Url::parse(url.trim()).ok()?;
    if parsed.host_str()? != "space.bilibili.com" {
        return None;
    }
    let first = parsed.path_segments()?.next()?;
    if first.is_empty() {
        return None;
    }
    first.parse::<u64>().ok()
}

/// Rewrite list/watchlater URLs that point at a specific BV id into `/video/{bvid}`.
///
/// Bilibili "Watch Later" browser URLs look like
/// `https://www.bilibili.com/list/watchlater?bvid=BVxxx`. yt-dlp routes those to the
/// watchlater playlist extractor (`KeyError('type')` / login errors) instead of the
/// single-video extractor unless we canonicalize first.
pub fn canonicalize_video_url(raw: &str) -> String {
    let trimmed = raw.trim();
    let Ok(parsed) = Url::parse(trimmed) else {
        return trimmed.to_string();
    };

    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "www.bilibili.com" | "bilibili.com" | "m.bilibili.com"
    ) {
        return trimmed.to_string();
    }

    let path = parsed.path();
    let is_list_context = path.starts_with("/list/")
        || path.starts_with("/medialist/")
        || path == "/watchlater"
        || path.starts_with("/watchlater/");
    if !is_list_context {
        return trimmed.to_string();
    }

    let Some(bvid) = query_bvid(&parsed) else {
        return trimmed.to_string();
    };
    let page = parsed
        .query_pairs()
        .find(|(k, _)| k == "p")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty());

    match page {
        Some(p) => format!("https://www.bilibili.com/video/{bvid}?p={p}"),
        None => format!("https://www.bilibili.com/video/{bvid}"),
    }
}

fn query_bvid(parsed: &Url) -> Option<String> {
    let value = parsed
        .query_pairs()
        .find(|(k, _)| k == "bvid")
        .map(|(_, v)| v.into_owned())?;
    let trimmed = value.trim();
    if !trimmed.starts_with("BV") {
        return None;
    }
    let end = trimmed
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphanumeric())
        .map(|(i, _)| i)
        .unwrap_or(trimmed.len());
    let bvid = &trimmed[..end];
    if bvid.len() < 3 {
        return None;
    }
    Some(bvid.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::detect_platform;
    use super::*;

    #[test]
    fn detects_bv_watch_url() {
        assert!(is_bilibili_url(
            "https://www.bilibili.com/video/BV1xx411c7mD"
        ));
    }

    #[test]
    fn detects_b23_short_link() {
        assert!(is_bilibili_url("https://b23.tv/abcdef"));
    }

    #[test]
    fn rejects_youtube() {
        assert!(!is_bilibili_url(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        ));
    }

    #[test]
    fn detect_platform_returns_bilibili() {
        assert_eq!(
            detect_platform("https://www.bilibili.com/video/BV1xx411c7mD"),
            Some("bilibili")
        );
        assert_eq!(detect_platform("https://example.com"), None);
    }

    #[test]
    fn rewrites_watchlater_list_url_with_bvid() {
        assert_eq!(
            canonicalize_video_url(
                "https://www.bilibili.com/list/watchlater?bvid=BV1kw411R7Jo&spm_id_from=333"
            ),
            "https://www.bilibili.com/video/BV1kw411R7Jo"
        );
    }

    #[test]
    fn rewrites_user_watchlater_url_with_oid_and_tracking() {
        assert_eq!(
            canonicalize_video_url(
                "https://www.bilibili.com/list/watchlater?oid=114687521852968&bvid=BV1xRM8ziEo7&spm_id_from=333.1007.top_right_bar_window_view_later.content.click&vd_source=fe19d7c651c61acd5907002c5f06a392"
            ),
            "https://www.bilibili.com/video/BV1xRM8ziEo7"
        );
    }

    #[test]
    fn rewrites_medialist_watchlater_with_bvid_and_keeps_page() {
        assert_eq!(
            canonicalize_video_url(
                "https://www.bilibili.com/medialist/play/watchlater?bvid=BV1xx411c7mD&p=3"
            ),
            "https://www.bilibili.com/video/BV1xx411c7mD?p=3"
        );
    }

    #[test]
    fn leaves_plain_video_url_unchanged() {
        let url = "https://www.bilibili.com/video/BV1xx411c7mD?p=2";
        assert_eq!(canonicalize_video_url(url), url);
    }

    #[test]
    fn leaves_watchlater_without_bvid_unchanged() {
        let url = "https://www.bilibili.com/list/watchlater";
        assert_eq!(canonicalize_video_url(url), url);
    }
}

#[cfg(test)]
mod space_url_tests {
    use super::*;

    #[test]
    fn parse_space_mid_accepts_query_and_subpaths() {
        assert_eq!(
            parse_space_mid("https://space.bilibili.com/470995011?spm_id_from=333.337.0.0"),
            Some(470995011)
        );
        assert_eq!(
            parse_space_mid("https://space.bilibili.com/470995011/video"),
            Some(470995011)
        );
        assert_eq!(
            parse_space_mid("https://space.bilibili.com/470995011/uploads/video"),
            Some(470995011)
        );
    }

    #[test]
    fn parse_space_mid_rejects_invalid() {
        assert_eq!(parse_space_mid("https://space.bilibili.com/abc"), None);
        assert_eq!(parse_space_mid("https://space.bilibili.com/"), None);
        assert_eq!(
            parse_space_mid("https://www.bilibili.com/video/BV1xx411c7mD"),
            None
        );
        assert_eq!(parse_space_mid("not a url"), None);
    }
}
