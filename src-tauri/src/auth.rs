use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use keyring::Entry;

use crate::cookies::{self, Cookie};
use crate::error::{AppError, AppResult};
use crate::models::AuthStatus;

pub const KEYRING_SERVICE: &str = "app.videofetch.desktop";
pub const KEYRING_ACCOUNT: &str = "bilibili_cookies";
const COOKIES_FILENAME: &str = "cookies.txt";
const AUTH_JSON_FILENAME: &str = "auth_cookies.json";

pub trait KeyringStore: Send + Sync {
    fn get(&self) -> AppResult<Option<String>>;
    fn set(&self, value: &str) -> AppResult<()>;
    fn delete(&self) -> AppResult<()>;
}

pub struct SystemKeyringStore;

impl KeyringStore for SystemKeyringStore {
    fn get(&self) -> AppResult<Option<String>> {
        match Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, value: &str) -> AppResult<()> {
        Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)?.set_password(value)?;
        Ok(())
    }

    fn delete(&self) -> AppResult<()> {
        match Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Dual storage for auth cookies JSON.
///
/// - **release:** prefer OS keyring, always mirror to `auth_cookies.json`
///   (unsigned Mac apps may prompt or fail on keyring).
/// - **debug:** file only — never call keyring (avoids login-keychain prompts in `tauri dev`).
///
/// Treat `auth_cookies.json` as a secret; DualStore sets owner-only perms on Unix.
pub struct DualStore {
    keyring: SystemKeyringStore,
    file_path: PathBuf,
}

impl DualStore {
    pub fn new(cache_dir: &Path) -> Self {
        Self {
            keyring: SystemKeyringStore,
            file_path: cache_dir.join(AUTH_JSON_FILENAME),
        }
    }
}

impl KeyringStore for DualStore {
    fn get(&self) -> AppResult<Option<String>> {
        if !cfg!(debug_assertions)
            && let Ok(Some(value)) = self.keyring.get()
        {
            return Ok(Some(value));
        }
        if self.file_path.exists() {
            let text = fs::read_to_string(&self.file_path)?;
            if text.trim().is_empty() {
                return Ok(None);
            }
            return Ok(Some(text));
        }
        Ok(None)
    }

    fn set(&self, value: &str) -> AppResult<()> {
        if !cfg!(debug_assertions) {
            let _ = self.keyring.set(value);
        }
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.file_path, value)?;
        restrict_private_file_perms(&self.file_path);
        Ok(())
    }

    fn delete(&self) -> AppResult<()> {
        if !cfg!(debug_assertions) {
            let _ = self.keyring.delete();
        }
        if self.file_path.exists() {
            fs::remove_file(&self.file_path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Option<String>>,
}

#[cfg(test)]
impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
impl KeyringStore for MemoryStore {
    fn get(&self) -> AppResult<Option<String>> {
        Ok(self.inner.lock().unwrap().clone())
    }

    fn set(&self, value: &str) -> AppResult<()> {
        *self.inner.lock().unwrap() = Some(value.to_string());
        Ok(())
    }

    fn delete(&self) -> AppResult<()> {
        *self.inner.lock().unwrap() = None;
        Ok(())
    }
}

pub struct AuthManager<S: KeyringStore = DualStore> {
    store: S,
    cache_dir: PathBuf,
}

impl AuthManager<DualStore> {
    pub fn new(cache_dir: PathBuf) -> Self {
        let store = DualStore::new(&cache_dir);
        Self { store, cache_dir }
    }
}

impl<S: KeyringStore> AuthManager<S> {
    #[cfg(test)]
    pub fn with_store(store: S, cache_dir: PathBuf) -> Self {
        Self { store, cache_dir }
    }

    pub fn import_cookies_text(&self, text: &str) -> AppResult<AuthStatus> {
        let cookies = cookies::parse_netscape_cookies(text)?;
        self.save_cookies(&cookies)
    }

    pub fn import_cookies_file(&self, path: &Path) -> AppResult<AuthStatus> {
        let text = fs::read_to_string(path)?;
        self.import_cookies_text(&text)
    }

    pub fn auth_status(&self) -> AuthStatus {
        match self.load_cookies() {
            Ok(cookies) => evaluate_auth_status(cookies.as_deref()),
            Err(_) => AuthStatus::LoggedOut,
        }
    }

    pub fn clear_auth(&self) -> AppResult<()> {
        self.store.delete()?;
        let path = self.cookies_file_path();
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    pub fn materialize_cookies_file(&self) -> AppResult<Option<PathBuf>> {
        let cookies = self.load_cookies()?;
        self.materialize_cookies_file_from(cookies.as_deref())
    }

    /// Write `cookies` to the Netscape file (or remove a stale file) without a
    /// second keyring read; callers that already loaded cookies via [`Self::cookies`]
    /// should use this to avoid hitting the OS credential store twice.
    pub fn materialize_cookies_file_from(
        &self,
        cookies: Option<&[Cookie]>,
    ) -> AppResult<Option<PathBuf>> {
        match cookies {
            Some(cookies) if evaluate_auth_status(Some(cookies)) == AuthStatus::LoggedIn => {
                fs::create_dir_all(&self.cache_dir)?;
                let path = self.cookies_file_path();
                cookies::write_netscape_file(&path, cookies)?;
                restrict_private_file_perms(&path);
                Ok(Some(path))
            }
            _ => {
                // Keep resolve/download cookie usage aligned: non-LoggedIn must not
                // leave a stale Netscape file for yt-dlp to pick up.
                let path = self.cookies_file_path();
                if path.exists() {
                    fs::remove_file(&path)?;
                }
                Ok(None)
            }
        }
    }

    /// Read the stored login cookies (used for the playurl request header).
    pub fn cookies(&self) -> AppResult<Option<Vec<Cookie>>> {
        self.load_cookies()
    }

    pub fn save_cookies_from_webview(&self, cookies: Vec<Cookie>) -> AppResult<AuthStatus> {
        self.save_cookies(&cookies)
    }

    fn save_cookies(&self, cookies: &[Cookie]) -> AppResult<AuthStatus> {
        let cookies = cookies::normalize_bilibili_cookies(cookies);
        if !cookies::has_bilibili_sessdata(&cookies) {
            return Err(AppError::Message(
                "未找到有效的 B 站 SESSDATA，请确认已在登录窗口完成登录".into(),
            ));
        }

        let json = serde_json::to_string(&cookies).map_err(|e| AppError::Message(e.to_string()))?;
        self.store.set(&json)?;
        Ok(evaluate_auth_status(Some(&cookies)))
    }

    fn load_cookies(&self) -> AppResult<Option<Vec<Cookie>>> {
        let Some(json) = self.store.get()? else {
            return Ok(None);
        };
        let cookies: Vec<Cookie> =
            serde_json::from_str(&json).map_err(|e| AppError::Message(e.to_string()))?;
        Ok(Some(cookies::normalize_bilibili_cookies(&cookies)))
    }

    fn cookies_file_path(&self) -> PathBuf {
        self.cache_dir.join(COOKIES_FILENAME)
    }
}

pub fn evaluate_auth_status(cookies: Option<&[Cookie]>) -> AuthStatus {
    let Some(cookies) = cookies else {
        return AuthStatus::LoggedOut;
    };
    if cookies.is_empty() {
        return AuthStatus::LoggedOut;
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    match cookies
        .iter()
        .find(|c| c.name == "SESSDATA" && !c.value.is_empty())
    {
        None => AuthStatus::PossiblyExpired,
        Some(cookie) if cookie.expiration != 0 && cookie.expiration < now => {
            AuthStatus::PossiblyExpired
        }
        Some(_) => AuthStatus::LoggedIn,
    }
}

/// Best-effort owner-only perms for secret files (no-op on non-Unix).
fn restrict_private_file_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# Netscape HTTP Cookie File
.bilibili.com	TRUE	/	FALSE	0	SESSDATA	abc123
.bilibili.com	TRUE	/	FALSE	0	bili_jct	tok
"#;

    fn test_manager() -> AuthManager<MemoryStore> {
        let dir = tempfile::tempdir().unwrap();
        AuthManager::with_store(MemoryStore::new(), dir.keep())
    }

    #[test]
    fn import_cookies_text_with_sessdata() {
        let mgr = test_manager();
        let status = mgr.import_cookies_text(SAMPLE).unwrap();
        assert_eq!(status, AuthStatus::LoggedIn);
        assert_eq!(mgr.auth_status(), AuthStatus::LoggedIn);
    }

    #[test]
    fn import_cookies_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cookies.txt");
        fs::write(&path, SAMPLE).unwrap();
        let mgr = AuthManager::with_store(MemoryStore::new(), dir.path().to_path_buf());
        let status = mgr.import_cookies_file(&path).unwrap();
        assert_eq!(status, AuthStatus::LoggedIn);
    }

    #[test]
    fn import_rejects_missing_sessdata() {
        let mgr = test_manager();
        let err = mgr
            .import_cookies_text("# Netscape\n.example.com\tTRUE\t/\tFALSE\t0\tfoo\tbar\n")
            .unwrap_err();
        assert!(err.to_string().contains("SESSDATA"));
    }

    #[test]
    fn auth_status_logged_out_when_empty() {
        let mgr = test_manager();
        assert_eq!(mgr.auth_status(), AuthStatus::LoggedOut);
    }

    #[test]
    fn clear_auth_removes_stored_cookies() {
        let mgr = test_manager();
        mgr.import_cookies_text(SAMPLE).unwrap();
        mgr.clear_auth().unwrap();
        assert_eq!(mgr.auth_status(), AuthStatus::LoggedOut);
    }

    #[test]
    fn materialize_writes_netscape_file() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = AuthManager::with_store(MemoryStore::new(), dir.path().to_path_buf());
        mgr.import_cookies_text(SAMPLE).unwrap();
        let path = mgr.materialize_cookies_file().unwrap().unwrap();
        assert!(path.exists());
        let text = fs::read_to_string(path).unwrap();
        assert!(text.contains("SESSDATA"));
    }

    #[test]
    fn materialize_returns_none_when_logged_out() {
        let mgr = test_manager();
        assert!(mgr.materialize_cookies_file().unwrap().is_none());
    }

    #[test]
    fn materialize_removes_stale_netscape_when_not_logged_in() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = AuthManager::with_store(MemoryStore::new(), dir.path().to_path_buf());
        mgr.import_cookies_text(SAMPLE).unwrap();
        let path = mgr.materialize_cookies_file().unwrap().unwrap();
        assert!(path.exists());
        mgr.clear_auth().unwrap();
        // Simulate leftover file after status flip without clear (e.g. expired).
        fs::write(&path, "stale").unwrap();
        assert!(mgr.materialize_cookies_file().unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn materialize_removes_netscape_when_possibly_expired() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = AuthManager::with_store(MemoryStore::new(), dir.path().to_path_buf());
        mgr.import_cookies_text(SAMPLE).unwrap();
        let path = mgr.materialize_cookies_file().unwrap().unwrap();
        // Overwrite store with expired SESSDATA (expiration = 1).
        mgr.store
            .set(
                r#"[{"domain":".bilibili.com","include_subdomains":true,"path":"/","secure":true,"expiration":1,"name":"SESSDATA","value":"old"}]"#,
            )
            .unwrap();
        assert_eq!(mgr.auth_status(), AuthStatus::PossiblyExpired);
        assert!(mgr.materialize_cookies_file().unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn save_cookies_from_webview() {
        let mgr = test_manager();
        let status = mgr
            .save_cookies_from_webview(vec![Cookie {
                domain: ".bilibili.com".into(),
                include_subdomains: true,
                path: "/".into(),
                secure: true,
                expiration: 0,
                name: "SESSDATA".into(),
                value: "token".into(),
            }])
            .unwrap();
        assert_eq!(status, AuthStatus::LoggedIn);
    }

    #[test]
    fn possibly_expired_when_sessdata_missing() {
        let cookies = vec![Cookie {
            domain: ".bilibili.com".into(),
            include_subdomains: true,
            path: "/".into(),
            secure: false,
            expiration: 0,
            name: "bili_jct".into(),
            value: "x".into(),
        }];
        assert_eq!(
            evaluate_auth_status(Some(&cookies)),
            AuthStatus::PossiblyExpired
        );
    }

    #[test]
    fn possibly_expired_when_sessdata_past_expiration() {
        let cookies = vec![Cookie {
            domain: ".bilibili.com".into(),
            include_subdomains: true,
            path: "/".into(),
            secure: true,
            expiration: 1,
            name: "SESSDATA".into(),
            value: "old".into(),
        }];
        assert_eq!(
            evaluate_auth_status(Some(&cookies)),
            AuthStatus::PossiblyExpired
        );
    }

    #[test]
    fn dual_store_file_roundtrip_without_requiring_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let store = DualStore::new(dir.path());
        let payload = r#"[{"domain":".bilibili.com","include_subdomains":true,"path":"/","secure":true,"expiration":0,"name":"SESSDATA","value":"abc"}]"#;
        store.set(payload).unwrap();

        let path = dir.path().join("auth_cookies.json");
        assert!(
            path.is_file(),
            "debug/release DualStore must persist auth_cookies.json"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("SESSDATA"));

        let got = store.get().unwrap().unwrap();
        assert!(got.contains("SESSDATA"));

        store.delete().unwrap();
        assert!(!path.exists());
        assert!(store.get().unwrap().is_none());
    }
}
