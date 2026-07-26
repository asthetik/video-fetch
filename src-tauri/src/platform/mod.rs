mod bilibili;

pub use bilibili::is_bilibili_url;

pub fn detect_platform(url: &str) -> Option<&'static str> {
    if is_bilibili_url(url) {
        Some("bilibili")
    } else {
        None
    }
}
