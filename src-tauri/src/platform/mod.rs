mod bilibili;

pub use bilibili::{BilibiliPlatform, canonicalize_video_url, is_bilibili_url};

use crate::models::{FormatOption, UrlKind, VideoMeta};

pub fn detect_platform(url: &str) -> Option<&'static str> {
    if is_bilibili_url(url) {
        Some("bilibili")
    } else {
        None
    }
}

/// Canonicalize `url` and refuse anything that is not a supported video page.
///
/// Every path that hands a URL to the downloader goes through this. Enqueue
/// used to skip it and pass the frontend's raw string to yt-dlp as a
/// positional argument, where a leading `-` is read as an option
/// (`yt-dlp -J "--version"` prints the version) and any host is fetched.
pub fn require_supported(url: &str) -> Result<String, &'static str> {
    let canonical = canonicalize_video_url(url);
    if detect_platform(&canonical).is_none() {
        return Err("暂不支持该链接，当前版本仅支持 B 站");
    }
    Ok(canonical)
}

/// Generic dependencies of the resolve orchestrator — everything a platform
/// implementation may touch. Bilibili-owned state (wbi keys, space info
/// cache) lives inside [`BilibiliPlatform`], not here.
pub(crate) struct ResolveCtx<'a> {
    pub client: &'a reqwest::Client,
    pub cookie_header: Option<&'a str>,
}

/// The resolve orchestration's event sink. The production implementation
/// wraps the Tauri app handle; the orchestrator tests capture the sequence.
pub(crate) trait ResolveEvents: Send + Sync {
    /// `event` is one of the RESOLVE_* event name constants.
    fn emit_meta(&self, event: &'static str, request_id: u64, meta: &VideoMeta);
    fn emit_formats_failed(&self, request_id: u64, error: &str);
}

/// A fast-path result: the platform's partial metadata (title/pages/
/// thumbnail from the cheap HTTP endpoints) plus, when the platform could
/// produce them without yt-dlp, the format lists.
pub(crate) struct FastResolve {
    pub meta: VideoMeta,
    pub formats: Option<(Vec<FormatOption>, Vec<FormatOption>)>,
}

/// B21: the resolve orchestration's platform seam. Single production
/// implementation (bilibili) plus a test fake — deliberately no registry,
/// discovery, or plugin machinery: a second platform is not on the roadmap,
/// and the trait exists so the orchestrator can be tested without HTTP and
/// so bilibili-specific detail has exactly one home.
#[async_trait::async_trait]
pub(crate) trait VideoPlatform: Send + Sync {
    /// detect_url's classification. The bilibili implementation includes
    /// space/mid parsing; another platform's would return Video/Unknown
    /// only — the space page is reached through space URLs, which stay a
    /// bilibili capability.
    fn classify_url(&self, url: &str) -> UrlKind;

    fn cache_scope(&self, has_cookies: bool) -> &'static str;
    fn cache_lookup_keys(&self, url: &str, scope: &str) -> Vec<String>;
    fn cache_store_keys(&self, url: &str, video_id: &str, scope: &str) -> Vec<String>;

    /// The platform's HTTP fast path (view → playurl → wbi → multi-page
    /// sampling for bilibili). `emit_partial` carries the orchestrator's
    /// is_current gate: the implementation calls it exactly once, when the
    /// partial metadata (the card the UI renders before formats exist) is
    /// ready, and returning `false` = superseded aborts the whole fast path
    /// (the pre-B21 `Ok(partial) if is_current()` arm).
    /// `emit_formats_progress` is deliberately ungated — per-sample batches
    /// fire as they complete and the frontend filters stale request ids.
    /// Returns `None` when even the partial metadata could not be fetched;
    /// `FastResolve.formats == None` means the orchestrator should run the
    /// generic yt-dlp fallback and [`Self::merge_fallback`].
    async fn fast_metadata(
        &self,
        ctx: &ResolveCtx<'_>,
        url: &str,
        emit_partial: &(dyn Fn(VideoMeta) -> bool + Send + Sync),
        emit_formats_progress: &(dyn Fn(VideoMeta) + Send + Sync),
    ) -> Option<FastResolve>;

    /// Fold a generic yt-dlp fallback result into the fast-path partial
    /// (pages/thumbnail/authoring from the fast path, formats from the
    /// fallback).
    fn merge_fallback(&self, partial: VideoMeta, fallback: VideoMeta) -> VideoMeta;

    /// What failed fast-path fetches leave behind for debugging — the
    /// orchestrator logs it with the resolve line. Bilibili returns the
    /// BV id; other platforms may return the URL or nothing.
    fn log_subject(&self, url: &str) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_supported_url_is_canonicalized() {
        assert_eq!(
            require_supported("  https://www.bilibili.com/video/BV1xx411c7mD  ").unwrap(),
            "https://www.bilibili.com/video/BV1xx411c7mD"
        );
        assert!(require_supported("https://b23.tv/abc").is_ok());
    }

    /// The URL becomes an argv entry for yt-dlp, so an unvalidated one is both
    /// an option-injection and an "fetch any host" primitive.
    #[test]
    fn anything_else_is_refused() {
        for url in [
            "",
            "--version",
            "--config-locations=/tmp/x",
            "https://example.com/video",
            "http://127.0.0.1:8731/x.mp4",
            "file:///etc/passwd",
            "https://www.bilibili.com.evil.test/video/BV1",
        ] {
            assert!(
                require_supported(url).is_err(),
                "should have been refused: {url}"
            );
        }
    }

    /// The rejection surfaces verbatim in the UI, so it must speak user
    /// language; "MVP" is development jargon a downloader user should never
    /// have to parse.
    #[test]
    fn the_rejection_message_speaks_user_language() {
        let message = require_supported("https://example.com/video").unwrap_err();
        assert!(
            message.contains("当前版本"),
            "rejection must name the current version, got: {message}"
        );
        assert!(
            !message.contains("最小可行产品"),
            "dev jargon leaked into user copy: {message}"
        );
    }
}
