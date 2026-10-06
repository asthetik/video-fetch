use std::path::Path;
use std::process::Command;

/// Decodes cleanly and carries both streams. ffprobe is NOT shipped with the
/// sidecars; parse `ffmpeg -i` instead.
pub fn assert_delivered_merged(ffmpeg: &Path, path: &Path) {
    assert!(path.is_file(), "delivered file missing: {}", path.display());
    let decode = Command::new(ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-f", "null", "-"])
        .output()
        .expect("spawn ffmpeg");
    assert!(
        decode.status.success(),
        "decode failed: {}",
        String::from_utf8_lossy(&decode.stderr)
    );
    let probe = Command::new(ffmpeg)
        .arg("-i")
        .arg(path)
        .output()
        .expect("spawn ffmpeg");
    let text = String::from_utf8_lossy(&probe.stderr);
    assert!(
        text.contains("Video:"),
        "no video stream in {}",
        path.display()
    );
    assert!(
        text.contains("Audio:"),
        "no audio stream in {}",
        path.display()
    );
}

/// Work dirs must never keep stray media after a terminal state (fragments,
/// `.part`, `.temp.` leftovers are all failures here).
pub fn assert_no_stray_media_files(work_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(work_dir) else {
        return; // gone = clean
    };
    let stray: Vec<String> = rd
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(stray.is_empty(), "work dir not clean: {stray:?}");
}

/// Media fragments must use the production naming shape `.f<digits>.<ext>`
/// (has_fragment_infix, ytdlp.rs:938). Names like `faud-audio` would slip past
/// the unmerged-stream guard; this check keeps the engine honest about the
/// shape it will meet in production.
pub fn assert_fragment_naming(work_dir: &Path) {
    let Ok(rd) = std::fs::read_dir(work_dir) else {
        return;
    };
    for entry in rd.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Strip the `.part` suffix before the extension test: mid-flight the
        // fragments only exist as `…f<id>.<ext>.part`, so filtering on the raw
        // name would skip every observable fragment and the check would be
        // vacuous (observed: `clip-1.f256.mp4.part`).
        let base = name.trim_end_matches(".part");
        if !(base.ends_with(".mp4")
            || base.ends_with(".m4a")
            || base.ends_with(".mkv")
            || base.ends_with(".webm"))
        {
            continue;
        }
        let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
        let ok = stem
            .rsplit_once(".f")
            .map(|(_, digits)| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or(false);
        assert!(
            ok,
            "fragment {name:?} does not match the production .f<digits> shape"
        );
    }
}

/// Live yt-dlp/ffmpeg processes we spawned: every one carries the harness's
/// unique work-root path in its argv. argv matching is more reliable than
/// pid-parentage — it survives reparenting and behaves the same on Windows.
pub fn live_processes_containing(marker: &str) -> Vec<(u32, String)> {
    #[cfg(unix)]
    {
        let out = Command::new("ps")
            .args(["-axww", "-o", "pid=,command="])
            .output()
            .expect("ps");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains(marker))
            .filter_map(|l| {
                let (pid, cmd) = l.trim().split_once(' ')?;
                Some((pid.parse().ok()?, cmd.trim().to_string()))
            })
            .collect()
    }
    #[cfg(windows)]
    {
        // Literal `Contains`, not a wildcard pattern: path characters stay safe.
        let script = format!(
            "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -and $_.CommandLine.Contains('{marker}') }} | ForEach-Object {{ \"$($_.ProcessId)`t$($_.CommandLine)\" }}"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()
            .expect("powershell");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                let (pid, cmd) = l.trim().split_once('\t')?;
                Some((pid.parse().ok()?, cmd.to_string()))
            })
            .collect()
    }
}

/// Poll until at least one process of this harness is alive (the scanner's
/// positive control).
pub async fn wait_for_live_processes(marker: &str, timeout: std::time::Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !live_processes_containing(marker).is_empty() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Poll until no process of this harness is left alive.
pub async fn wait_no_stray_processes(marker: &str, timeout: std::time::Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let live = live_processes_containing(marker);
        if live.is_empty() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            let detail: Vec<String> = live
                .iter()
                .map(|(pid, cmd)| format!("{pid}: {cmd}"))
                .collect();
            panic!("stray download processes:\n{}", detail.join("\n"));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// SIGKILL every leftover downloader of this harness (teardown and crash reaping).
pub fn kill_processes_containing(marker: &str) {
    for (pid, _) in live_processes_containing(marker) {
        #[cfg(unix)]
        {
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
    }
}
