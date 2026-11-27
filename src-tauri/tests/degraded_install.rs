//! The degraded install claims the process-global tracing subscriber, so it
//! runs as its own integration-test process: in-process it would silence
//! every later capture-based test. CARGO_BIN_EXE_* locates the dedicated
//! probe binary (this cargo version keeps the bin name's hyphens in the
//! variable name). The variable is runtime-injected, so the path is read
//! with std::env::var — a compile-time env! would break `cargo clippy
//! --all-targets`, which checks this target without the bin in its env.

#[test]
fn install_reports_an_uncreatable_log_directory() {
    let probe = std::env::var("CARGO_BIN_EXE_degraded-install-probe")
        .expect("cargo sets CARGO_BIN_EXE_* for integration tests");
    let output = std::process::Command::new(probe)
        .output()
        .expect("probe binary built by cargo");
    assert!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains("degraded install probe ok"),
        "the subprocess probe must run the degraded install and pass: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
