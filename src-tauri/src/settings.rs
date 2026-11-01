use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{AppError, AppResult};
use crate::models::AppSettings;

const SETTINGS_FILE: &str = "settings.json";

pub fn settings_path(app_dir: &Path) -> PathBuf {
    app_dir.join(SETTINGS_FILE)
}

pub fn load_settings(app_dir: &Path) -> AppResult<AppSettings> {
    let path = settings_path(app_dir);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AppSettings::default());
        }
        Err(e) => {
            // Same reasoning as a file that parses badly, one step earlier:
            // `setup` propagates this, and propagating it means the app never
            // opens again.
            eprintln!(
                "videofetch: cannot read {} ({e}); starting from default settings",
                path.display()
            );
            return Ok(AppSettings::default());
        }
    };
    let mut settings: AppSettings = match serde_json::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            // A truncated write used to brick the app: setup propagates this
            // error, so every launch died on the same file and the only way
            // out was deleting it by hand. Keep it for diagnosis, start from
            // defaults, and let the next save replace it.
            let backup = path.with_extension("json.bad");
            // `eprintln!` rather than `tracing::warn!`: this runs before the
            // activity-log subscriber is installed, so a warn here is dropped
            // and the user would find their settings reset with no explanation.
            eprintln!(
                "videofetch: {} is not valid JSON ({e}); kept as {} and starting \
                 from default settings",
                path.display(),
                backup.display()
            );
            let _ = fs::rename(&path, &backup);
            return Ok(AppSettings::default());
        }
    };
    if migrate_filename_template(&mut settings) {
        let _ = save_settings(app_dir, &settings);
    }
    Ok(settings)
}

/// Upgrade older compact local-time tokens to `YYYY-MM-DDTHH-MM-SS`.
fn migrate_filename_template(settings: &mut AppSettings) -> bool {
    let mut next = settings.filename_template.clone();
    // Longer compact token first so minute-only replace cannot partially match it.
    next = next.replace(
        "%(timestamp>%Y%m%dT%H-%M-%S)s",
        "%(timestamp>%Y-%m-%dT%H-%M-%S)s",
    );
    next = next.replace(
        "%(timestamp>%Y%m%dT%H-%M)s",
        "%(timestamp>%Y-%m-%dT%H-%M-%S)s",
    );
    if next == settings.filename_template {
        return false;
    }
    settings.filename_template = next;
    true
}

pub fn save_settings(app_dir: &Path, settings: &AppSettings) -> AppResult<()> {
    fs::create_dir_all(app_dir)?;
    let path = settings_path(app_dir);
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| AppError::Message(e.to_string()))?;
    // Written beside the real file and renamed into place: `fs::write` truncates
    // first, so an interrupted save left a settings.json that every later launch
    // failed to parse — before the window ever opened.
    let tmp = path.with_extension("json.tmp");
    let write = (|| -> AppResult<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if write.is_err() {
        // Nothing else ever tidies this directory, so a scratch file left by a
        // failed save would sit there for the life of the install.
        let _ = fs::remove_file(&tmp);
    }
    write
}

/// Concurrency bounds mirrored by the settings page slider
/// (CONCURRENCY_MIN/CONCURRENCY_MAX). IPC input is untrusted, so the
/// slider cannot be the only enforcement.
pub const MIN_CONCURRENCY: u32 = 1;
pub const MAX_CONCURRENCY: u32 = 16;

/// Validate a newly chosen save dir: only an existing, absolute, local
/// directory is accepted. An empty string would download into the process
/// CWD, a relative path does the same from another base, and a UNC path
/// turns the first file operation into an SMB probe with the user's
/// credentials.
pub fn validate_save_dir(raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("保存目录不能为空".into());
    }
    // String-level on purpose: it covers every UNC spelling
    // (`\\server\share`, `\\?\UNC\...`, `//server/share`) on every
    // platform, so the tests exercise it on non-Windows runners too.
    if raw.starts_with("\\\\") || raw.starts_with("//") {
        return Err("保存目录不能是网络路径（UNC）".into());
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err("保存目录必须是绝对路径".into());
    }
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("保存路径不是一个文件夹：{raw}")),
        Err(e) => Err(format!("保存目录不可用：{raw}（{e}）")),
    }
}

pub fn clamp_concurrency(value: u32) -> u32 {
    value.clamp(MIN_CONCURRENCY, MAX_CONCURRENCY)
}

/// Sanitize IPC-supplied settings against the stored ones. A save_dir that
/// did not change passes through untouched — the on-disk default is an
/// empty string until the first directory pick, and re-saving an unchanged
/// value (any other field was toggled) must not brick settings saves. A new
/// value must be a usable local directory. Concurrency is always clamped.
pub fn sanitize_settings(old: &AppSettings, mut next: AppSettings) -> Result<AppSettings, String> {
    if next.save_dir != old.save_dir {
        validate_save_dir(&next.save_dir)?;
    }
    next.concurrency = clamp_concurrency(next.concurrency);
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A truncated write used to brick startup: `setup` propagates the parse
    /// error, so every launch died on the same file and the only way out was
    /// deleting it by hand.
    #[test]
    fn a_corrupt_settings_file_starts_from_defaults() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(settings_path(dir.path()), br#"{"concurrency": "#).unwrap();

        let loaded = load_settings(dir.path()).expect("startup must not fail");

        assert_eq!(loaded.concurrency, AppSettings::default().concurrency);
    }

    #[test]
    fn a_corrupt_settings_file_is_kept_for_diagnosis() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(settings_path(dir.path()), b"not json at all").unwrap();

        load_settings(dir.path()).unwrap();

        assert!(dir.path().join("settings.json.bad").is_file());
        assert!(!settings_path(dir.path()).exists());
    }

    #[test]
    fn saving_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        save_settings(dir.path(), &AppSettings::default()).unwrap();

        let leftovers: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != SETTINGS_FILE)
            .collect();

        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// The first cut only covered a file that parses badly. One that cannot be
    /// read at all still returned early through `?`, and `setup` propagates
    /// that, so the app exited on every launch — the same brick, one step
    /// earlier.
    #[test]
    fn an_unreadable_settings_path_starts_from_defaults() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file belongs: a readable path that is not a
        // readable file.
        fs::create_dir(settings_path(dir.path())).unwrap();

        let loaded = load_settings(dir.path()).expect("startup must not fail");

        assert_eq!(loaded.concurrency, AppSettings::default().concurrency);
    }

    /// A save that cannot land must not leave its scratch file behind: nothing
    /// cleans that directory, so it would sit there for the life of the
    /// install.
    #[test]
    fn a_failed_save_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where settings.json belongs, so the rename cannot land.
        fs::create_dir(settings_path(dir.path())).unwrap();

        assert!(save_settings(dir.path(), &AppSettings::default()).is_err());

        assert!(!dir.path().join("settings.json.tmp").exists());
    }

    /// The save swaps a finished temp file in rather than rewriting the real
    /// one, and that swap is the whole point: an interrupted `fs::write`
    /// truncates first, and the truncated file then fails every launch before
    /// the window opens. Atomicity itself needs fault injection to observe,
    /// so this pins the structure that provides it — a rename installs a new
    /// inode, an in-place write keeps the old one.
    #[cfg(unix)]
    #[test]
    fn saving_replaces_the_file_instead_of_rewriting_it() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        save_settings(dir.path(), &AppSettings::default()).unwrap();
        let first = fs::metadata(settings_path(dir.path())).unwrap().ino();

        save_settings(dir.path(), &AppSettings::default()).unwrap();
        let second = fs::metadata(settings_path(dir.path())).unwrap().ino();

        assert_ne!(first, second);
    }

    #[test]
    fn settings_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let s = AppSettings {
            save_dir: dir.path().join("videos").to_string_lossy().into(),
            concurrency: 2,
            ..AppSettings::default()
        };
        save_settings(dir.path(), &s).unwrap();
        let loaded = load_settings(dir.path()).unwrap();
        assert_eq!(loaded.concurrency, 2);
        assert_eq!(loaded.filename_template, "%(title)s [%(id)s].%(ext)s");
    }

    #[test]
    fn migrates_compact_datetime_to_hyphenated() {
        let dir = tempfile::tempdir().unwrap();
        let s = AppSettings {
            filename_template: "%(timestamp>%Y%m%dT%H-%M-%S)s_%(title)s [%(id)s].%(ext)s".into(),
            ..AppSettings::default()
        };
        save_settings(dir.path(), &s).unwrap();
        let loaded = load_settings(dir.path()).unwrap();
        assert_eq!(
            loaded.filename_template,
            "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s [%(id)s].%(ext)s"
        );
        let raw = fs::read_to_string(settings_path(dir.path())).unwrap();
        assert!(raw.contains("%Y-%m-%dT%H-%M-%S"));
    }

    #[test]
    fn migrates_minute_only_compact_to_hyphenated_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let s = AppSettings {
            filename_template: "%(timestamp>%Y%m%dT%H-%M)s_%(title)s [%(id)s].%(ext)s".into(),
            ..AppSettings::default()
        };
        save_settings(dir.path(), &s).unwrap();
        let loaded = load_settings(dir.path()).unwrap();
        assert_eq!(
            loaded.filename_template,
            "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s [%(id)s].%(ext)s"
        );
    }

    #[test]
    fn does_not_double_migrate_hyphenated_format() {
        let dir = tempfile::tempdir().unwrap();
        let s = AppSettings {
            filename_template: "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s.%(ext)s".into(),
            ..AppSettings::default()
        };
        save_settings(dir.path(), &s).unwrap();
        let loaded = load_settings(dir.path()).unwrap();
        assert_eq!(
            loaded.filename_template,
            "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s.%(ext)s"
        );
    }

    // --- B10: IPC input validation -----------------------------------------

    #[test]
    fn empty_save_dir_is_rejected() {
        assert!(validate_save_dir("").is_err());
        assert!(validate_save_dir("   ").is_err());
    }

    #[test]
    fn relative_save_dir_is_rejected() {
        assert!(validate_save_dir("Downloads/video").is_err());
    }

    #[test]
    fn unc_save_dir_is_rejected() {
        // Backslash form (Windows UNC), verbatim-UNC form, and the
        // forward-slash form Windows APIs also accept: none of these may
        // reach a file operation, which would turn the first download into
        // an SMB probe with the user's credentials.
        assert!(validate_save_dir(r"\\server\share").is_err());
        assert!(validate_save_dir(r"\\?\UNC\server\share").is_err());
        assert!(validate_save_dir("//server/share").is_err());
    }

    #[test]
    fn existing_directory_passes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_save_dir(dir.path().to_str().unwrap()).is_ok());
    }

    #[test]
    fn missing_save_dir_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        assert!(validate_save_dir(gone.to_str().unwrap()).is_err());
    }

    #[test]
    fn file_save_dir_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        fs::write(&file, b"x").unwrap();
        assert!(validate_save_dir(file.to_str().unwrap()).is_err());
    }

    #[test]
    fn concurrency_clamps_into_bounds() {
        assert_eq!(clamp_concurrency(0), 1);
        assert_eq!(clamp_concurrency(3), 3);
        assert_eq!(clamp_concurrency(16), 16);
        assert_eq!(clamp_concurrency(17), 16);
        assert_eq!(clamp_concurrency(u32::MAX), 16);
    }

    #[test]
    fn unchanged_empty_save_dir_passes_through() {
        // The on-disk default is an empty save_dir until the first pick;
        // re-saving other fields must not brick settings saves.
        let old = AppSettings::default();
        let mut next = old.clone();
        next.skip_existing = !old.skip_existing;
        let out = sanitize_settings(&old, next).unwrap();
        assert_eq!(out.save_dir, "");
        assert_eq!(out.skip_existing, !old.skip_existing);
    }

    #[test]
    fn changed_save_dir_is_validated() {
        let old = AppSettings::default();
        let mut next = old.clone();
        next.save_dir = "relative/path".into();
        assert!(sanitize_settings(&old, next).is_err());
    }

    #[test]
    fn concurrency_is_clamped_by_sanitize() {
        let old = AppSettings::default();
        let mut next = old.clone();
        next.concurrency = 999;
        assert_eq!(sanitize_settings(&old, next).unwrap().concurrency, 16);
    }
}
