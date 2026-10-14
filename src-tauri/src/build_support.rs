//! Build-time helpers shared between build.rs and the crate's tests (build.rs
//! itself is never unit-tested by cargo, so the extraction logic lives here).

/// ffmpeg pin: find the `FFMPEG_VERSION` assignment in
/// scripts/fetch_sidecars.py and take the first quoted value on that line;
/// trailing content (e.g. a comment) is tolerated. Mentions without a quoted
/// value (e.g. the module docstring) are skipped.
pub fn extract_ffmpeg_version(source: &str) -> String {
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        // Assignment shape: the line must start with the constant name
        // (leading indentation and trailing content are tolerated), so a
        // mention elsewhere can never hijack the injection.
        let Some(rest) = trimmed.strip_prefix("FFMPEG_VERSION") else {
            continue;
        };
        // Anchor on the assignment: a longer constant that merely starts with
        // FFMPEG_VERSION (e.g. FFMPEG_VERSION_FALLBACK) must never be read as
        // this one.
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let Some(value) = quoted_value(rest) else {
            continue;
        };
        if !is_x_y_z(&value) {
            panic!(
                "unsupported ffmpeg version {value:?} in scripts/fetch_sidecars.py; \
                 expected a full x.y.z release like 9.0.1"
            );
        }
        return value;
    }
    panic!("no FFMPEG_VERSION = \"x.y.z\" assignment found in scripts/fetch_sidecars.py");
}

fn quoted_value(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let end = s[start..].find('"')? + start;
    Some(s[start..end].to_string())
}

fn is_x_y_z(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Value: protects=the '=' anchor in the version extraction;
    /// fails_when=a longer FFMPEG_-prefixed constant is read as the version;
    /// why_new=the anchor was added untested (build.rs cannot carry tests);
    /// seam=none
    #[test]
    fn a_prefixed_constant_is_not_read_as_the_version() {
        let src = "FFMPEG_VERSION_FALLBACK = \"0.0.0\"\nFFMPEG_VERSION = \"9.0.2\"\n";
        assert_eq!(extract_ffmpeg_version(src), "9.0.2");
    }

    #[test]
    fn comments_and_call_sites_do_not_hijack_the_extraction() {
        let src = "# FFMPEG_VERSION = \"0.0.0\"\nprint(FFMPEG_VERSION)\nFFMPEG_VERSION=\"9.0.2\"\n";
        assert_eq!(extract_ffmpeg_version(src), "9.0.2");
    }

    #[test]
    #[should_panic(expected = "no FFMPEG_VERSION")]
    fn a_missing_assignment_panics_loudly() {
        let _ = extract_ffmpeg_version("FFMPEG_VERSION_FALLBACK = \"0.0.0\"\n");
    }
}
