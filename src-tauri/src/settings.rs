use std::fs;
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
    let mut settings: AppSettings =
        serde_json::from_str(&text).map_err(|e| AppError::Message(e.to_string()))?;
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
    fs::write(path, json)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
