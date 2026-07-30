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
