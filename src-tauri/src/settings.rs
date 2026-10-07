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
    if !path.exists() {
        return Ok(AppSettings::default());
    }
    let text = fs::read_to_string(&path)?;
    let mut settings: AppSettings = match serde_json::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            // A truncated write used to brick the app: setup propagates this
            // error, so every launch died on the same file and the only way
            // out was deleting it by hand. Keep it for diagnosis, start from
            // defaults, and let the next save replace it.
            let backup = path.with_extension("json.bad");
            tracing::warn!(
                "settings.json 解析失败（{e}），已另存为 {}，本次使用默认设置",
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
    let mut file = fs::File::create(&tmp)?;
    file.write_all(json.as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp, &path)?;
    Ok(())
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
}
