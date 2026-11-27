use sha2::Sha256;
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
    env_override(std::env::var(var).ok(), cfg!(debug_assertions))
}

/// Split from the lookup so the gate can be asserted: `cfg!` is fixed at
/// compile time, and a test build always takes the permissive branch.
fn env_override(value: Option<String>, allowed: bool) -> Option<PathBuf> {
    // Debug-only, like the PATH fallback below. A release build must run the
    // bundled sidecar: its digest is what `scripts/sidecar_pins.json` fixes,
    // and an override here would put an arbitrary binary on that trust path.
    // It also keeps the version floor honest — an overridden yt-dlp is not
    // guaranteed to accept every flag `ytdlp.rs` now passes. The README
    // describes these variables as a development convenience.
    if !allowed {
        return None;
    }
    value.map(PathBuf::from).filter(|p| p.is_file())
}

fn path_from_which(name: &str) -> Option<PathBuf> {
    which::which(name).ok()
}

/// Where a resolved binary came from, for the log line that explains a fallback.
const ORIGIN_BUNDLED: &str = "内置";
const ORIGIN_ENV: &str = "环境变量";
const ORIGIN_SYSTEM: &str = "系统 PATH";

pub(crate) const TOOL_YT_DLP: &str = "yt-dlp";
pub(crate) const TOOL_FFMPEG: &str = "ffmpeg";

// --- Runtime integrity ------------------------------------------------------
//
// `sidecar_pins.json` vouches for the shipped binary per target; build.rs
// injects the current target's digest at compile time. A background thread
// hashes the resolved binaries once at startup (tens of milliseconds, off
// the main thread — the version probe's 3s startup cost was the reason a
// probe-free resolution exists and hashing is an order of magnitude under
// it), and the download path refuses to spawn a yt-dlp whose digest does
// not match. ffmpeg is invoked by yt-dlp rather than by us, so a mismatch
// there cannot be blocked — it is logged loudly and the merge failure is
// already caught at the delivery point (`looks_unmerged`).
//
// Env/PATH origins are exempt by design: they are dev-only overrides (a
// release build refuses them) and a system binary was never ours to vouch
// for.

/// Placeholders written synchronously by `spawn_digest_verification` so a
/// download racing the hash thread waits instead of running unverified.
const VERIFYING: &str = "校验中";

static DIGEST_VERDICTS: std::sync::Mutex<
    Option<std::collections::HashMap<&'static str, Result<(), String>>>,
> = std::sync::Mutex::new(None);

fn expected_digest(tool: &str) -> Option<&'static str> {
    match tool {
        TOOL_YT_DLP => option_env!("SIDECAR_SHA256_YT_DLP"),
        TOOL_FFMPEG => option_env!("SIDECAR_SHA256_FFMPEG"),
        _ => None,
    }
}

/// The gate decision, pure so every branch is unit-testable: nothing to
/// vouch for (no pin compiled in) and nothing vouched for (verification
/// never started — the test harness) both pass; only a started verification
/// that failed blocks.
fn digest_gate(expected: bool, verdict: Option<Result<(), String>>) -> Result<(), String> {
    match (expected, verdict) {
        (false, _) => Ok(()),
        (true, None) => Ok(()),
        (true, Some(Ok(()))) => Ok(()),
        (true, Some(Err(e))) => Err(e),
    }
}

fn digest_file(path: &std::path::Path) -> Result<String, String> {
    use sha2::Digest;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn verdicts() -> std::sync::MutexGuard<
    'static,
    Option<std::collections::HashMap<&'static str, Result<(), String>>>,
> {
    DIGEST_VERDICTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Hash the resolved bundled binaries once, off the main thread, and record
/// per-tool verdicts. Called from `build_app_state` before queued work can
/// resume; the placeholders it writes synchronously make a racing download
/// wait for the verdict instead of running an unverified binary.
pub fn spawn_digest_verification(yt_dlp: &std::path::Path, ffmpeg: Option<&std::path::Path>) {
    {
        let mut verdicts = verdicts();
        for tool in [TOOL_YT_DLP, TOOL_FFMPEG] {
            verdicts
                .get_or_insert_with(std::collections::HashMap::new)
                .insert(tool, Err(VERIFYING.into()));
        }
    }
    let yt_dlp = yt_dlp.to_path_buf();
    let ffmpeg = ffmpeg.map(std::path::Path::to_path_buf);
    std::thread::spawn(move || {
        let mut verdicts = verdicts();
        for (tool, path) in [(TOOL_YT_DLP, Some(yt_dlp)), (TOOL_FFMPEG, ffmpeg)] {
            let verdict = match (expected_digest(tool), path) {
                (Some(expected), Some(path)) => match digest_file(&path) {
                    Ok(digest) if digest == expected => Ok(()),
                    Ok(digest) => {
                        let message = format!(
                            "{tool} 完整性校验失败：SHA-256 不匹配（期望 {expected}，实际 {digest}）"
                        );
                        if tool == TOOL_FFMPEG {
                            // yt-dlp spawns ffmpeg itself; the mismatch
                            // cannot be blocked here. The merge failure is
                            // already caught loudly at the delivery point.
                            tracing::warn!(target: "core", "sidecar: {message}（合并失败会在交付点浮出）");
                        }
                        Err(message)
                    }
                    Err(e) => Err(format!("{tool} 完整性校验失败：无法读取（{e}）")),
                },
                _ => Ok(()),
            };
            if let Some(map) = verdicts.as_mut() {
                map.insert(tool, verdict);
            }
        }
    });
}

/// The download path's gate: refuse to spawn a yt-dlp whose digest failed
/// verification. Waits (bounded) while the background hash is still running;
/// a 5s timeout proceeds with a loud warning — hashing 130MB has never come
/// close, and a permanently stalled hash thread must not brick downloads.
pub fn ensure_digest_ok(tool: &str) -> Result<(), String> {
    if expected_digest(tool).is_none() {
        return Ok(());
    }
    for _ in 0..100 {
        let snapshot = {
            let verdicts = verdicts();
            verdicts.as_ref().and_then(|m| m.get(tool).cloned())
        };
        match digest_gate(true, snapshot) {
            Ok(()) => return Ok(()),
            Err(e) if e == VERIFYING => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(e) => return Err(e),
        }
    }
    tracing::warn!(target: "core", "sidecar: {tool} 完整性校验超时，本次执行放行");
    Ok(())
}

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
    match probe_command(path, version_flag).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(match status.code() {
            Some(code) => format!("退出码 {code}"),
            None => "被信号终止".to_string(),
        }),
        Err(e) => Err(e.to_string()),
    }
}

/// Build the argv for a version probe.
///
/// Unlike the invocations in `ytdlp.rs` this one carries no URL and no
/// frontend-supplied value, and yt-dlp answers `--version` while parsing argv,
/// before it loads any configuration file — so it is deliberately left without
/// `--ignore-config`. The flag set is per tool (ffmpeg takes `-version`), so
/// there is nothing to share here either.
fn probe_command(path: &std::path::Path, version_flag: &str) -> StdCommand {
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
    cmd
}

/// Walk the candidates in preference order and return the first that runs,
/// naming the ones that exist but cannot. Skipping in silence is what kept a
/// broken sidecar invisible until a download failed.
fn resolve_candidates(spec: ToolCandidates) -> Option<PathBuf> {
    resolve_with(spec, probe)
}

/// `probe` is the only syscall under here, so callers that cannot spawn (the
/// tests) inject their own and still exercise the walk.
///
/// Probing means spawning the candidate and waiting for it to exit, which is
/// the most expensive thing startup does: a release bundle offers exactly one
/// candidate, so the walk below would only ever be *verifying* it, and yt-dlp
/// is a PyInstaller bundle that takes seconds to unpack and boot just to print
/// a version (measured ~2.8-3.5 s against the shipped macOS build). That cost
/// sits inside the Tauri `setup` hook, which blocks the main thread before the
/// event loop pumps, so it lands directly on time-to-window.
///
/// A single candidate is therefore taken as-is. The shipped binary's digest
/// is verified off the main thread by `spawn_digest_verification` (against
/// the compile-time-injected pin), and the download path refuses a yt-dlp
/// whose verdict failed — see the runtime-integrity block above. ffmpeg is
/// yt-dlp's to spawn, so its mismatch is logged and caught at the delivery
/// point by `looks_unmerged`.
fn resolve_with(
    spec: ToolCandidates,
    probe: impl Fn(&std::path::Path, &str) -> Result<(), String>,
) -> Option<PathBuf> {
    let ToolCandidates {
        label,
        version_flag,
        bundled,
        from_env,
        from_system,
    } = spec;
    let bundled_present = bundled.is_some();
    // Each candidate was already existence-checked where it was looked up, so
    // everything that survives here is a real file.
    let present = [
        (ORIGIN_BUNDLED, bundled),
        (ORIGIN_ENV, from_env),
        (ORIGIN_SYSTEM, from_system),
    ]
    .into_iter()
    .filter_map(|(origin, candidate)| candidate.map(|path| (origin, path)))
    .collect::<Vec<_>>();

    // Nothing to choose between: do not pay a spawn to confirm it. The walk
    // below logs as it goes; with a single candidate there is no degradation to
    // report, so this path stays as silent as a healthy bundled binary was.
    if let [(_, only)] = present.as_slice() {
        return Some(only.clone());
    }

    // Candidates are probed lazily: a working bundled binary never spawns the
    // discarded ones.
    for (origin, path) in present {
        match probe(&path, version_flag) {
            Ok(()) => {
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
            Err(reason) => tracing::warn!(
                target: "core",
                "sidecar: {label} 无法运行（{origin}），跳过：{}：{reason}",
                path.display()
            ),
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

    /// A release build must run the sidecar the pins file vouches for. An
    /// override in the environment would let anything that can set a variable
    /// choose the binary instead, and would put an unvetted yt-dlp — one that
    /// need not accept the flags `ytdlp.rs` passes — on the download path.
    #[test]
    fn digest_file_hashes_known_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.bin");
        std::fs::write(&path, b"abc").unwrap();
        // NIST test vector for SHA-256("abc").
        assert_eq!(
            digest_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(digest_file(&dir.path().join("absent")).is_err());
    }

    #[test]
    fn the_download_path_gates_on_the_digest_verdict() {
        // CRLF-normalized: Windows checkouts hand include_str CRLF source.
        let src = include_str!("ytdlp.rs").replace("\r\n", "\n");
        let needle = ["crate::sidecar::ensure_digest_ok(crate::sidecar::TOOL_YT_DLP)"].concat();
        assert!(
            src.contains(&needle),
            "the download path must gate on the digest verdict; without the gate a failed verification never blocks a spawn"
        );
    }

    #[test]
    fn digest_gate_blocks_only_a_failed_verdict() {
        // No pin compiled in: everything passes, nothing can fail.
        assert!(digest_gate(false, None).is_ok());
        assert!(digest_gate(false, Some(Err("篡改".into()))).is_ok());
        // Pin present: no verdict (harness) and a passing verdict pass.
        assert!(digest_gate(true, None).is_ok());
        assert!(digest_gate(true, Some(Ok(()))).is_ok());
        assert!(digest_gate(true, Some(Err("校验失败".into()))).is_err());
    }

    #[test]
    fn a_release_build_ignores_the_sidecar_override() {
        let file = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        assert!(env_override(Some(file.clone()), true).is_some());
        assert!(env_override(Some(file), false).is_none());
        // A value that is not a file is refused either way.
        assert!(env_override(Some("/nonexistent/yt-dlp".into()), true).is_none());
    }

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

    /// The flag is per tool, and sharing one is a real trap rather than a
    /// style choice: yt-dlp's parser reads `-version` as `-v ersion` and
    /// exits 2, so ffmpeg's flag would reject a healthy yt-dlp.
    #[test]
    fn each_tool_probes_with_its_own_version_flag() {
        let path = std::path::Path::new("/virtual/yt-dlp");
        assert_eq!(
            probe_command(path, YT_DLP_VERSION_FLAG)
                .get_args()
                .collect::<Vec<_>>(),
            ["--version"]
        );
        assert_eq!(
            probe_command(path, FFMPEG_VERSION_FLAG)
                .get_args()
                .collect::<Vec<_>>(),
            ["-version"]
        );
    }

    /// Resolution with a probe that answers by path, so the walk and its
    /// fallbacks are covered without spawning anything: a spawn that fails for
    /// reasons of the machine would otherwise read as a resolution bug.
    fn resolve_with_fake_probe(
        bundled: Option<&str>,
        from_env: Option<&str>,
        from_system: Option<&str>,
        usable: &[&str],
    ) -> Option<PathBuf> {
        let spec = ToolCandidates {
            label: TOOL_FFMPEG,
            version_flag: FFMPEG_VERSION_FLAG,
            bundled: bundled.map(PathBuf::from),
            from_env: from_env.map(PathBuf::from),
            from_system: from_system.map(PathBuf::from),
        };
        resolve_with(spec, |path, _flag| {
            if usable.iter().any(|u| path == std::path::Path::new(u)) {
                Ok(())
            } else {
                Err("无法执行".to_string())
            }
        })
    }

    #[test]
    fn resolve_prefers_a_usable_bundled_binary() {
        assert_eq!(
            resolve_with_fake_probe(
                Some("/bundled/ffmpeg"),
                Some("/env/ffmpeg"),
                None,
                &["/bundled/ffmpeg", "/env/ffmpeg"],
            ),
            Some(PathBuf::from("/bundled/ffmpeg"))
        );
    }

    #[test]
    fn resolve_falls_back_when_the_bundled_binary_cannot_run() {
        assert_eq!(
            resolve_with_fake_probe(
                Some("/bundled/ffmpeg"),
                Some("/env/ffmpeg"),
                None,
                &["/env/ffmpeg"],
            ),
            Some(PathBuf::from("/env/ffmpeg"))
        );
    }

    #[test]
    fn resolve_uses_the_system_candidate_last() {
        assert_eq!(
            resolve_with_fake_probe(
                Some("/bundled/ffmpeg"),
                Some("/env/ffmpeg"),
                Some("/system/ffmpeg"),
                &["/system/ffmpeg"],
            ),
            Some(PathBuf::from("/system/ffmpeg"))
        );
    }

    /// A release bundle resolves to exactly one candidate (both the env
    /// override and the PATH lookup are dev-only), and spawning that candidate
    /// to prove it runs is what put seconds on startup. With nothing to choose
    /// between, it must be taken as-is.
    #[test]
    fn a_lone_candidate_is_taken_without_being_spawned() {
        let spec = ToolCandidates {
            label: TOOL_YT_DLP,
            version_flag: YT_DLP_VERSION_FLAG,
            bundled: Some(PathBuf::from("/bundled/yt-dlp")),
            from_env: None,
            from_system: None,
        };
        let spawned = std::cell::Cell::new(false);

        let resolved = resolve_with(spec, |_, _| {
            spawned.set(true);
            Ok(())
        });

        assert_eq!(resolved, Some(PathBuf::from("/bundled/yt-dlp")));
        assert!(!spawned.get(), "a lone candidate must not be spawned");
    }

    #[test]
    fn resolve_returns_none_when_no_candidate_is_usable() {
        assert_eq!(
            resolve_with_fake_probe(
                Some("/bundled/ffmpeg"),
                Some("/env/ffmpeg"),
                Some("/system/ffmpeg"),
                &[],
            ),
            None
        );
    }

    #[test]
    fn resolve_returns_none_when_there_are_no_candidates() {
        assert_eq!(resolve_with_fake_probe(None, None, None, &[]), None);
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
