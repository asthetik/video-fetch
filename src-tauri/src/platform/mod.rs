mod bilibili;

pub use bilibili::{canonicalize_video_url, is_bilibili_url, parse_space_mid};

pub fn detect_platform(url: &str) -> Option<&'static str> {
    if is_bilibili_url(url) {
        Some("bilibili")
    } else {
        None
    }
}
