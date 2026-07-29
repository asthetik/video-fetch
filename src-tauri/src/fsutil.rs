use std::path::{Path, PathBuf};

pub fn work_dir_for(work_root: &Path, job_id: &str) -> PathBuf {
    work_root.join(job_id)
}

pub fn find_work_product(work: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(work).ok()?;
    for ent in entries.flatten() {
        let path = ent.path();
        if path.is_file() {
            let name = path.file_name()?.to_string_lossy();
            if !name.ends_with(".part") && path.metadata().ok()?.len() > 0 {
                return Some(path);
            }
        } else if path.is_dir()
            && let Some(found) = find_work_product(&path)
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
