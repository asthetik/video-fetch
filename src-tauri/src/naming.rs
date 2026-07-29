use std::path::Path;

use chrono::{DateTime, Local};

const OUTPUT_EXTS: &[&str] = &["mp4", "mkv", "webm", "flv", "mov"];

pub fn sanitize_filename_component(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    if out.is_empty() || out == "." || out == ".." {
        "_".into()
    } else {
        out
    }
}

/// Reject templates that could escape the chosen save directory.
pub fn validate_output_template(template: &str) -> Result<(), String> {
    let trimmed = template.trim();
    if trimmed.is_empty() {
        return Err("文件名模板不能为空".into());
    }
    if looks_like_absolute_path(trimmed) {
        return Err("文件名模板不能使用绝对路径".into());
    }
    for seg in trimmed.split(['/', '\\']) {
        if seg == ".." || seg == "." {
            return Err("文件名模板不能包含相对路径段（. 或 ..）".into());
        }
    }
    Ok(())
}

/// True for OS-absolute paths and for Unix-style roots that Windows does not
/// treat as absolute (e.g. `/virtual/...`).
fn looks_like_absolute_path(template: &str) -> bool {
    if Path::new(template).is_absolute() {
        return true;
    }
    if template.starts_with('/') || template.starts_with('\\') {
        return true;
    }
    let bytes = template.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Replace datetime tokens with a concrete local-time string so yt-dlp does not
/// expand `timestamp` in UTC.
pub fn bake_local_datetime_tokens(template: &str, when: &DateTime<Local>) -> String {
    expand_datetime_tokens(template, when)
}

fn expand_datetime_tokens(template: &str, when: &DateTime<Local>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'('
            && let Some(rel_end) = template[i + 2..].find(")s")
        {
            let inner = &template[i + 2..i + 2 + rel_end];
            let token_end = i + 2 + rel_end + 2;
            if let Some((field, fmt)) = inner.split_once('>') {
                if matches!(
                    field,
                    "timestamp" | "release_timestamp" | "epoch" | "upload_date"
                ) {
                    out.push_str(&when.format(fmt).to_string());
                    i = token_end;
                    continue;
                }
            } else if inner == "upload_date" || inner == "timestamp" {
                out.push_str(&when.format("%Y-%m-%dT%H-%M-%S").to_string());
                i = token_end;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

pub fn preview_filename(
    template: &str,
    title: &str,
    id: &str,
    uploader: &str,
    ext: &str,
    playlist_index: u32,
) -> String {
    preview_filename_at(
        template,
        title,
        id,
        uploader,
        ext,
        playlist_index,
        &Local::now(),
    )
}

pub fn preview_filename_at(
    template: &str,
    title: &str,
    id: &str,
    uploader: &str,
    ext: &str,
    playlist_index: u32,
    when: &DateTime<Local>,
) -> String {
    let mut out = expand_datetime_tokens(template, when);
    let replacements = [
        ("%(title)s", sanitize_filename_component(title)),
        ("%(id)s", sanitize_filename_component(id)),
        ("%(uploader)s", sanitize_filename_component(uploader)),
        ("%(ext)s", sanitize_filename_component(ext)),
        ("%(playlist_index)s", playlist_index.to_string()),
        ("%(playlist_index)03d", format!("{:03}", playlist_index)),
    ];
    for (token, value) in replacements {
        out = out.replace(token, &value);
    }
    // Keep '/' as directory separator from the template; sanitize each segment.
    out.split('/')
        .map(|seg| {
            let cleaned = if seg.contains('%') {
                seg.to_string()
            } else {
                sanitize_filename_component(seg)
            };
            if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
                "_".into()
            } else {
                cleaned
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Insert ` (n)` before `.%(ext)s`, or append ` (n)` if that token is absent.
pub fn with_copy_suffix(template: &str, n: u32) -> String {
    if let Some(pos) = template.rfind(".%(ext)s") {
        format!("{} ({}){}", &template[..pos], n, &template[pos..])
    } else {
        format!("{template} ({n})")
    }
}

fn any_ext_exists(
    save_dir: &Path,
    template: &str,
    title: &str,
    id: &str,
    uploader: &str,
    page_index: u32,
) -> bool {
    OUTPUT_EXTS.iter().any(|ext| {
        let rel = preview_filename(template, title, id, uploader, ext, page_index);
        save_dir.join(rel).is_file()
    })
}

/// Return `template` if free; otherwise the smallest `with_copy_suffix(template, n)` that is free.
pub fn next_available_output_template(
    save_dir: &Path,
    template: &str,
    title: &str,
    id: &str,
    uploader: &str,
    page_index: u32,
) -> String {
    if !any_ext_exists(save_dir, template, title, id, uploader, page_index) {
        return template.to_string();
    }
    for n in 1u32..=10_000 {
        let candidate = with_copy_suffix(template, n);
        if !any_ext_exists(save_dir, &candidate, title, id, uploader, page_index) {
            return candidate;
        }
    }
    with_copy_suffix(template, 10_001)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::fs;

    fn sample_local() -> DateTime<Local> {
        Local
            .with_ymd_and_hms(2024, 1, 1, 12, 0, 0)
            .single()
            .expect("valid local datetime")
    }

    #[test]
    fn with_copy_suffix_inserts_before_ext_token() {
        assert_eq!(
            with_copy_suffix("%(title)s [%(id)s].%(ext)s", 1),
            "%(title)s [%(id)s] (1).%(ext)s"
        );
        assert_eq!(
            with_copy_suffix("%(title)s [%(id)s].%(ext)s", 2),
            "%(title)s [%(id)s] (2).%(ext)s"
        );
    }

    #[test]
    fn next_available_skips_occupied_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        fs::write(base.join("demo [BV1].mp4"), b"x").unwrap();
        fs::write(base.join("demo [BV1] (1).mp4"), b"x").unwrap();
        let tmpl = next_available_output_template(
            base,
            "%(title)s [%(id)s].%(ext)s",
            "demo",
            "BV1",
            "",
            1,
        );
        assert_eq!(tmpl, "%(title)s [%(id)s] (2).%(ext)s");
    }

    #[test]
    fn next_available_returns_base_when_free() {
        let dir = tempfile::tempdir().unwrap();
        let tmpl = next_available_output_template(
            dir.path(),
            "%(title)s [%(id)s].%(ext)s",
            "demo",
            "BV9",
            "",
            1,
        );
        assert_eq!(tmpl, "%(title)s [%(id)s].%(ext)s");
    }

    #[test]
    fn strips_illegal_path_chars() {
        assert_eq!(sanitize_filename_component("a/b:c*d?.mp4"), "a_b_c_d_.mp4");
    }

    #[test]
    fn sanitize_rejects_dot_segments() {
        assert_eq!(sanitize_filename_component(".."), "_");
        assert_eq!(sanitize_filename_component("."), "_");
        assert_eq!(sanitize_filename_component(""), "_");
    }

    #[test]
    fn validate_output_template_rejects_parent_segments() {
        assert!(validate_output_template("../../%(title)s.%(ext)s").is_err());
        assert!(validate_output_template("%(uploader)s/../%(title)s.%(ext)s").is_err());
        assert!(validate_output_template("/virtual/%(title)s.%(ext)s").is_err());
        assert!(validate_output_template("C:/virtual/%(title)s.%(ext)s").is_err());
        assert!(validate_output_template("\\\\virtual\\share\\%(title)s.%(ext)s").is_err());
        assert!(validate_output_template("%(title)s [%(id)s].%(ext)s").is_ok());
        assert!(validate_output_template("%(uploader)s/%(title)s.%(ext)s").is_ok());
    }

    #[test]
    fn preview_neutralizes_dot_title() {
        let name = preview_filename_at(
            "%(title)s/%(id)s.%(ext)s",
            "..",
            "BV1",
            "UP",
            "mp4",
            1,
            &sample_local(),
        );
        assert_eq!(name, "_/BV1.mp4");
    }

    #[test]
    fn previews_default_template() {
        let name = preview_filename_at(
            "%(title)s [%(id)s].%(ext)s",
            "你好/世界",
            "BV1xx",
            "UP",
            "mp4",
            1,
            &sample_local(),
        );
        assert_eq!(name, "你好_世界 [BV1xx].mp4");
    }

    #[test]
    fn supports_uploader_subdir_tokens() {
        let name = preview_filename_at(
            "%(uploader)s/%(title)s [%(id)s].%(ext)s",
            "t",
            "BV1",
            "Alice",
            "mp4",
            1,
            &sample_local(),
        );
        assert_eq!(name, "Alice/t [BV1].mp4");
    }

    #[test]
    fn bakes_timestamp_in_local_timezone_to_seconds() {
        let when = sample_local();
        let baked = bake_local_datetime_tokens(
            "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s [%(id)s].%(ext)s",
            &when,
        );
        assert_eq!(
            baked,
            format!(
                "{}_%(title)s [%(id)s].%(ext)s",
                when.format("%Y-%m-%dT%H-%M-%S")
            )
        );
        let name = preview_filename_at(
            "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s [%(id)s].%(ext)s",
            "示例",
            "BV1xx",
            "UP",
            "mp4",
            1,
            &when,
        );
        assert_eq!(
            name,
            format!("{}_示例 [BV1xx].mp4", when.format("%Y-%m-%dT%H-%M-%S"))
        );
    }

    #[test]
    fn bare_upload_date_defaults_to_local_seconds() {
        let when = sample_local();
        let baked = bake_local_datetime_tokens("%(upload_date)s_%(title)s.%(ext)s", &when);
        assert_eq!(
            baked,
            format!("{}_%(title)s.%(ext)s", when.format("%Y-%m-%dT%H-%M-%S"))
        );
    }
}
