//! Pin parsing shared between `build.rs` and unit tests.
//!
//! `build.rs` includes this file via `#[path]` and `lib.rs` pulls it in under
//! `#[cfg(test)]`, so the build script's parsing logic is testable without
//! executing the build script.

/// ffmpeg pin: find the `FFMPEG_VERSION` assignment in
/// scripts/fetch_sidecars.py and take its quoted value; trailing content
/// (e.g. a comment) is tolerated. The value must open with the quote — a
/// computed right-hand side is rejected rather than allowing a version-shaped
/// string in its trailing comment to be injected. Mentions without a quoted
/// value (e.g. the module docstring) are skipped.
pub(crate) fn extract_ffmpeg_version(source: &str) -> String {
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        // Assignment shape: split at the `=` and require the name to be
        // exactly `FFMPEG_VERSION`, so a longer constant sharing the prefix
        // (e.g. `FFMPEG_VERSION_EXTRA = "9.9"`) can never hijack the
        // injection. Spaces around the `=` are tolerated; a mention elsewhere
        // (docstring, comment prose) never reaches this point.
        let Some((name, rest)) = trimmed.split_once('=') else {
            continue;
        };
        if name.trim_end() != "FFMPEG_VERSION" {
            continue;
        }
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
    // The literal must open the value: taking the first quote anywhere on the
    // line would read a version out of a comment attached to a computed RHS.
    let rest = s.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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

    /// Value: protects=the ffmpeg pin build.rs injects; fails_when=a longer
    /// constant name sharing the `FFMPEG_VERSION` prefix (e.g.
    /// `FFMPEG_VERSION_EXTRA`) is mistaken for the pin — with the unanchored
    /// prefix match, a leading `FFMPEG_VERSION_EXTRA = "9.9.1"` was returned
    /// as the version (and `= "9.9"` panicked) instead of the real assignment
    /// below it; why_new=review finding A15; seam=none
    #[test]
    fn longer_constant_names_never_hijack_the_ffmpeg_version() {
        // x.y.z-shaped trap: silently returned as the version before the fix.
        let source = "FFMPEG_VERSION_EXTRA = \"9.9.1\"\nFFMPEG_VERSION = \"9.0.2\"\n";
        assert_eq!(extract_ffmpeg_version(source), "9.0.2");

        // Finding's literal example (not x.y.z): used to panic before the
        // real assignment was ever reached.
        let source = "FFMPEG_VERSION_EXTRA=9.9\nFFMPEG_VERSION = \"9.0.2\"\n";
        assert_eq!(extract_ffmpeg_version(source), "9.0.2");
    }

    /// The real assignment shape (spaces around `=`, trailing comment) must
    /// keep matching; a mention inside a comment is still skipped.
    #[test]
    fn plain_assignment_and_comment_handling_are_unchanged() {
        let source = "# see FFMPEG_VERSION = \"0.0.0\"\nFFMPEG_VERSION = \"9.0.2\"  # pin\n";
        assert_eq!(extract_ffmpeg_version(source), "9.0.2");
    }

    /// A computed right-hand side must not borrow a version-looking string
    /// from its trailing comment: only the quoted literal that opens the
    /// assignment is the value. Unreachable while the pin stays a literal —
    /// this pins the parser's shape against the next edit, not today's file.
    ///
    /// Value: protects=the quote anchored at the start of the value;
    /// fails_when=the first quote anywhere on the line is taken (a comment
    /// like `# falls back to "9.0.1"` becomes the injected version);
    /// why_new=the post-0.4.2 review flagged the unanchored quote scan;
    /// seam=none
    #[test]
    fn a_computed_rhs_cannot_borrow_a_version_from_its_comment() {
        let source = concat!(
            "FFMPEG_VERSION = compute_version()  # falls back to \"9.0.1\"\n",
            "FFMPEG_VERSION = \"9.0.2\"\n",
        );
        assert_eq!(
            extract_ffmpeg_version(source),
            "9.0.2",
            "the comment's quote pair must not be read as the version"
        );

        let computed_only = "FFMPEG_VERSION = compute_version()  # see \"9.0.1\"\n";
        assert!(
            std::panic::catch_unwind(|| extract_ffmpeg_version(computed_only)).is_err(),
            "a computed RHS must fail the build instead of injecting a comment's version"
        );
    }
}
