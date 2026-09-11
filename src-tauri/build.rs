use std::fs;
use std::path::PathBuf;

fn main() {
    // The pin files live outside the src-tauri package dir, and emitting any
    // rerun-if-changed replaces cargo's default rebuild policy, so both files
    // must be declared explicitly or a Dependabot bump would not recompile.
    println!("cargo:rerun-if-changed=../scripts/requirements-sidecars.txt");
    println!("cargo:rerun-if-changed=../scripts/fetch_sidecars.py");

    let scripts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts");
    let requirements = fs::read_to_string(scripts.join("requirements-sidecars.txt"))
        .unwrap_or_else(|e| panic!("failed to read scripts/requirements-sidecars.txt: {e}"));
    let fetch_script = fs::read_to_string(scripts.join("fetch_sidecars.py"))
        .unwrap_or_else(|e| panic!("failed to read scripts/fetch_sidecars.py: {e}"));

    let ytdlp = normalize_ytdlp_pin(&requirements);
    let ffmpeg = extract_ffmpeg_version(&fetch_script);

    println!("cargo:rustc-env=SIDECAR_YTDLP_VERSION={ytdlp}");
    println!("cargo:rustc-env=SIDECAR_FFMPEG_VERSION={ffmpeg}");

    tauri_build::build()
}

/// yt-dlp pin: parse the `yt-dlp==<version>` line and zero-pad the
/// PyPI-normalized version (2026.8.19) back to the GitHub release tag
/// (2026.08.19). Mirrors the validation in scripts/fetch_sidecars.py so the
/// shown value always matches the binary the release downloads.
fn normalize_ytdlp_pin(source: &str) -> String {
    let mut pin: Option<String> = None;
    for line in source.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, rest)) = line.split_once("==") else {
            continue;
        };
        if name.trim() != "yt-dlp" {
            continue;
        }
        // Tolerate a trailing comment after the version.
        let raw = rest.split('#').next().unwrap_or("").trim();
        let Some(tag) = normalize_version_tag(raw) else {
            panic!(
                "unsupported yt-dlp pin {raw:?} in scripts/requirements-sidecars.txt; \
                 expected a stable release like 2026.8.19"
            );
        };
        // Mirror fetch_sidecars.py: refuse ambiguous multi-pin files.
        if pin.is_some() {
            panic!("multiple yt-dlp pins found in scripts/requirements-sidecars.txt");
        }
        pin = Some(tag);
    }
    pin.unwrap_or_else(|| {
        panic!("no 'yt-dlp==<version>' pin found in scripts/requirements-sidecars.txt")
    })
}

/// PyPI strips leading zeros (2026.08.19 -> 2026.8.19); GitHub release tags
/// zero-pad month and day. Requires 3 numeric segments, a 4-digit year and
/// month 1-12 / day 1-31.
fn normalize_version_tag(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split('.').collect();
    if parts.len() != 3 || parts[0].len() != 4 {
        return None;
    }
    if !parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let nums: Vec<u32> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    let [year, month, day] = nums[..] else {
        return None;
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year}.{month:02}.{day:02}"))
}

/// ffmpeg pin: find the `FFMPEG_VERSION` assignment in
/// scripts/fetch_sidecars.py and take the first quoted value on that line;
/// trailing content (e.g. a comment) is tolerated. Mentions without a quoted
/// value (e.g. the module docstring) are skipped.
fn extract_ffmpeg_version(source: &str) -> String {
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
