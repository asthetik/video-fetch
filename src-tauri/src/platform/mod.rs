mod bilibili;

pub use bilibili::{canonicalize_video_url, is_bilibili_url, parse_space_mid};

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
        return Err("暂不支持该链接，最小可行产品仅支持 B 站");
    }
    Ok(canonical)
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
}
