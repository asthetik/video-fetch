use url::Url;

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
}
