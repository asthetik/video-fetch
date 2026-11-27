use std::path::Path;

use chrono::{DateTime, Local};

/// Video container extensions this app may produce (also used to spot the work-dir product).
pub const OUTPUT_EXTS: &[&str] = &["mp4", "mkv", "webm", "flv", "mov"];

/// Audio containers produced by `-x --audio-format`.
pub const AUDIO_OUTPUT_EXTS: &[&str] = &["m4a", "mp3", "flac", "opus", "ogg", "aac", "wav"];

const M4A_EXTS: &[&str] = &["m4a"];
const MP3_EXTS: &[&str] = &["mp3"];
const FLAC_EXTS: &[&str] = &["flac"];

/// Extensions that count as "the same file" for skip-existing checks. Audio
/// containers are distinct: an existing .m4a must not block a new .mp3 of the
/// same source, and save-as-copy numbering only counts same-container files.
pub fn conflict_exts(audio_format: Option<&str>) -> &'static [&'static str] {
    match audio_format {
        Some("m4a") => M4A_EXTS,
        Some("mp3") => MP3_EXTS,
        Some("flac") => FLAC_EXTS,
        Some(_) => AUDIO_OUTPUT_EXTS,
        None => OUTPUT_EXTS,
    }
}

pub fn path_is_audio_output(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    AUDIO_OUTPUT_EXTS
        .iter()
        .any(|e| e.eq_ignore_ascii_case(ext))
}

/// The app's own escape guard: a whole `.`, `..`, or empty segment is a
/// template-escape attempt that yt-dlp cannot produce (validate rejects the
/// literals, and substituted values cannot contain '/'), so it is folded to
/// `_` before any other layer sees it. This deliberately diverges from
/// yt-dlp, which resolves a `..` segment as a parent reference on Windows
/// and leaves it on Unix — the app refuses to ever write one.
fn guard_dot_segment(seg: &str) -> String {
    if seg.is_empty() || seg == "." || seg == ".." {
        "_".into()
    } else {
        seg.to_string()
    }
}

/// Reject templates that could escape the chosen save directory.
pub fn validate_output_template(template: &str) -> Result<(), String> {
    let trimmed = template.trim();
    if trimmed.is_empty() {
        return Err("文件名模板不能为空".into());
    }
    validate_path_shape(trimmed)?;
    validate_template_fields(trimmed)?;
    // The `>` formats are baked to literals before the template reaches
    // yt-dlp, so everything above only sees what was typed by hand. Bake with
    // any date and check the result too: `%(timestamp>/tmp/evil)s_…` becomes
    // an absolute path, and `output_dir.join` discards the work directory
    // entirely for an absolute argument. The date itself does not matter —
    // the shape is what is being checked.
    let baked = bake_local_datetime_tokens(trimmed, &Local::now());
    if baked != trimmed {
        validate_path_shape(&baked)?;
    }
    Ok(())
}

/// Reject a template whose shape would put the output outside its directory.
fn validate_path_shape(path: &str) -> Result<(), String> {
    if looks_like_absolute_path(path) {
        return Err("文件名模板不能使用绝对路径".into());
    }
    for seg in path.split(['/', '\\']) {
        if seg == ".." || seg == "." {
            return Err("文件名模板不能包含相对路径段（. 或 ..）".into());
        }
    }
    Ok(())
}

/// Fields the app substitutes itself, and therefore the only ones a template
/// may use: yt-dlp expands every field it knows, and its template syntax
/// reaches the filesystem path.
const ALLOWED_TEMPLATE_FIELDS: &[&str] = &[
    "title",
    "id",
    "uploader",
    "ext",
    "playlist_index",
    "upload_date",
    "timestamp",
    "release_timestamp",
    "epoch",
];

/// Reject a template field the app does not substitute.
///
/// The separator split above cannot see this one: to it `%(playlist_index|..)s`
/// is a single segment, but yt-dlp resolves it to `..` for a single-part video,
/// so the file lands above the work directory (verified against the pinned
/// 2026.08.19 build). A `>` format is checked as well — it is baked to a
/// literal before yt-dlp runs, so `%(timestamp>..)s` turns into a real `..`.
fn validate_template_fields(template: &str) -> Result<(), String> {
    let mut rest = template;
    while let Some(start) = rest.find("%(") {
        let after = &rest[start + 2..];
        let close = after
            .find(')')
            .ok_or_else(|| "文件名模板里有未闭合的 %(...) 字段".to_string())?;
        let inner = &after[..close];
        let (field, format) = match inner.split_once('>') {
            Some((field, format)) => (field, Some(format)),
            None => (inner, None),
        };
        // `%(field|default)s` substitutes the default when the field is empty.
        // The substituted *value* cannot escape: yt-dlp rewrites separators in
        // anything it substitutes (a title of `../../escape` lands as one
        // directory entry). The default is template text, though, so it is the
        // part that has to be checked — `%(playlist_index|..)s` was the escape.
        let (field, default) = match field.split_once('|') {
            Some((field, default)) => (field, Some(default)),
            None => (field, None),
        };
        if !ALLOWED_TEMPLATE_FIELDS.contains(&field) {
            return Err(format!("文件名模板不支持字段 %({inner})s"));
        }
        if let Some(default) = default
            && (default.contains(['/', '\\', ':'])
                || default == "."
                || default == ".."
                || default.is_empty())
        {
            return Err(format!("文件名模板的默认值不能是路径：%({inner})s"));
        }
        if let Some(format) = format
            && format.contains("..")
        {
            return Err(format!("文件名模板的日期格式不能包含 ..：%({inner})s"));
        }
        rest = &after[close + 1..];
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
    // Non-token text is copied as verbatim spans (`template[copied..i]`), never
    // re-encoded byte by byte: `u8 as char` mapped each UTF-8 byte to a Latin-1
    // codepoint (视 → è§†), so a second bake stacked another layer and
    // pause/resume renamed the file. `%` cannot appear inside a multi-byte
    // UTF-8 sequence, so scanning bytes for it never lands mid-character, and
    // every slice below starts and ends on a char boundary.
    let mut copied = 0;
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
                    out.push_str(&template[copied..i]);
                    out.push_str(&when.format(fmt).to_string());
                    i = token_end;
                    copied = i;
                    continue;
                }
            } else if inner == "upload_date" || inner == "timestamp" {
                out.push_str(&template[copied..i]);
                out.push_str(&when.format("%Y-%m-%dT%H-%M-%S").to_string());
                i = token_end;
                copied = i;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&template[copied..]);
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
        ("%(title)s", yt_dlp_value_sanitize(title)),
        ("%(id)s", yt_dlp_value_sanitize(id)),
        ("%(uploader)s", yt_dlp_value_sanitize(uploader)),
        ("%(ext)s", yt_dlp_value_sanitize(ext)),
        ("%(playlist_index)s", playlist_index.to_string()),
        ("%(playlist_index)03d", format!("{:03}", playlist_index)),
    ];
    for (token, value) in replacements {
        out = out.replace(token, &value);
    }
    // Keep '/' as the directory separator from the template. Per segment:
    // the app's escape guard first (a dot segment must never survive, on any
    // platform, whatever the later layers would do with it), then yt-dlp's
    // Windows path layer.
    out.split('/')
        .map(|seg| {
            if seg.contains('%') {
                // Unexpanded template text (a preview of a template validate
                // would reject): leave it visible instead of mangling it.
                seg.to_string()
            } else {
                yt_dlp_path_layer(&guard_dot_segment(seg))
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The field-value sanitizer yt-dlp actually applies by default (pinned
/// 2026.11.23, `YoutubeDL.prepare_filename`). Deliberately
/// platform-dependent, because yt-dlp itself is:
/// - non-Windows builds take the plain path — field values only get
///   `/` → U+29F8 and NULs stripped; `?`, `"`, `:` etc. survive verbatim
///   (they are legal on Unix filesystems), and `sanitize_path` is a no-op.
/// - Windows builds run `sanitize_filename(value, restricted=False,
///   is_id=NO_DEFAULT)` — the new rules that map `" * : < > ? | / \` to
///   their full-width counterparts, turn `\n` into a space (via the NUL
///   marker dance) and drop other control chars — and then fix each path
///   segment up like `sanitize_path` does (trailing dot/space → `#`,
///   ASCII-invalid → `#`).
///
/// Template literal text is never sanitized by yt-dlp and is left alone
/// here too; the '%' guard in the caller keeps preview text visible.
fn yt_dlp_value_sanitize(value: &str) -> String {
    #[cfg(windows)]
    {
        sanitize_filename_yt_dlp_windows(value)
    }
    #[cfg(not(windows))]
    {
        value.replace('/', "\u{29F8}").replace('\0', "")
    }
}

/// Windows path layer: `_sanitize_path_parts`' per-segment rewrite — each
/// of `/ < > : " \ | ? *` becomes `#`, and one trailing whitespace or dot
/// becomes `#` (`asdf...` → `asdf..#`). The '.'/'..' segment drop/pop of
/// that function is unreachable here: validate rejects literal dot
/// segments, and substituted values cannot contain '/' (sanitized above).
fn yt_dlp_path_layer(segment: &str) -> String {
    #[cfg(windows)]
    {
        let mut out: String = segment
            .chars()
            .map(|c| {
                if matches!(c, '/' | '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*') {
                    '#'
                } else {
                    c
                }
            })
            .collect();
        // `[\s.]$`: a single trailing whitespace or dot becomes '#'.
        let needs_fix = out
            .chars()
            .last()
            .is_some_and(|c| c.is_whitespace() || c == '.');
        if needs_fix {
            out.pop();
            out.push('#');
        }
        out
    }
    #[cfg(not(windows))]
    {
        segment.to_string()
    }
}

/// Port of `yt_dlp.utils.sanitize_filename` (2026.11.23) with
/// `restricted=False, is_id=NO_DEFAULT` — the new rules Windows applies to
/// every substituted field value. Verified against the shipped wheel's own
/// outputs (golden tests below). The `if not is_id` tail of the original
/// (`__` collapse, `'_'` strip, `'.'` strip) does not run on this default
/// path, so it is not ported.
#[cfg(windows)]
fn sanitize_filename_yt_dlp_windows(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let marked = collapse_timestamp_colons(s);
    let mut marked: String = {
        let mut out = String::with_capacity(marked.len() * 2);
        for c in marked.chars() {
            out.push_str(&replace_insane_windows(c));
        }
        out
    };
    marked = collapse_repeated_markers(&marked);
    marked = strip_marker_edges(&marked);
    let out = marked.replace('\0', "");
    if out.is_empty() { "_".into() } else { out }
}

/// `re.sub(r'[0-9]+(?::[0-9]+)+', ...)` — colons inside digit:digit:digit
/// timestamp runs become '_' (1:23:45 → 1_23_45), before the per-char pass.
#[cfg(windows)]
fn collapse_timestamp_colons(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let mut k = j;
            let mut groups = 0;
            while k < b.len() && b[k] == ':' {
                let mut m = k + 1;
                let mut digits = 0;
                while m < b.len() && b[m].is_ascii_digit() {
                    m += 1;
                    digits += 1;
                }
                if digits == 0 {
                    break;
                }
                k = m;
                groups += 1;
            }
            if groups > 0 {
                for &c in &b[start..k] {
                    if c == ':' {
                        out.push('_');
                    } else {
                        out.push(c);
                    }
                }
                i = k;
                continue;
            }
            for &c in &b[start..j] {
                out.push(c);
            }
            i = j;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// `replace_insane` for `restricted=False, is_id=NO_DEFAULT`: the full-width
/// branch catches all of `" * : < > ? | / \` first, so the later `?`/`"`/
/// `:`/`\/|*<>` branches are unreachable and only the control-char deletion
/// after them still fires. `\n` becomes a NUL marker + space, which the
/// marker phases below collapse and strip.
#[cfg(windows)]
fn replace_insane_windows(c: char) -> String {
    if c == '\n' {
        return "\0 ".into();
    }
    if matches!(c, '"' | '*' | ':' | '<' | '>' | '?' | '|' | '/' | '\\') {
        return match c {
            '/' => "\u{29F8}".into(),
            '\\' => "\u{29F9}".into(),
            other => char::from_u32(other as u32 + 0xFEE0)
                .map(String::from)
                .unwrap_or_else(|| other.to_string()),
        };
    }
    if (c as u32) < 32 || (c as u32) == 127 {
        return String::new();
    }
    c.to_string()
}

/// `re.sub(r'(\0.)(?:(?=\1)..)+', r'\1')` — a run of identical NUL+char
/// marker pairs collapses to its first pair.
#[cfg(windows)]
fn collapse_repeated_markers(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\0' && i + 1 < chars.len() {
            let (a, b) = (chars[i], chars[i + 1]);
            out.push(a);
            out.push(b);
            i += 2;
            while i + 1 < chars.len() && chars[i] == a && chars[i + 1] == b {
                i += 2;
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `re.sub(r'^\0.{STRIP_RE}|{STRIP_RE}\0.$', '')` with
/// `STRIP_RE = (?:\0.|[ _-])*` — marker pairs and plain `[ _-]` runs are
/// stripped from the ends, behind (or ahead of) a marker pair.
#[cfg(windows)]
fn strip_marker_edges(s: &str) -> String {
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && chars[0] == '\0' {
        let mut i = 2;
        loop {
            if i + 1 < chars.len() && chars[i] == '\0' {
                i += 2;
            } else if i < chars.len() && (chars[i] == ' ' || chars[i] == '_' || chars[i] == '-') {
                i += 1;
            } else {
                break;
            }
        }
        chars.drain(..i);
    }
    let n = chars.len();
    if n >= 2 && chars[n - 2] == '\0' {
        let mut end = n - 2;
        loop {
            if end >= 2 && chars[end - 2] == '\0' {
                end -= 2;
            } else if end > 0
                && (chars[end - 1] == ' ' || chars[end - 1] == '_' || chars[end - 1] == '-')
            {
                end -= 1;
            } else {
                break;
            }
        }
        chars.truncate(end);
    }
    chars.into_iter().collect()
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
    audio_format: Option<&str>,
) -> bool {
    conflict_exts(audio_format).iter().any(|ext| {
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
    audio_format: Option<&str>,
) -> String {
    if !any_ext_exists(
        save_dir,
        template,
        title,
        id,
        uploader,
        page_index,
        audio_format,
    ) {
        return template.to_string();
    }
    for n in 1u32..=10_000 {
        let candidate = with_copy_suffix(template, n);
        if !any_ext_exists(
            save_dir,
            &candidate,
            title,
            id,
            uploader,
            page_index,
            audio_format,
        ) {
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
            None,
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
            None,
        );
        assert_eq!(tmpl, "%(title)s [%(id)s].%(ext)s");
    }

    #[test]
    fn next_available_audio_ignores_existing_video_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("demo [BV1].mp4"), b"x").unwrap();
        let tmpl = next_available_output_template(
            dir.path(),
            "%(title)s [%(id)s].%(ext)s",
            "demo",
            "BV1",
            "",
            1,
            Some("m4a"),
        );
        assert_eq!(tmpl, "%(title)s [%(id)s].%(ext)s");
    }

    #[test]
    fn next_available_audio_ignores_other_container_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("demo [BV1].m4a"), b"x").unwrap();
        // An existing .m4a must not count as a copy of a new .mp3.
        let tmpl = next_available_output_template(
            dir.path(),
            "%(title)s [%(id)s].%(ext)s",
            "demo",
            "BV1",
            "",
            1,
            Some("mp3"),
        );
        assert_eq!(tmpl, "%(title)s [%(id)s].%(ext)s");
    }

    /// B16: the old nine-character mapping is gone. This pins the new
    /// prediction for an illegal-char title end to end (platform-split:
    /// unix keeps them verbatim, Windows maps to full-width twins — the
    /// char-level golden tests pin the layers).
    #[cfg(not(windows))]
    #[test]
    fn strips_illegal_path_chars() {
        let name = preview_filename_at(
            "%(title)s.mp4",
            "a/b:c*d?.mp4",
            "BV1xx",
            "UP",
            "mp4",
            1,
            &sample_local(),
        );
        assert_eq!(name, "a\u{29F8}b:c*d?.mp4.mp4");
    }

    #[test]
    fn guard_dot_segment_neutralizes_escape_segments() {
        assert_eq!(guard_dot_segment(".."), "_");
        assert_eq!(guard_dot_segment("."), "_");
        assert_eq!(guard_dot_segment(""), "_");
        assert_eq!(guard_dot_segment("title"), "title");
    }

    /// B16 golden: the Windows value layer against the shipped yt-dlp
    /// 2026.11.23 wheel — `sanitize_filename(s, restricted=False)` (the
    /// default `is_id=NO_DEFAULT` new rules). Full-width mapping for
    /// `" * : < > ? | / \`, `\n` → space through the NUL-marker dance,
    /// other control chars and 127 deleted, timestamp colons → '_', and —
    /// confirmed against the wheel — NO trailing strip, NO `__` collapse on
    /// this default path. This test is the Windows CI lane's witness.
    #[cfg(windows)]
    #[test]
    fn windows_value_layer_matches_the_pinned_yt_dlp() {
        let golden = [
            ("a?b", "a？b"),
            ("a\"b", "a＂b"),
            ("a:b", "a：b"),
            ("a<b>c|d*e\\f/g", "a＜b＞c｜d＊e⧹f⧸g"),
            ("line1\nline2", "line1 line2"),
            ("a\u{1}b", "ab"),
            ("a\u{7f}b", "ab"),
            ("a\tb", "ab"),
            ("1:23:45", "1_23_45"),
            ("测试？标题", "测试？标题"),
            ("", ""),
            ("trailing..", "trailing.."),
            ("a__b", "a__b"),
            (".dotfile", ".dotfile"),
        ];
        for (input, expected) in golden {
            assert_eq!(
                sanitize_filename_yt_dlp_windows(input),
                expected,
                "input {input:?}"
            );
        }
    }

    /// B16 golden: the Windows path layer against `_sanitize_path_parts`
    /// (`sanitize_path(force=True)` per segment) — ASCII-invalid chars and
    /// one trailing whitespace/dot become '#'.
    #[cfg(windows)]
    #[test]
    fn windows_path_layer_matches_the_pinned_yt_dlp() {
        let golden = [
            ("tail. ", "tail.#"),
            ("trailing..", "trailing.#"),
            ("a<b", "a#b"),
            ("a  b.", "a  b#"),
            ("normal", "normal"),
        ];
        for (input, expected) in golden {
            assert_eq!(yt_dlp_path_layer(input), expected, "input {input:?}");
        }
    }

    /// B16: the non-Windows value layer touches only '/' (→ U+29F8, the big
    /// solidus yt-dlp itself writes on macOS/Linux) and NULs. Everything a
    /// Unix filesystem accepts — '?', '"', ':' — survives verbatim, exactly
    /// as yt-dlp's plain default path leaves it.
    #[cfg(not(windows))]
    #[test]
    fn unix_value_layer_leaves_unix_legal_characters_alone() {
        assert_eq!(yt_dlp_value_sanitize("你好/世界"), "你好\u{29F8}世界");
        assert_eq!(yt_dlp_value_sanitize("a?b:\"c|d*e"), "a?b:\"c|d*e");
        assert_eq!(yt_dlp_value_sanitize("a\0b"), "ab");
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

    /// The separator split above cannot see yt-dlp's template syntax: to it
    /// `%(playlist_index|..)s` is one segment, but yt-dlp resolves it to `..`
    /// for a single-part video. Verified against the pinned 2026.08.19 build —
    /// the file landed two levels above the work directory. So the fields
    /// themselves are allowlisted, and a `>` format may not smuggle `..`
    /// either: the app bakes it to a literal before yt-dlp runs.
    /// The `>` format is baked to a literal before the template reaches
    /// yt-dlp, so a separator inside it supplies a prefix the checks on the
    /// raw template never see: `%(timestamp>/tmp/evil)s_…` bakes to
    /// `/tmp/evil_…`, and `output_dir.join` discards the work directory
    /// entirely for an absolute argument.
    #[test]
    fn validate_output_template_rejects_a_format_that_bakes_absolute() {
        for template in [
            "%(timestamp>/tmp/evil)s_%(title)s.%(ext)s",
            "%(upload_date>C:/evil)s_%(title)s.%(ext)s",
            "%(timestamp>\\\\server\\share\\x)s.%(ext)s",
        ] {
            assert!(
                validate_output_template(template).is_err(),
                "should have been rejected: {template}"
            );
        }
    }

    /// The same check must not take away date subdirectories, which are the
    /// reason a `>` format is allowed to contain a separator at all.
    #[test]
    fn validate_output_template_keeps_a_relative_date_subdirectory() {
        assert!(validate_output_template("%(timestamp>%Y/%m)s/%(title)s.%(ext)s").is_ok());
        assert!(validate_output_template("%(upload_date>%Y-%m-%d)s_%(title)s.%(ext)s").is_ok());
    }

    #[test]
    fn validate_output_template_rejects_fields_yt_dlp_would_expand() {
        for template in [
            "%(playlist_index|..)s/%(playlist_index|..)s/x.%(ext)s",
            "%(nonexist|..)s/x.%(ext)s",
            "%(timestamp>..)s/x.%(ext)s",
            "%(id)s/%(title|..)s.%(ext)s",
        ] {
            assert!(
                validate_output_template(template).is_err(),
                "should have been rejected: {template}"
            );
        }
    }

    #[test]
    fn validate_output_template_keeps_the_fields_the_app_substitutes() {
        for template in [
            "%(title)s.%(ext)s",
            "%(uploader)s/%(title)s [%(id)s].%(ext)s",
            "%(playlist_index)03d_%(title)s.%(ext)s",
            "%(upload_date)s_%(title)s.%(ext)s",
            "%(timestamp>%Y-%m-%d)s/%(title)s.%(ext)s",
            "%(release_timestamp>%Y%m%d)s-%(title)s.%(ext)s",
            // A default value is ordinary template text, and a harmless one
            // stays available — only the escaping ones are refused.
            "%(playlist_index|0)03d_%(title)s.%(ext)s",
            "%(playlist_index|unknown)s-%(title)s.%(ext)s",
        ] {
            assert!(
                validate_output_template(template).is_ok(),
                "should have been accepted: {template}"
            );
        }
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
        // Both platforms map '/' in a title to U+29F8 (the big solidus):
        // yt-dlp's unix default replaces it explicitly, and the Windows
        // full-width table sends it to the same codepoint. The prediction
        // must match that, or skip-existing stat checks miss the file.
        assert_eq!(name, "你好\u{29F8}世界 [BV1xx].mp4");
    }

    /// B16: unix-legal characters pass through untouched — the old
    /// nine-character mapping used to rewrite them into `_` and made the
    /// prediction diverge from yt-dlp's actual output (skip-existing then
    /// missed existing files). Windows output differs (full-width twins);
    /// pinned separately on the Windows lane.
    #[cfg(not(windows))]
    #[test]
    fn preview_keeps_unix_legal_title_characters_verbatim() {
        let name = preview_filename_at(
            "%(title)s.%(ext)s",
            "What? \"Is\" This: a|b<c>d*e",
            "BV1xx",
            "UP",
            "mp4",
            1,
            &sample_local(),
        );
        assert_eq!(name, "What? \"Is\" This: a|b<c>d*e.mp4");
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

    #[test]
    fn bake_keeps_non_ascii_template_text_intact() {
        let when = sample_local();
        // 🎬/📼 are 4-byte UTF-8 sequences flanking the 3-byte CJK brackets.
        // Bake, the pause/resume re-bake, and preview all expand through the
        // same verbatim span copy in `expand_datetime_tokens`, so exercising
        // the widest literal width here covers the whole family (the old
        // byte-wise re-encode mangled 3-byte and 4-byte text alike).
        let template = "🎬【%(uploader)s】📼%(title)s.%(ext)s";
        assert_eq!(
            bake_local_datetime_tokens(template, &when),
            template,
            "token-free bake must be the identity, not a byte-wise re-encode"
        );
    }

    #[test]
    fn bake_is_idempotent_for_non_ascii_templates() {
        let when = sample_local();
        let template = "【%(uploader)s】%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s.%(ext)s";
        let once = bake_local_datetime_tokens(template, &when);
        let twice = bake_local_datetime_tokens(&once, &when);
        assert_eq!(
            once, twice,
            "bake(bake(x)) must equal bake(x) for CJK templates"
        );
        assert!(
            once.starts_with(&format!(
                "【%(uploader)s】{}",
                when.format("%Y-%m-%dT%H-%M-%S")
            )),
            "CJK text must survive baking verbatim: {once}"
        );

        // Pin the old byte-wise expansion: `out.push(bytes[i] as char)` mapped
        // each UTF-8 byte to a Latin-1 codepoint (视 → è§†), so a second pass
        // stacked another layer (Ã¨Â§Â†) and pause/resume renamed the file.
        // Replay that pass on the same input so the regression stays visible:
        // the old pass must disagree with itself, while the real bake above
        // agrees with itself.
        fn byte_wise_latin1(s: &str) -> String {
            s.bytes().map(|b| b as char).collect()
        }
        let mojibake_once = byte_wise_latin1(template);
        let mojibake_twice = byte_wise_latin1(&mojibake_once);
        assert_ne!(
            mojibake_once, mojibake_twice,
            "the old byte-wise expansion must stay non-idempotent to pin the regression"
        );
    }

    #[test]
    fn preview_keeps_non_ascii_template_text_intact() {
        let when = sample_local();
        let name = preview_filename_at(
            "【%(uploader)s】%(title)s.%(ext)s",
            "标题",
            "BV1xx",
            "UP主",
            "mp4",
            1,
            &when,
        );
        assert_eq!(name, "【UP主】标题.mp4");
    }
}
