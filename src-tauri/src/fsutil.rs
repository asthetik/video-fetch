use std::path::{Path, PathBuf};

use crate::naming::{AUDIO_OUTPUT_EXTS, OUTPUT_EXTS};

/// Restrict a private file to owner-only on Unix (0600). No-op elsewhere.
pub(crate) fn restrict_private_file_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Restrict a private directory to owner-only on Unix (0700). No-op elsewhere.
pub(crate) fn restrict_private_dir_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
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
    match std::fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            if let Err(copy_err) = std::fs::copy(src, dest) {
                let _ = std::fs::remove_file(dest);
                return Err(copy_err);
            }
            if let Err(remove_err) = std::fs::remove_file(src) {
                let _ = std::fs::remove_file(dest);
                return Err(remove_err);
            }
            Ok(())
        }
    }
}

pub fn remove_job_work_dir(work_root: &Path, job_id: &str) -> std::io::Result<()> {
    let dir = work_dir_for(work_root, job_id);
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
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

        restrict_private_file_perms(&path);

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

        restrict_private_dir_perms(&logs);

        assert_eq!(
            fs::metadata(&logs).unwrap().permissions().mode() & 0o777,
            0o700
        );
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
}
