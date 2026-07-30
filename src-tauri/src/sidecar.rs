use std::path::PathBuf;
use std::process::Command as StdCommand;

use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

pub const YT_DLP_SIDECAR: &str = "binaries/yt-dlp";
pub const FFMPEG_SIDECAR: &str = "binaries/ffmpeg";

fn sidecar_filename(base_name: &str) -> String {
    if cfg!(windows) {
        format!("{base_name}.exe")
    } else {
        base_name.to_string()
    }
}

pub(crate) fn sidecar_in_dir(dir: &std::path::Path, base_name: &str) -> Option<PathBuf> {
    let path = dir.join(sidecar_filename(base_name));
    if path.is_file() { Some(path) } else { None }
}

fn sidecar_next_to_current_exe(base_name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    sidecar_in_dir(dir, base_name)
}

/// Resolve a bundled sidecar binary path via Tauri shell (production/dev bundle layout).
fn bundled_sidecar_path(app: &AppHandle, sidecar_name: &str) -> Option<PathBuf> {
    let std_cmd: StdCommand = app.shell().sidecar(sidecar_name).ok()?.into();
    let path = PathBuf::from(std_cmd.get_program());
    if path.is_file() { Some(path) } else { None }
}

/// Dev fallback: `src-tauri/binaries/{name}-{TARGET_TRIPLE}` when sidecars were fetched locally.
#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn dev_sidecar_path(base_name: &str) -> Option<PathBuf> {
    #[cfg(debug_assertions)]
    {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
        let prefix = format!("{base_name}-");
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn resolve_bundled(app: &AppHandle, sidecar_name: &str, dev_base: &str) -> Option<PathBuf> {
    sidecar_next_to_current_exe(dev_base)
        .or_else(|| bundled_sidecar_path(app, sidecar_name))
        .or_else(|| dev_sidecar_path(dev_base))
}

fn path_from_env(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

fn path_from_which(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

pub(crate) fn pick_tool_path(
    bundled: Option<PathBuf>,
    from_env: Option<PathBuf>,
    from_system: Option<PathBuf>,
    allow_system_fallback: bool,
) -> Option<PathBuf> {
    bundled.or(from_env).or(if allow_system_fallback {
        from_system
    } else {
        None
    })
}

pub fn resolve_yt_dlp_path(app: &AppHandle) -> PathBuf {
    let bundled = resolve_bundled(app, YT_DLP_SIDECAR, "yt-dlp");
    let from_env = path_from_env("YT_DLP_PATH");
    let from_system = path_from_which("yt-dlp");
    pick_tool_path(bundled, from_env, from_system, cfg!(debug_assertions)).unwrap_or_default()
}

pub fn resolve_ffmpeg_path(app: &AppHandle) -> Option<PathBuf> {
    let bundled = resolve_bundled(app, FFMPEG_SIDECAR, "ffmpeg");
    let from_env = path_from_env("FFMPEG_PATH");
    let from_system = path_from_which("ffmpeg");
    pick_tool_path(bundled, from_env, from_system, cfg!(debug_assertions))
}

pub fn resolve_ytdlp_config(app: &AppHandle) -> crate::ytdlp::YtDlpConfig {
    crate::ytdlp::YtDlpConfig {
        yt_dlp_path: resolve_yt_dlp_path(app),
        ffmpeg_path: resolve_ffmpeg_path(app),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_sidecar_path_uses_manifest_binaries_dir() {
        let p = dev_sidecar_path("yt-dlp");
        if let Some(path) = p {
            assert!(path.starts_with(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries")));
        }
    }

    #[test]
    fn pick_tool_path_prefers_bundled_then_env_then_system() {
        let bundled = PathBuf::from("virtual/bundled/yt-dlp");
        let env = PathBuf::from("virtual/env/yt-dlp");
        let system = PathBuf::from("virtual/system/yt-dlp");
        assert_eq!(
            pick_tool_path(
                Some(bundled.clone()),
                Some(env.clone()),
                Some(system.clone()),
                true
            ),
            Some(bundled)
        );
        assert_eq!(
            pick_tool_path(None, Some(env.clone()), Some(system.clone()), true),
            Some(env)
        );
        assert_eq!(
            pick_tool_path(None, None, Some(system.clone()), true),
            Some(system)
        );
    }

    #[test]
    fn pick_tool_path_skips_system_when_fallback_disabled() {
        let system = PathBuf::from("virtual/system/yt-dlp");
        assert_eq!(pick_tool_path(None, None, Some(system), false), None);
    }

    #[test]
    fn sidecar_in_dir_finds_sibling_file() {
        let dir = tempfile::tempdir().unwrap();
        let name = sidecar_filename("yt-dlp");
        let path = dir.path().join(&name);
        std::fs::write(&path, b"x").unwrap();
        assert_eq!(sidecar_in_dir(dir.path(), "yt-dlp"), Some(path));
    }

    #[test]
    fn sidecar_in_dir_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(sidecar_in_dir(dir.path(), "yt-dlp"), None);
    }
}
