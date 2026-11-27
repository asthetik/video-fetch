//! The degraded install claims the process-global tracing subscriber, so it
//! runs as its own integration-test process: in-process it would silence
//! every later capture-based test. CARGO_BIN_EXE_* locates the dedicated
//! probe binary (a lib unit test target cannot see it — cargo only sets the
//! variable for integration tests).

use video_fetch_lib::activity_log;

#[test]
fn install_reports_an_uncreatable_log_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"x").expect("write blocker");
    // A regular file where the logs directory should be: install returns
    // Err (the caller degrades to disabled) — after having installed the
    // stderr subscriber so telemetry survives.
    assert!(activity_log::install(blocker.join("logs"), 1024, 7).is_err());
}
