//! Subprocess probe for `activity_log::install`'s degraded path: the
//! degraded install claims the process-global tracing subscriber, so the
//! unit test that exercises it runs this binary instead of doing it
//! in-process (in-process would silence every later capture-based test).

fn main() {
    let dir = std::env::temp_dir().join(format!("vf-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let blocker = dir.join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    // A regular file where the logs directory should be: install returns
    // Err (the caller degrades to disabled) — after having installed the
    // stderr subscriber so telemetry survives.
    // logs must live under the blocker file so the directory creation
    // fails — that is the degraded path being probed.
    assert!(video_fetch_lib::activity_log::install(blocker.join("logs"), 1024, 7).is_err());
    println!("degraded install probe ok");
    std::fs::remove_dir_all(&dir).ok();
}
