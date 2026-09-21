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

/// Where a resolved binary came from, for the log line that explains a fallback.
const ORIGIN_BUNDLED: &str = "内置";
const ORIGIN_ENV: &str = "环境变量";
const ORIGIN_SYSTEM: &str = "系统 PATH";

const TOOL_YT_DLP: &str = "yt-dlp";
const TOOL_FFMPEG: &str = "ffmpeg";

/// ffmpeg answers `-version`. yt-dlp's parser reads that as `-v ersion` and
/// exits 2, so a shared flag would reject every healthy yt-dlp.
const FFMPEG_VERSION_FLAG: &str = "-version";
const YT_DLP_VERSION_FLAG: &str = "--version";

/// One tool's candidates, already looked up so resolution stays a pure function
/// over paths.
struct ToolCandidates {
    label: &'static str,
    version_flag: &'static str,
    bundled: Option<PathBuf>,
    from_env: Option<PathBuf>,
    from_system: Option<PathBuf>,
}

/// Only the dev build may use a binary from PATH: a release must ship the
/// bundled one, and a broken bundle has to surface as the missing-sidecar error
/// rather than silently running whatever the user happens to have installed.
fn system_candidate(name: &str) -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        path_from_which(name)
    } else {
        None
    }
}

/// Spawn a candidate with its version flag. A binary that cannot execute fails
/// here with the OS error, which is how the 0.4.0 macOS build shipped an ffmpeg
/// that could never merge: Intel-only, so `exec format error` on Apple silicon.
fn probe(path: &std::path::Path, version_flag: &str) -> Result<(), String> {
    let mut cmd = StdCommand::new(path);
    cmd.arg(version_flag)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(match status.code() {
            Some(code) => format!("退出码 {code}"),
            None => "被信号终止".to_string(),
        }),
        Err(e) => Err(e.to_string()),
    }
}

/// Probe one candidate, naming the ones that exist but cannot run. Skipping in
/// silence is what kept a broken sidecar invisible until a download failed.
fn usable(
    candidate: Option<PathBuf>,
    label: &str,
    origin: &str,
    version_flag: &str,
) -> Option<PathBuf> {
    let path = candidate?;
    match probe(&path, version_flag) {
        Ok(()) => Some(path),
        Err(reason) => {
            tracing::warn!(
                target: "core",
                "sidecar: {label} 无法运行（{origin}），跳过：{}：{reason}",
                path.display()
            );
            None
        }
    }
}

/// Walk the candidates in preference order and return the first that runs.
/// Probes are lazy: a working bundled binary never spawns the discarded ones.
fn resolve_candidates(spec: ToolCandidates) -> Option<PathBuf> {
    let ToolCandidates {
        label,
        version_flag,
        bundled,
        from_env,
        from_system,
    } = spec;
    let bundled_present = bundled.is_some();
    for (origin, candidate) in [
        (ORIGIN_BUNDLED, bundled),
        (ORIGIN_ENV, from_env),
        (ORIGIN_SYSTEM, from_system),
    ] {
        if let Some(path) = usable(candidate, label, origin, version_flag) {
            // A fallback is a degradation, so it is a warning, not a state
            // transition; a dev run without bundled sidecars simply has no
            // bundled candidate to lose and stays silent.
            if bundled_present && origin != ORIGIN_BUNDLED {
                tracing::warn!(
                    target: "core",
                    "sidecar: {label} 改用{origin}的二进制：{}",
                    path.display()
                );
            }
            return Some(path);
        }
    }
    tracing::warn!(target: "core", "sidecar: {label} 无可用二进制，相关下载会失败");
    None
}

pub fn resolve_yt_dlp_path(app: &AppHandle) -> PathBuf {
    resolve_candidates(ToolCandidates {
        label: TOOL_YT_DLP,
        version_flag: YT_DLP_VERSION_FLAG,
        bundled: resolve_bundled(app, YT_DLP_SIDECAR, TOOL_YT_DLP),
        from_env: path_from_env("YT_DLP_PATH"),
        from_system: system_candidate(TOOL_YT_DLP),
    })
    .unwrap_or_default()
}

pub fn resolve_ffmpeg_path(app: &AppHandle) -> Option<PathBuf> {
    resolve_candidates(ToolCandidates {
        label: TOOL_FFMPEG,
        version_flag: FFMPEG_VERSION_FLAG,
        bundled: resolve_bundled(app, FFMPEG_SIDECAR, TOOL_FFMPEG),
        from_env: path_from_env("FFMPEG_PATH"),
        from_system: system_candidate(TOOL_FFMPEG),
    })
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

    #[cfg(unix)]
    fn write_exec(dir: &std::path::Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn probe_rejects_a_missing_path() {
        let err = probe(
            std::path::Path::new("virtual/missing/ffmpeg"),
            FFMPEG_VERSION_FLAG,
        )
        .unwrap_err();
        assert!(!err.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn probe_reports_the_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let broken = write_exec(dir.path(), "ffmpeg-broken", "#!/bin/sh\nexit 3\n");
        assert_eq!(probe(&broken, FFMPEG_VERSION_FLAG).unwrap_err(), "退出码 3");
    }

    #[cfg(unix)]
    #[test]
    fn probe_honours_each_tools_version_flag() {
        let dir = tempfile::tempdir().unwrap();
        // yt-dlp's parser reads `-version` as `-v ersion` and exits 2, so
        // probing it with ffmpeg's flag would reject a healthy binary.
        let yt_dlp = write_exec(
            dir.path(),
            "yt-dlp",
            "#!/bin/sh\n[ \"$1\" = \"--version\" ] || exit 2\n",
        );
        assert!(probe(&yt_dlp, YT_DLP_VERSION_FLAG).is_ok());
        assert!(probe(&yt_dlp, FFMPEG_VERSION_FLAG).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn resolve_prefers_a_usable_bundled_binary() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = write_exec(dir.path(), "bundled-ffmpeg", "#!/bin/sh\nexit 0\n");
        let from_env = write_exec(dir.path(), "env-ffmpeg", "#!/bin/sh\nexit 0\n");
        assert_eq!(
            resolve_candidates(ToolCandidates {
                label: TOOL_FFMPEG,
                version_flag: FFMPEG_VERSION_FLAG,
                bundled: Some(bundled.clone()),
                from_env: Some(from_env),
                from_system: None,
            }),
            Some(bundled)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_falls_back_when_the_bundled_binary_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = write_exec(dir.path(), "bundled-ffmpeg", "#!/bin/sh\nexit 3\n");
        let from_env = write_exec(dir.path(), "env-ffmpeg", "#!/bin/sh\nexit 0\n");
        assert_eq!(
            resolve_candidates(ToolCandidates {
                label: TOOL_FFMPEG,
                version_flag: FFMPEG_VERSION_FLAG,
                bundled: Some(bundled),
                from_env: Some(from_env.clone()),
                from_system: None,
            }),
            Some(from_env)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_uses_the_system_candidate_last() {
        let dir = tempfile::tempdir().unwrap();
        let system = write_exec(dir.path(), "system-ffmpeg", "#!/bin/sh\nexit 0\n");
        assert_eq!(
            resolve_candidates(ToolCandidates {
                label: TOOL_FFMPEG,
                version_flag: FFMPEG_VERSION_FLAG,
                bundled: None,
                from_env: None,
                from_system: Some(system.clone()),
            }),
            Some(system)
        );
    }

    #[test]
    fn resolve_returns_none_when_no_candidate_is_usable() {
        assert_eq!(
            resolve_candidates(ToolCandidates {
                label: TOOL_FFMPEG,
                version_flag: FFMPEG_VERSION_FLAG,
                bundled: None,
                from_env: None,
                from_system: None,
            }),
            None
        );
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
