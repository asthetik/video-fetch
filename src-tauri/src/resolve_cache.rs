pub const RESOLVE_CACHE_TTL_SECS: u64 = 7 * 24 * 3600;

/// Cache key generation (`*_g2`). Bump when cached `VideoMeta.formats` shape changes.
pub const SCOPE_AUTHED: &str = "authed_g2";
pub const SCOPE_GUEST: &str = "guest_g2";

const STRIP_QUERY_PARAMS: &[&str] = &["spm_id_from", "vd_source", "from_spmid", "p"];

pub fn extract_bilibili_id(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let path = parsed.path();
    let bv_start = path.find("BV")?;
    let rest = &path[bv_start..];
    let end = rest
        .char_indices()
        .find(|(_, c)| !c.is_ascii_alphanumeric())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

pub fn normalize_url_for_cache(url: &str) -> String {
    let trimmed = url.trim();
    let Ok(mut parsed) = url::Url::parse(trimmed) else {
        return trimmed.to_string();
    };

    if let Some(host) = parsed.host_str() {
        let _ = parsed.set_host(Some(&host.to_ascii_lowercase()));
    }

    parsed.set_fragment(None);

    if let Some(query) = parsed.query() {
        let kept: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .filter(|(key, _)| !STRIP_QUERY_PARAMS.contains(&key.as_ref()))
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();

        let mut query_pairs = parsed.query_pairs_mut();
        query_pairs.clear();
        for (key, value) in kept {
            query_pairs.append_pair(&key, &value);
        }
    }

    parsed.to_string()
}

pub fn cache_scope(has_cookies: bool) -> &'static str {
    if has_cookies {
        SCOPE_AUTHED
    } else {
        SCOPE_GUEST
    }
}

pub fn lookup_cache_keys(url: &str, scope: &str) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(id) = extract_bilibili_id(url) {
        keys.push(format!("bilibili:{}:{}", id.to_ascii_lowercase(), scope));
    }
    keys.push(format!("url:{}:{}", normalize_url_for_cache(url), scope));
    keys
}

pub fn store_cache_keys(url: &str, video_id: &str, scope: &str) -> Vec<String> {
    vec![
        format!("bilibili:{}:{}", video_id.to_ascii_lowercase(), scope),
        format!("url:{}:{}", normalize_url_for_cache(url), scope),
    ]
}

pub fn is_legacy_cache_key(key: &str) -> bool {
    !(key.ends_with(":authed_g2") || key.ends_with(":guest_g2"))
}

pub fn is_fresh(fetched_at: i64, now: i64, ttl_secs: u64) -> bool {
    now.saturating_sub(fetched_at) <= ttl_secs as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_bv_from_watch_url() {
        assert_eq!(
            extract_bilibili_id("https://www.bilibili.com/video/BV1xx411c7mD?p=2&spm_id_from=x"),
            Some("BV1xx411c7mD".into())
        );
    }

    #[test]
    fn lookup_keys_include_scope_suffix() {
        let keys = lookup_cache_keys(
            "https://www.bilibili.com/video/BV1xx411c7mD?p=3&vd_source=abc",
            SCOPE_GUEST,
        );
        assert_eq!(keys[0], "bilibili:bv1xx411c7md:guest_g2");
        assert!(keys[1].starts_with("url:") && keys[1].ends_with(":guest_g2"));
        assert!(!keys[1].contains("vd_source"));
        assert!(!keys[1].contains("p=3"));
    }

    #[test]
    fn store_keys_authed_differ_from_guest() {
        let url = "https://www.bilibili.com/video/BV1xx411c7mD";
        let guest = store_cache_keys(url, "BV1xx411c7mD", SCOPE_GUEST);
        let authed = store_cache_keys(url, "BV1xx411c7mD", SCOPE_AUTHED);
        assert_eq!(guest[0], "bilibili:bv1xx411c7md:guest_g2");
        assert_eq!(authed[0], "bilibili:bv1xx411c7md:authed_g2");
        assert_ne!(guest[0], authed[0]);
    }

    #[test]
    fn cache_scope_from_cookies_flag() {
        assert_eq!(cache_scope(true), SCOPE_AUTHED);
        assert_eq!(cache_scope(false), SCOPE_GUEST);
    }

    #[test]
    fn legacy_keys_detected() {
        assert!(is_legacy_cache_key("bilibili:bv1xx411c7md"));
        assert!(is_legacy_cache_key("url:https://example.com/x"));
        assert!(is_legacy_cache_key("bilibili:bv1xx411c7md:guest"));
        assert!(is_legacy_cache_key("bilibili:bv1xx411c7md:authed"));
        assert!(!is_legacy_cache_key("bilibili:bv1xx411c7md:guest_g2"));
        assert!(!is_legacy_cache_key("bilibili:bv1xx411c7md:authed_g2"));
    }

    #[test]
    fn is_fresh_respects_ttl() {
        let now = 1_000_000_i64;
        assert!(is_fresh(now - 10, now, 100));
        assert!(!is_fresh(now - 200, now, 100));
    }
}
