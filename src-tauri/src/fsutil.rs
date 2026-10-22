use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::naming::{AUDIO_OUTPUT_EXTS, OUTPUT_EXTS};

/// Restrict a private file to owner-only on Unix (0600).
///
/// The failure is returned rather than dropped: on a filesystem that carries
/// no Unix modes the restriction does not happen, and a caller that ignored
/// that went on to report the file as private. Elsewhere this is a no-op —
/// Windows files inherit the per-user ACL of the profile directory they are
/// created under, and nothing here tightens them further.
pub(crate) fn restrict_private_file_perms(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Restrict a private directory to owner-only on Unix (0700). Same reporting
/// and the same non-Unix no-op as [`restrict_private_file_perms`].
pub(crate) fn restrict_private_dir_perms(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Report a restriction that failed, for callers that cannot act on it.
pub(crate) fn warn_unrestricted(path: &Path, error: &std::io::Error) {
    tracing::warn!(
        "could not restrict {} to owner-only: {error}; it may be readable by \
         other local users",
        path.display()
    );
}

pub fn work_dir_for(work_root: &Path, job_id: &str) -> PathBuf {
    work_root.join(job_id)
}

fn has_output_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    OUTPUT_EXTS
        .iter()
        .chain(AUDIO_OUTPUT_EXTS.iter())
        .any(|ext| lower.ends_with(ext))
}

/// Media files in a finished download's work dir, counted recursively (the
/// output template may put files in a subdirectory). A completed yt-dlp run
/// leaves exactly one; two or more means separate streams stayed as fragments
/// because the merge never ran, even though yt-dlp exited 0.
pub fn count_media_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut count = 0;
    for ent in entries.flatten() {
        let path = ent.path();
        if path.is_file() {
            if let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned())
                && !name.ends_with(".part")
                && has_output_ext(&name)
                && path.metadata().map(|m| m.len() > 0).unwrap_or(false)
            {
                count += 1;
            }
        } else if path.is_dir() {
            count += count_media_files(&path);
        }
    }
    count
}

pub fn find_work_product(work: &Path) -> Option<PathBuf> {
    // First pass prefers real media containers (a thumbnail/subtitle written into the
    // work dir must not be relocated as the download product); fall back to any
    // non-empty non-part file to keep recovery working for unusual outputs.
    find_work_product_pass(work, true).or_else(|| find_work_product_pass(work, false))
}

/// True when the work dir still holds yt-dlp stream fragments (`.f<id>.` names).
/// A finished product must not be relocated while fragments coexist: a killed
/// merge leaves a half-written `….temp.<ext>` next to them, and the temp file
/// alone would pass every other product check. Walks subdirectories like
/// find_work_product: the output template may nest files.
pub fn has_fragment_leftovers(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for ent in entries.flatten() {
        let path = ent.path();
        if path.is_file() {
            if let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned())
                && crate::ytdlp::has_fragment_infix(&name)
            {
                return true;
            }
        } else if path.is_dir() && has_fragment_leftovers(&path) {
            return true;
        }
    }
    false
}

fn find_work_product_pass(work: &Path, media_only: bool) -> Option<PathBuf> {
    let entries = std::fs::read_dir(work).ok()?;
    for ent in entries.flatten() {
        let path = ent.path();
        if path.is_file() {
            let name = path.file_name()?.to_string_lossy();
            // `….temp.<ext>` is yt-dlp's in-flight merge output, not a product:
            // relocating it would deliver a half-merged file (0.4.0 class bug).
            // Anchor on the stem so a real product whose TITLE embeds ".temp."
            // (e.g. `how-to-fix-.temp.-files [BV1xx].mp4`) stays a candidate.
            let is_merge_temp = Path::new(&*name)
                .file_stem()
                .is_some_and(|stem| stem.to_string_lossy().ends_with(".temp"));
            // `….ytdl` is yt-dlp's fragment-resume state sidecar (a JSON index),
            // never a product: an early pause leaves only `<stream>.part` +
            // `<stream>.ytdl`, and relocating the sidecar delivered a 50-byte
            // JSON as the finished download (pause→resume bug).
            if !name.ends_with(".part")
                && !name.ends_with(".ytdl")
                && !is_merge_temp
                && path.metadata().ok()?.len() > 0
                && (!media_only || has_output_ext(&name))
            {
                return Some(path);
            }
        } else if path.is_dir()
            && let Some(found) = find_work_product_pass(&path, media_only)
        {
            return Some(found);
        }
    }
    None
}

/// Test-only control for the cross-volume delivery path. Production never
/// arms it: the integration suite forces the copy fallback, which a unit test
/// cannot reach (`rename` succeeds within one volume).
#[cfg(feature = "test-utils")]
pub mod delivery_hooks {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ACTIVE: AtomicUsize = AtomicUsize::new(0);

    /// Disarms the hook when the test that armed it ends — including on panic.
    /// Refcounted: tests run in parallel, and one finishing must not disarm
    /// another's still-running delivery.
    pub struct ForceCopyGuard;

    impl Drop for ForceCopyGuard {
        fn drop(&mut self) {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Route deliveries through the cross-volume copy fallback.
    pub fn force_copy_delivery() -> ForceCopyGuard {
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        ForceCopyGuard
    }

    pub(crate) fn forced() -> bool {
        ACTIVE.load(Ordering::SeqCst) > 0
    }
}

pub fn relocate_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    if dest.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("destination exists: {}", dest.display()),
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(feature = "test-utils")]
    let renamed = if delivery_hooks::forced() {
        Err(std::io::Error::other("test: forced cross-volume delivery"))
    } else {
        std::fs::rename(src, dest)
    };
    #[cfg(not(feature = "test-utils"))]
    let renamed = std::fs::rename(src, dest);
    match renamed {
        Ok(()) => {
            sync_parent_dir(dest);
            Ok(())
        }
        Err(_) => copy_across_volumes(src, dest),
    }
}

/// Where a cross-volume copy is staged: a sibling of `dest`, so the rename
/// that publishes it stays on one volume and stays atomic. The app-owned
/// prefix keeps it out of the way twice over — it is not a name the user
/// would plausibly have in their own save dir, and it does not read as a
/// delivered `<name>.*` to anything scanning for one.
///
/// Every attempt gets its own name. Two downloads of the same title can
/// deliver to the same destination at once; a shared staging name would let
/// one attempt rename the other's still-growing copy into place (publishing a
/// truncated product as finished) and truncate the other's in-flight bytes on
/// open. The destination name is kept byte-for-byte: a lossy conversion would
/// fold distinct non-UTF-8 names onto one staging name.
fn copy_temp_for(dest: &Path) -> std::io::Result<PathBuf> {
    let Some(name) = dest.file_name() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("destination has no file name: {}", dest.display()),
        ));
    };
    let mut tmp = OsString::from(".videofetch-");
    tmp.push(name);
    tmp.push(format!(".{}.tmp", staging_suffix()));
    Ok(dest.with_file_name(tmp))
}

/// Unique per attempt within and across processes: pid, nanoseconds and a
/// per-process counter. Uniqueness is a correctness requirement, so creation
/// still uses `create_new`; this suffix only keeps collisions (and retries)
/// from happening at all.
fn staging_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static ATTEMPT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        nanos,
        ATTEMPT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Create the staging file exclusively. An existing path — a stale temp, a
/// concurrent attempt's file, or a planted symlink — is an error, never a
/// truncate and never followed: this attempt only ever writes a file it
/// created itself.
fn create_staging(tmp: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
}

/// The fallback for a `rename` that cannot cross volumes (the save dir on
/// another mount). The copy is staged in a temp and renamed into place,
/// because copying straight to `dest` is only atomic in name: a death
/// mid-copy (SIGKILL, panic abort, power loss) leaves a truncated file where
/// the delivered one belongs. That file then blocks every later attempt: the
/// relocation fails `destination exists`, the row sticks at Failed, `retry`
/// re-enters the same failure, and the finished product sits in the work dir
/// with nowhere to go until the row is cleared and the work dir leaves with
/// it as an orphan. Renaming into place leaves either no `dest` or the whole
/// file. A death mid-copy leaves only a hidden staging file, which no later
/// attempt touches (each picks a fresh name).
fn copy_across_volumes(src: &Path, dest: &Path) -> std::io::Result<()> {
    let mut src_file = std::fs::File::open(src)?;
    let tmp = copy_temp_for(dest)?;
    let mut tmp_file = create_staging(&tmp)?;
    // Land the bytes before the rename publishes them: on a delayed-allocation
    // filesystem the rename can otherwise reach the disk first and leave a
    // zero-length destination — the very blocker this temp exists to avoid.
    // The settings store flushes its own temp-and-rename the same way.
    let copied = std::io::copy(&mut src_file, &mut tmp_file)
        .and_then(|_| {
            let perms = src_file.metadata()?.permissions();
            tmp_file.set_permissions(perms)
        })
        .and_then(|_| tmp_file.sync_all());
    drop(tmp_file);
    if let Err(copy_err) = copied {
        let _ = std::fs::remove_file(&tmp);
        return Err(copy_err);
    }
    if let Err(rename_err) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(rename_err);
    }
    // Flush the published name before the source goes away: the caller
    // removes the work dir right after this returns, and a crash that loses
    // the rename but keeps the deletion would leave no copy at all.
    sync_parent_dir(dest);
    // Same rule as the copy: a source that would not go away removes the
    // destination again, so a retry still sees one complete copy and not two.
    if let Err(remove_err) = std::fs::remove_file(src) {
        let _ = std::fs::remove_file(dest);
        return Err(remove_err);
    }
    Ok(())
}

/// Flush the directory that holds a just-published file. Some filesystems can
/// persist a rename's new name later than the file's data; the source is
/// removed immediately after publication (and the work dir right after that),
/// so a crash in the window would leave neither the source nor the delivered
/// name. Best effort: filesystems that cannot flush a directory reject it with
/// ENOTSUP/EINVAL, and failing every delivery on such a mount would be worse
/// than the durability the filesystem can offer.
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        match std::fs::File::open(parent).and_then(|dir| dir.sync_all()) {
            Ok(()) => {
                #[cfg(test)]
                sync_probe::record(parent);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
                ) => {}
            Err(e) => tracing::warn!(
                "could not flush {} after publishing a download: {e}",
                parent.display()
            ),
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Records which directories the delivery path flushed, so a unit test can
/// pin the call itself: directory durability cannot be observed without a
/// power loss.
#[cfg(all(test, unix))]
mod sync_probe {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    static SYNCED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    pub(super) fn record(path: &Path) {
        SYNCED.lock().unwrap().push(path.to_path_buf());
    }

    pub(super) fn synced(path: &Path) -> bool {
        SYNCED.lock().unwrap().iter().any(|p| p == path)
    }
}

pub fn remove_job_work_dir(work_root: &Path, job_id: &str) -> std::io::Result<()> {
    let dir = work_dir_for(work_root, job_id);
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// The exact OS encoding of `path`, for database columns that must outlive a
/// lossy `to_string_lossy`. Platform-native and stable across app upgrades:
/// Unix raw bytes, Windows UTF-16LE code units.
pub(crate) fn path_to_record_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
}

/// Inverse of `path_to_record_bytes`, for bytes this app recorded on this
/// platform. A malformed (odd-length) Windows record drops its last byte
/// rather than mispairing code units.
pub(crate) fn path_from_record_bytes(bytes: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        PathBuf::from(std::ffi::OsString::from_wide(&units))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The perms helpers are the only thing keeping a private file or
    /// directory out of other users' reach. The mode is read back after a
    /// deliberately permissive setup so the assertion cannot hold with the
    /// restriction removed.
    #[cfg(unix)]
    #[test]
    fn restrict_private_file_perms_takes_a_file_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.json");
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        restrict_private_file_perms(&path).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn restrict_private_dir_perms_takes_a_directory_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir(&logs).unwrap();
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o755)).unwrap();

        restrict_private_dir_perms(&logs).unwrap();

        assert_eq!(
            fs::metadata(&logs).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// Dropping the error meant a filesystem without Unix modes left the file
    /// readable by every local user while its caller reported it private.
    #[cfg(unix)]
    #[test]
    fn restrict_private_perms_reports_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("not-there");

        assert!(restrict_private_file_perms(&missing).is_err());
        assert!(restrict_private_dir_perms(&missing).is_err());
    }

    #[test]
    fn work_dir_for_joins_job_id() {
        let root = PathBuf::from("virtual-download-work");
        assert_eq!(work_dir_for(&root, "job-abc"), root.join("job-abc"));
    }

    #[test]
    fn remove_job_work_dir_ok_when_missing() {
        let root = tempfile::tempdir().unwrap();
        remove_job_work_dir(root.path(), "missing-job").unwrap();
    }

    #[test]
    fn remove_job_work_dir_removes_existing_dir() {
        let root = tempfile::tempdir().unwrap();
        let work = work_dir_for(root.path(), "job-1");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("part.mp4"), b"x").unwrap();
        remove_job_work_dir(root.path(), "job-1").unwrap();
        assert!(!work.exists());
    }

    #[test]
    fn find_work_product_skips_part_files() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("job-1");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("clip.mp4.part"), b"x").unwrap();
        fs::write(work.join("clip.mp4"), b"video").unwrap();
        assert_eq!(find_work_product(&work), Some(work.join("clip.mp4")));
    }

    #[test]
    fn find_work_product_finds_nested_file() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("job-1");
        let nested = work.join("sub");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("out.mkv"), b"video").unwrap();
        assert_eq!(find_work_product(&work), Some(nested.join("out.mkv")));
    }

    #[test]
    fn find_work_product_prefers_media_over_sidecar_files() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("job-1");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("cover.jpg"), b"thumb").unwrap();
        fs::write(work.join("clip.mp4"), b"video").unwrap();
        assert_eq!(find_work_product(&work), Some(work.join("clip.mp4")));
    }

    #[test]
    fn find_work_product_keeps_a_title_that_embeds_dot_temp() {
        // Only `<stem>.temp.<ext>` is yt-dlp's in-flight merge file; a finished
        // product whose title merely contains ".temp." must stay a candidate.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("how-to-fix-.temp.-files [BV1xx].mp4"),
            b"final",
        )
        .unwrap();
        assert_eq!(
            find_work_product(dir.path()),
            Some(dir.path().join("how-to-fix-.temp.-files [BV1xx].mp4"))
        );

        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("demo.temp.mp4"), b"half-merged").unwrap();
        assert_eq!(
            find_work_product(temp.path()),
            None,
            "the in-flight merge file must still be excluded"
        );
    }

    /// An early pause leaves the work dir holding only the interrupted
    /// fragment's `.part` file plus yt-dlp's `.ytdl` fragment-resume state
    /// sidecar. Neither is a product; the sidecar is a 50-byte JSON index.
    ///
    /// Value: protects=the resume short-circuit in `run_job`, which relocates a
    /// `find_work_product` hit without re-running the download; fails_when=the
    /// fallback pass accepts `….ytdl` and delivers a broken 50-byte file as a
    /// finished download; why_new=the pause→resume e2e regression exposed this
    /// exact early-pause delivery; seam=none
    #[test]
    fn find_work_product_skips_ytdl_state_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("job-1");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("clip-1.f256.mp4.part"), b"").unwrap();
        fs::write(
            work.join("clip-1.f256.mp4.ytdl"),
            br#"{"downloader": {"current_fragment": {"index": 0}}}"#,
        )
        .unwrap();
        assert_eq!(
            find_work_product(&work),
            None,
            "the .ytdl state sidecar must not be relocated as a product"
        );
    }

    /// The `.ytdl` skip must not over-reject: a real product sitting next to
    /// the sidecar is still found.
    #[test]
    fn find_work_product_still_finds_a_real_product_next_to_ytdl() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("job-1");
        fs::create_dir_all(&work).unwrap();
        fs::write(work.join("clip-1.f256.mp4.part"), b"").unwrap();
        fs::write(
            work.join("clip-1.f256.mp4.ytdl"),
            br#"{"downloader": {"current_fragment": {"index": 0}}}"#,
        )
        .unwrap();
        fs::write(work.join("clip-1.mp4"), b"final").unwrap();
        assert_eq!(
            find_work_product(&work),
            Some(work.join("clip-1.mp4")),
            "a real product must still be found next to the sidecar"
        );
    }

    /// `run_job` refuses to relocate when stream fragments coexist with a
    /// product: a merge killed mid-flight leaves `….temp.<ext>` next to the
    /// fragments, and the temp file alone would pass every other product check.
    ///
    /// Value: protects=the coexist guard that keeps a half-merged merge file
    /// from being relocated/delivered as a finished download; fails_when=
    /// has_fragment_leftovers misses `.f<id>.<ext>` names or reports false
    /// while a product sits next to fragments; why_new=new helper, and no
    /// fsutil test covers fragments (only `.part` and media-preference cases
    /// exist); seam=none
    #[test]
    fn has_fragment_leftovers_detects_fragments_next_to_a_product() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !has_fragment_leftovers(dir.path()),
            "an empty dir has no leftovers"
        );

        // The in-flight temp file is not a fragment; it is excluded from
        // product candidates separately (find_work_product).
        fs::write(dir.path().join("demo.temp.mp4"), b"half-merged").unwrap();
        assert!(
            !has_fragment_leftovers(dir.path()),
            "a lone in-flight temp file is not a fragment leftover"
        );

        fs::write(dir.path().join("demo.f30016.mp4"), b"frag").unwrap();
        assert!(
            has_fragment_leftovers(dir.path()),
            "a stream fragment is a leftover regardless of other files"
        );

        fs::write(dir.path().join("demo.mp4"), b"product").unwrap();
        assert!(
            has_fragment_leftovers(dir.path()),
            "fragments next to a product must still block relocation"
        );
    }

    #[test]
    fn has_fragment_leftovers_walks_subdirs() {
        // The output template may nest files; find_work_product recurses, so
        // the fragment scan must too or the run_job coexist guard no-ops.
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("uploader");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("demo.mp4"), b"product").unwrap();
        fs::write(sub.join("demo.f30016.mp4"), b"frag").unwrap();
        assert!(has_fragment_leftovers(dir.path()));

        let clean = tempfile::tempdir().unwrap();
        assert!(!has_fragment_leftovers(
            clean.path().join("missing").as_path()
        ));
    }

    #[test]
    fn count_media_files_counts_containers_only() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("clip.mp4"), b"video").unwrap();
        fs::write(dir.path().join("clip.f30280.m4a"), b"audio").unwrap();
        fs::write(dir.path().join("clip.mp4.part"), b"x").unwrap();
        fs::write(dir.path().join("cover.jpg"), b"thumb").unwrap();
        fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        fs::write(dir.path().join("empty.mp4"), b"").unwrap();
        assert_eq!(count_media_files(dir.path()), 2);
    }

    #[test]
    fn count_media_files_walks_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("uploader");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("clip.mp4"), b"video").unwrap();
        assert_eq!(count_media_files(dir.path()), 1);
    }

    #[test]
    fn count_media_files_zero_when_dir_missing() {
        assert_eq!(count_media_files(Path::new("virtual/missing-dir")), 0);
    }

    #[test]
    fn relocate_file_moves_within_same_dir_tree() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/a.mp4");
        let dest = dir.path().join("dest/sub/a.mp4");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"video").unwrap();
        relocate_file(&src, &dest).unwrap();
        assert!(dest.is_file());
        assert!(!src.exists());
        assert_eq!(fs::read(&dest).unwrap(), b"video");
    }

    /// The cross-volume fallback publishes a rename, so the destination is
    /// only ever the whole file — that is what a death mid-copy cannot
    /// guarantee, and the truncated file it leaves blocks every later attempt
    /// (see `copy_across_volumes`). These drive the fallback directly: a unit
    /// test cannot make `rename` fail with EXDEV.
    ///
    /// The death window itself is unpinnable here — it needs the process to
    /// die between the copy's first byte and its last, which no assertion can
    /// stage. What is pinned is the mechanism that closes it: delivery through
    /// a temp, no leftovers, and the temp's own failure paths.
    fn cross_volume_fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("work/a.mp4");
        let dest = dir.path().join("save/sub/a.mp4");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&src, b"video").unwrap();
        (dir, src, dest)
    }

    fn staging_leftovers(dir: &Path) -> Vec<PathBuf> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with(".videofetch-"))
                    .unwrap_or(false)
            })
            .collect()
    }

    /// The exact-path record must round-trip a name UTF-8 cannot carry: the
    /// display string melts it to U+FFFD, and the record is what lets "open"
    /// reach the real file.
    #[cfg(unix)]
    #[test]
    fn recorded_path_bytes_roundtrip_a_non_utf8_name() {
        use std::os::unix::ffi::OsStrExt;
        let path = PathBuf::from("/save").join(std::ffi::OsStr::from_bytes(b"bad-\xff-name.mp4"));
        assert!(path.to_str().is_none());
        let bytes = path_to_record_bytes(&path);
        assert_eq!(bytes, b"/save/bad-\xff-name.mp4");
        assert_eq!(path_from_record_bytes(&bytes), path);
    }

    #[test]
    fn recorded_path_bytes_roundtrip_a_utf8_path() {
        let path = PathBuf::from("/save/clip.mp4");
        assert_eq!(path_from_record_bytes(&path_to_record_bytes(&path)), path);
    }

    #[test]
    fn copy_across_volumes_delivers_the_whole_file_and_no_temp() {
        let (_dir, src, dest) = cross_volume_fixture();
        copy_across_volumes(&src, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"video");
        assert!(
            !src.exists(),
            "the source must not outlive a delivered copy"
        );
        assert!(
            staging_leftovers(dest.parent().unwrap()).is_empty(),
            "the temp must not survive a successful copy"
        );
    }

    /// The temp must not be mistakable for the delivered file: the save dir
    /// may hold a user's own `<name>.tmp`, and the harness and soak oracles
    /// list the save dir by `<name>.*` prefix.
    #[test]
    fn copy_temp_does_not_collide_with_a_user_file_or_a_product_scan() {
        let (_dir, _src, dest) = cross_volume_fixture();
        let tmp = copy_temp_for(&dest).unwrap();
        assert_ne!(tmp, dest.with_file_name("a.mp4.tmp"));
        assert!(
            !tmp.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("a.mp4."),
            "a leftover temp must not read as a delivered a.mp4 to a prefix scan"
        );
    }

    /// Two attempts at the same destination must never share a staging file:
    /// an attempt that renames another's still-growing temp publishes a
    /// truncated product as finished, and a truncating second writer corrupts
    /// the first's in-flight copy.
    #[test]
    fn copy_temp_for_uses_a_fresh_name_for_every_attempt() {
        let (_dir, _src, dest) = cross_volume_fixture();
        let a = copy_temp_for(&dest).unwrap();
        let b = copy_temp_for(&dest).unwrap();
        assert_ne!(a, b, "two attempts must not share a staging file");
    }

    /// A staging file that belongs to someone else — a concurrent attempt in
    /// this or another process — must survive a delivery untouched: the
    /// attempt picks its own name instead of truncating the one it finds.
    #[test]
    fn copy_across_volumes_never_touches_another_attempts_staging_file() {
        let (_dir, src, dest) = cross_volume_fixture();
        let other = copy_temp_for(&dest).unwrap();
        fs::write(&other, b"another attempt in flight").unwrap();
        copy_across_volumes(&src, &dest).unwrap();
        assert_eq!(
            fs::read(&other).unwrap(),
            b"another attempt in flight",
            "a delivery must not truncate or remove another attempt's staging file"
        );
        assert_eq!(fs::read(&dest).unwrap(), b"video");
    }

    /// A lossy conversion folds distinct non-UTF-8 destination names onto the
    /// same staging name — the collision this staging scheme exists to avoid.
    #[cfg(unix)]
    #[test]
    fn copy_temp_for_preserves_a_non_utf8_destination_name() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let raw = b"bad-\xff-name.mp4";
        let dest = dir.path().join(std::ffi::OsStr::from_bytes(raw));
        let tmp = copy_temp_for(&dest).unwrap();
        let bytes = tmp.file_name().unwrap().as_bytes();
        assert!(
            bytes.windows(raw.len()).any(|w| w == raw),
            "staging name must keep the destination's original bytes, got {:?}",
            tmp.file_name()
        );
    }

    /// Two deliveries that pass the destination-exists check together must
    /// each publish their own complete copy; the destination ends as one
    /// complete file, never a mixture or a truncated one, and no staging
    /// files survive.
    #[test]
    fn concurrent_deliveries_to_one_destination_leave_one_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("save/a.mp4");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let src_a = dir.path().join("work-a/a.mp4");
        let src_b = dir.path().join("work-b/a.mp4");
        fs::create_dir_all(src_a.parent().unwrap()).unwrap();
        fs::create_dir_all(src_b.parent().unwrap()).unwrap();
        let a = vec![b'A'; 1 << 20];
        let b = vec![b'B'; 1 << 20];
        fs::write(&src_a, &a).unwrap();
        fs::write(&src_b, &b).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let spawn = |src: PathBuf, dest: PathBuf| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                copy_across_volumes(&src, &dest)
            })
        };
        let t1 = spawn(src_a, dest.clone());
        let t2 = spawn(src_b, dest.clone());
        t1.join().unwrap().unwrap();
        t2.join().unwrap().unwrap();

        let got = fs::read(&dest).unwrap();
        assert!(
            got == a || got == b,
            "destination must be exactly one complete delivery, got {} bytes",
            got.len()
        );
        assert!(
            staging_leftovers(dest.parent().unwrap()).is_empty(),
            "staging files left behind: {:?}",
            staging_leftovers(dest.parent().unwrap())
        );
    }

    #[test]
    fn copy_across_volumes_leaves_no_destination_when_the_copy_fails() {
        let (_dir, src, dest) = cross_volume_fixture();
        fs::remove_file(&src).unwrap();
        assert!(copy_across_volumes(&src, &dest).is_err());
        assert!(
            !dest.exists(),
            "a failed copy must not leave a file where the delivered one belongs"
        );
        assert!(
            staging_leftovers(dest.parent().unwrap()).is_empty(),
            "a failed copy must not leave its staging file behind"
        );
    }

    /// The source not going away removes the destination again, so a retry
    /// sees one complete copy rather than two files with the same name.
    ///
    /// The error kind is asserted so a failure raised earlier — the copy
    /// itself, rather than the unlink this test stages — cannot satisfy it.
    /// Under euid 0 the unlink succeeds and this fails outright, which is the
    /// same exposure the other permission tests here carry; CI runs unprivileged.
    #[cfg(unix)]
    #[test]
    fn copy_across_volumes_removes_the_destination_when_the_source_stays() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, src, dest) = cross_volume_fixture();
        // A read-only parent lets the copy read the source but not unlink it.
        let src_dir = src.parent().unwrap();
        fs::set_permissions(src_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let result = copy_across_volumes(&src, &dest);
        fs::set_permissions(src_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the staged failure is the unlink, not the copy"
        );
        assert!(src.exists());
        assert!(
            !dest.exists(),
            "a source that stayed must take the destination with it"
        );
        assert!(
            staging_leftovers(dest.parent().unwrap()).is_empty(),
            "a failed delivery must not leave its staging file behind"
        );
    }

    /// The staging file is created exclusively: an existing path must fail
    /// creation rather than be truncated, so an attempt can never damage a
    /// file it did not create.
    #[test]
    fn create_staging_refuses_an_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".videofetch-a.mp4.deadbeef.tmp");
        fs::write(&path, b"keep").unwrap();
        let err = create_staging(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"keep");
    }

    /// `create_new` also refuses through a planted symlink; following it
    /// would let the copy write through to the link's target.
    #[cfg(unix)]
    #[test]
    fn create_staging_does_not_follow_a_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"target content").unwrap();
        let path = dir.path().join(".videofetch-a.mp4.deadbeef.tmp");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let err = create_staging(&path).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"target content");
    }

    /// The delivered file must be durable before the caller drops the work
    /// dir: the rename that published it needs its parent directory flushed,
    /// or a crash can leave neither copy.
    #[cfg(unix)]
    #[test]
    fn relocate_file_syncs_the_destination_parent_before_returning() {
        let (_dir, src, dest) = cross_volume_fixture();
        relocate_file(&src, &dest).unwrap();
        assert!(
            sync_probe::synced(dest.parent().unwrap()),
            "the destination parent must be flushed after a same-volume rename"
        );
    }

    /// Same rule for the cross-volume fallback: the flush must happen before
    /// the source is removed.
    #[cfg(unix)]
    #[test]
    fn copy_across_volumes_syncs_the_destination_parent() {
        let (_dir, src, dest) = cross_volume_fixture();
        copy_across_volumes(&src, &dest).unwrap();
        assert!(
            sync_probe::synced(dest.parent().unwrap()),
            "the destination parent must be flushed after the fallback rename"
        );
    }
}
