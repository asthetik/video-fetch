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
        // The marker rides in VF_MARKER instead of the script text: an
        // interpolated marker would sit in powershell.exe's own CommandLine,
        // and the CIM query enumerates every process — caller included — so
        // the scan would always match at least the caller and could never
        // report an empty set. The env var also removes argv quoting hazards
        // for path characters. Literal `Contains` (case and separators
        // normalized) over the full table: a name prefilter cannot see the
        // synthetic sleeper the positive control plants. The full scan is
        // affordable now that the gate parks a downloader until the test
        // releases it and the suite runs serially in CI.
        let out = Command::new("powershell")
            .env("VF_MARKER", marker)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "if (-not $env:VF_MARKER) { Write-Error 'VF_MARKER not set'; exit 2 }\n\
                 $needle = $env:VF_MARKER.Replace('/','\\').ToLowerInvariant()\n\
                 Get-CimInstance Win32_Process | \
                 Where-Object { $_.CommandLine -and $_.CommandLine.Replace('/','\\').ToLowerInvariant().Contains($needle) } | \
                 ForEach-Object { \"$($_.ProcessId)`t$($_.CommandLine)\" }",
            ])
            .output()
            .expect("powershell");
        if !out.status.success() {
            let msg = format!(
                "process scan failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            // Panicking during an unwind aborts the process; Drop-time
            // cleanup stays best-effort.
            if std::thread::panicking() {
                eprintln!("oracles: {msg}");
                return Vec::new();
            }
            panic!("{msg}");
        }
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

/// Best-effort forensic snapshot for failure messages: what the scanner sees
/// right now, plus (Windows) the needle it compared, the size of the process
/// table, and every row whose CommandLine contains the needle — distinguishes
/// "process already gone" from "marker mismatch" without a second CI round
/// trip.
pub fn debug_process_snapshot(marker: &str) -> String {
    let scanned = live_processes_containing(marker);
    #[cfg(unix)]
    {
        format!("scan={scanned:?}")
    }
    #[cfg(windows)]
    {
        let out = Command::new("powershell")
            .env("VF_MARKER", marker)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "if (-not $env:VF_MARKER) { Write-Error 'VF_MARKER not set'; exit 2 }\n\
                 $needle = $env:VF_MARKER.Replace('/','\\').ToLowerInvariant()\n\
                 $procs = Get-CimInstance Win32_Process\n\
                 \"count=$($procs.Count) needle=$needle\"\n\
                 $procs | ForEach-Object { $c = $_.CommandLine; if ($c -and $c.Replace('/','\\').ToLowerInvariant().Contains($needle)) { \"hit[$($_.ProcessId)]=$c\" } }",
            ])
            .output();
        match out {
            Ok(o) => format!(
                "scan={scanned:?} forensic={:?}",
                String::from_utf8_lossy(&o.stdout)
            ),
            Err(e) => format!("scan={scanned:?} forensic-error={e}"),
        }
    }
}

/// A marker-bearing sleeper for the scanner's positive control: its argv
/// carries the marker, so a successful scan proves the scan path itself,
/// independent of yt-dlp's boot and exit timings on load-saturated runners.
pub fn spawn_marker_sleeper(marker: &str) -> std::process::Child {
    #[cfg(unix)]
    {
        std::process::Command::new("sh")
            .args(["-c", "sleep 45"])
            .arg(marker)
            .spawn()
            .expect("spawn marker sleeper")
    }
    #[cfg(windows)]
    {
        // The marker rides in a trailing comment so the script stays valid
        // while the CommandLine carries the token under test.
        let script = format!("Start-Sleep -Seconds 45 # {marker}");
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .spawn()
            .expect("spawn marker sleeper")
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
