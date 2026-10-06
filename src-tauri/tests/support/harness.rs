use std::path::PathBuf;

/// Mirrors sidecar.rs's `dev_sidecar_path`: prefix-scan the fetched binaries
/// dir. Fail loud — anyone running this target should have fetched sidecars.
pub fn find_sidecar(base: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("binaries");
    let prefix = format!("{base}-");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().starts_with(&prefix))
                        .unwrap_or(false)
                        && p.is_file()
                })
                .collect()
        })
        .unwrap_or_else(|e| {
            panic!(
                "read {}: {e}; run python3 scripts/fetch_sidecars.py",
                dir.display()
            )
        });
    found.sort();
    found.into_iter().next().unwrap_or_else(|| {
        panic!(
            "no {base}-* under {}; run python3 scripts/fetch_sidecars.py",
            dir.display()
        )
    })
}
