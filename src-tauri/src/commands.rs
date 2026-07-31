use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::DialogExt;
use url::Url;

use crate::auth::AuthManager;
use crate::bilibili_view;
use crate::cookies::{self, Cookie};
use crate::download::{
    DownloadManager, DownloadProgressEvent, PROGRESS_EVENT, ProgressEmitter,
    cleanup_orphan_work_dirs,
};
use crate::error::{AppError, AppResult};
use crate::models::{
    AppSettings, AuthStatus, CancelAllResult, ClearFinishedResult, DownloadConflict, DownloadJob,
    JobStatus, VideoMeta,
};
use crate::naming;
use crate::platform;
use crate::resolve_cache;
use crate::settings as settings_store;
use crate::sidecar;
use crate::ytdlp::{self, YtDlpConfig};

const BILIBILI_LOGIN_LABEL: &str = "bilibili-login";
const BILIBILI_LOGIN_URL: &str = "https://passport.bilibili.com/login";

pub const RESOLVE_PARTIAL_EVENT: &str = "resolve://partial";
pub const RESOLVE_COMPLETE_EVENT: &str = "resolve://complete";
pub const RESOLVE_FORMATS_FAILED_EVENT: &str = "resolve://formats_failed";

pub struct AppState {
    pub app_dir: PathBuf,
    pub auth: AuthManager,
    pub downloads: DownloadManager,
    pub settings: std::sync::Mutex<AppSettings>,
    pub ytdlp: YtDlpConfig,
    /// Latest resolve request id; stale in-flight results must not emit or write cache.
    pub active_resolve_id: AtomicU64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveMetaEvent {
    pub request_id: u64,
    pub meta: VideoMeta,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveFormatsFailedEvent {
    pub request_id: u64,
    pub error: String,
}

pub fn is_resolve_current(active_id: u64, request_id: u64) -> bool {
    active_id == request_id
}

/// Register a resolve generation. Only increases `active` (never lets an older id win).
pub fn claim_resolve_id(active: &AtomicU64, request_id: Option<u64>) -> u64 {
    match request_id {
        Some(id) => {
            active.fetch_max(id, Ordering::SeqCst);
            id
        }
        None => active.fetch_add(1, Ordering::SeqCst).saturating_add(1),
    }
}

pub struct TauriProgressEmitter {
    app: AppHandle,
}

impl TauriProgressEmitter {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl ProgressEmitter for TauriProgressEmitter {
    fn emit_progress(&self, event: DownloadProgressEvent) {
        let _ = self.app.emit(PROGRESS_EVENT, &event);
    }
}

#[derive(Debug, Deserialize)]
pub struct EnqueueArgs {
    pub url: String,
    pub video_id: String,
    #[serde(default)]
    pub title: String,
    pub page_indexes: Vec<u32>,
    pub format_id: String,
    pub output_template: Option<String>,
    /// Save as a new numbered copy; never overwrite. Does not bypass active-queue lock.
    #[serde(default, alias = "force")]
    pub save_as_copy: bool,
    #[serde(default)]
    pub uploader: String,
}

#[derive(Debug, Deserialize)]
pub struct CheckConflictArgs {
    pub video_id: String,
    pub page_indexes: Vec<u32>,
    pub format_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub uploader: String,
}

#[tauri::command]
pub fn check_download_conflict(
    state: State<'_, AppState>,
    args: CheckConflictArgs,
) -> AppResult<DownloadConflict> {
    if args.page_indexes.is_empty() {
        return Ok(DownloadConflict {
            downloading: false,
            exists: false,
            file_exists: false,
        });
    }

    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))?
        .clone();
    let title = if args.title.is_empty() {
        args.video_id.clone()
    } else {
        args.title.clone()
    };
    let multi_page = args.page_indexes.len() > 1;
    let template = ensure_playlist_index_template(&settings.filename_template, multi_page);

    state.downloads.check_conflict(
        &args.video_id,
        &args.page_indexes,
        &args.format_id,
        &title,
        &args.uploader,
        &template,
    )
}

fn fallback_save_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("downloads")
}

/// Prefer the OS Downloads folder (locale-aware). Fall back to app data.
pub fn default_save_dir(app: &AppHandle, app_dir: &Path) -> PathBuf {
    app.path()
        .download_dir()
        .unwrap_or_else(|_| fallback_save_dir(app_dir))
}

fn rematerialize_cookies(state: &AppState) -> AppResult<Option<PathBuf>> {
    state.auth.materialize_cookies_file()
}

/// Build a Bilibili page URL using 1-based `?p=` (page 1 omits the query).
fn page_url(base: &str, page_index: u32) -> String {
    if page_index <= 1 {
        return strip_page_query(base);
    }
    if let Ok(mut u) = Url::parse(base) {
        let pairs: Vec<(String, String)> = u
            .query_pairs()
            .filter(|(k, _)| k != "p")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        u.query_pairs_mut().clear();
        for (k, v) in &pairs {
            u.query_pairs_mut().append_pair(k, v);
        }
        u.query_pairs_mut()
            .append_pair("p", &page_index.to_string());
        return u.to_string();
    }
    format!("{base}?p={page_index}")
}

fn strip_page_query(base: &str) -> String {
    let Ok(mut u) = Url::parse(base) else {
        return base.to_string();
    };
    if u.query_pairs().all(|(k, _)| k != "p") {
        return base.to_string();
    }
    let pairs: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| k != "p")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.is_empty() {
        u.set_query(None);
    } else {
        u.query_pairs_mut().clear();
        for (k, v) in &pairs {
            u.query_pairs_mut().append_pair(k, v);
        }
    }
    u.to_string()
}

/// Avoid multi-P filename collisions when the template has no playlist index token.
pub fn ensure_playlist_index_template(template: &str, multi_page: bool) -> String {
    if !multi_page || template.contains("%(playlist_index)") {
        return template.to_string();
    }
    if let Some(pos) = template.rfind(".%(ext)s") {
        let mut out = String::with_capacity(template.len() + 24);
        out.push_str(&template[..pos]);
        out.push_str(" [P%(playlist_index)s]");
        out.push_str(&template[pos..]);
        return out;
    }
    format!("{template} [P%(playlist_index)s]")
}

fn tauri_cookie_to_app(c: &tauri::webview::Cookie<'_>) -> Cookie {
    let expiration = c
        .expires_datetime()
        .map(|dt| dt.unix_timestamp().max(0) as u64)
        .unwrap_or(0);
    let domain = c.domain().unwrap_or(".bilibili.com").to_string();
    Cookie {
        domain: domain.clone(),
        include_subdomains: domain.starts_with('.'),
        path: c.path().unwrap_or("/").to_string(),
        secure: c.secure().unwrap_or(true),
        expiration,
        name: c.name().to_string(),
        value: c.value().to_string(),
    }
}

#[tauri::command]
pub async fn resolve_url(
    app: AppHandle,
    state: State<'_, AppState>,
    url: String,
    force: Option<bool>,
    request_id: Option<u64>,
) -> AppResult<VideoMeta> {
    let force = force.unwrap_or(false);
    let url = platform::canonicalize_video_url(&url);
    if platform::detect_platform(&url).is_none() {
        return Err(AppError::Message(
            "暂不支持该链接，最小可行产品仅支持 B 站".into(),
        ));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let cookies = rematerialize_cookies(&state)?;
    let scope = resolve_cache::cache_scope(cookies.is_some());

    if !force {
        for key in resolve_cache::lookup_cache_keys(&url, scope) {
            if let Some((mut meta, fetched_at)) = state.downloads.get_resolve_cache(&key)?
                && resolve_cache::is_fresh(fetched_at, now, resolve_cache::RESOLVE_CACHE_TTL_SECS)
            {
                // Old cache entries may still have resolution-based requires_login;
                // listed formats are downloadable under the resolve's cookie state.
                for format in &mut meta.formats {
                    format.requires_login = false;
                }
                meta.formats = ytdlp::finalize_formats_for_pages(
                    std::mem::take(&mut meta.formats),
                    meta.pages.len(),
                );
                return Ok(meta);
            }
        }
    }

    let request_id = claim_resolve_id(&state.active_resolve_id, request_id);

    let is_current =
        || is_resolve_current(state.active_resolve_id.load(Ordering::SeqCst), request_id);

    let view_url = url.clone();
    let mut view_task = tokio::spawn(async move { bilibili_view::resolve_view(&view_url).await });

    let ytdlp_cfg = state.ytdlp.clone();
    let ytdlp_url = url.clone();
    let cookies_path = cookies.clone();
    let mut ytdlp_task = tokio::spawn(async move {
        ytdlp::resolve_meta(&ytdlp_cfg, &ytdlp_url, cookies_path.as_deref()).await
    });

    let mut view_meta: Option<VideoMeta> = None;
    let mut emitted_partial = false;

    // Prefer whichever finishes first: emit partial early if view wins; never block
    // complete on a slow/hung view once yt-dlp is done (view client also has a timeout).
    let ytdlp_result = tokio::select! {
        view_res = &mut view_task => {
            if let Ok(Ok(partial)) = view_res {
                if is_current() {
                    let _ = app.emit(
                        RESOLVE_PARTIAL_EVENT,
                        &ResolveMetaEvent {
                            request_id,
                            meta: partial.clone(),
                        },
                    );
                    emitted_partial = true;
                    view_meta = Some(partial);
                }
            }
            match ytdlp_task.await {
                Ok(inner) => inner,
                Err(e) => Err(AppError::Message(format!("yt-dlp 任务失败: {e}"))),
            }
        }
        ytdlp_res = &mut ytdlp_task => {
            let ytdlp_result = match ytdlp_res {
                Ok(inner) => inner,
                Err(e) => Err(AppError::Message(format!("yt-dlp 任务失败: {e}"))),
            };
            // Short grace so a nearly-done view can still merge; then abort.
            match tokio::time::timeout(Duration::from_millis(300), &mut view_task).await {
                Ok(Ok(Ok(partial))) => {
                    if is_current() {
                        let _ = app.emit(
                            RESOLVE_PARTIAL_EVENT,
                            &ResolveMetaEvent {
                                request_id,
                                meta: partial.clone(),
                            },
                        );
                        emitted_partial = true;
                        view_meta = Some(partial);
                    }
                }
                _ => {
                    view_task.abort();
                }
            }
            ytdlp_result
        }
    };

    if !is_current() {
        return Err(AppError::Message("解析已取消（有更新的请求）".into()));
    }

    match ytdlp_result {
        Ok(ytdlp_meta) => {
            let meta = match view_meta {
                Some(view) => bilibili_view::merge_view_with_formats(view, ytdlp_meta),
                None => ytdlp_meta,
            };
            for key in resolve_cache::store_cache_keys(&url, &meta.id, scope) {
                state.downloads.upsert_resolve_cache(&key, &meta, now)?;
            }
            let _ = app.emit(
                RESOLVE_COMPLETE_EVENT,
                &ResolveMetaEvent {
                    request_id,
                    meta: meta.clone(),
                },
            );
            Ok(meta)
        }
        Err(err) => {
            if emitted_partial {
                let partial = view_meta.expect("partial was emitted");
                let _ = app.emit(
                    RESOLVE_FORMATS_FAILED_EVENT,
                    &ResolveFormatsFailedEvent {
                        request_id,
                        error: err.to_string(),
                    },
                );
                // Keep the card; UI uses formats_failed for the formats area.
                Ok(partial)
            } else {
                Err(err)
            }
        }
    }
}

#[tauri::command]
pub fn enqueue_download(state: State<'_, AppState>, args: EnqueueArgs) -> AppResult<DownloadJob> {
    if args.page_indexes.is_empty() {
        return Err(AppError::Message("请至少选择一个分 P".into()));
    }

    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))?
        .clone();
    let base_template = args
        .output_template
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&settings.filename_template);
    naming::validate_output_template(base_template).map_err(AppError::Message)?;
    let multi_page = args.page_indexes.len() > 1;
    let template = ensure_playlist_index_template(base_template, multi_page);
    let title = if args.title.is_empty() {
        args.video_id.clone()
    } else {
        args.title.clone()
    };

    let _ = rematerialize_cookies(&state)?;

    let save_as_copy = args.save_as_copy;
    let uploader = args.uploader;
    let save_dir = Path::new(&settings.save_dir);
    let base_url = platform::canonicalize_video_url(&args.url);

    let mut last = None;
    for page_index in args.page_indexes {
        let page_template = if save_as_copy {
            naming::next_available_output_template(
                save_dir,
                &template,
                &title,
                &args.video_id,
                &uploader,
                page_index,
            )
        } else {
            template.clone()
        };

        let job = DownloadJob {
            id: String::new(),
            url: page_url(&base_url, page_index),
            video_id: args.video_id.clone(),
            page_index,
            format_id: args.format_id.clone(),
            title: title.clone(),
            output_template: page_template,
            status: JobStatus::Pending,
            progress: 0.0,
            error: None,
            output_path: None,
        };
        last = Some(state.downloads.enqueue(job, save_as_copy)?);
    }
    last.ok_or_else(|| AppError::Message("未能创建下载任务".into()))
}

#[tauri::command]
pub fn list_jobs(state: State<'_, AppState>) -> AppResult<Vec<DownloadJob>> {
    state.downloads.list()
}

#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    state.downloads.cancel(&id)
}

#[tauri::command]
pub fn cancel_all_jobs(state: State<'_, AppState>) -> AppResult<CancelAllResult> {
    state.downloads.cancel_all()
}

#[tauri::command]
pub fn clear_finished_jobs(state: State<'_, AppState>) -> AppResult<ClearFinishedResult> {
    state.downloads.clear_finished()
}

#[tauri::command]
pub fn retry_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    state.downloads.retry(&id)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteJobArgs {
    pub id: String,
    /// When true, also remove the downloaded file on disk (if any).
    #[serde(default)]
    pub delete_file: bool,
}

#[tauri::command]
pub fn delete_job(state: State<'_, AppState>, args: DeleteJobArgs) -> AppResult<()> {
    state.downloads.delete(&args.id, args.delete_file)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))?
        .clone();
    Ok(settings)
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: AppSettings) -> AppResult<()> {
    naming::validate_output_template(&settings.filename_template).map_err(AppError::Message)?;
    settings_store::save_settings(&state.app_dir, &settings)?;
    *state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))? = settings.clone();
    state.downloads.update_settings(settings)?;
    Ok(())
}

#[tauri::command]
pub fn get_auth_status(state: State<'_, AppState>) -> AuthStatus {
    state.auth.auth_status()
}

#[tauri::command]
pub fn import_cookies_path(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> AppResult<AuthStatus> {
    let status = state.auth.import_cookies_file(Path::new(&path))?;
    let _ = rematerialize_cookies(&state)?;
    let _ = app.emit("auth://status", status.clone());
    Ok(status)
}

#[tauri::command]
pub fn clear_auth(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    state.auth.clear_auth()?;
    let status = state.auth.auth_status();
    let _ = app.emit("auth://status", status);
    Ok(())
}

fn collect_bilibili_cookies(win: &tauri::WebviewWindow) -> Vec<Cookie> {
    let mut collected = Vec::new();

    if let Ok(cookies) = win.cookies() {
        collected.extend(cookies.iter().map(tauri_cookie_to_app));
    }

    for raw in [
        "https://www.bilibili.com/",
        "https://bilibili.com/",
        "https://m.bilibili.com/",
        "https://passport.bilibili.com/",
        "https://api.bilibili.com/",
        "https://member.bilibili.com/",
    ] {
        if let Ok(url) = Url::parse(raw)
            && let Ok(cookies) = win.cookies_for_url(url)
        {
            collected.extend(cookies.iter().map(tauri_cookie_to_app));
        }
    }

    // Prefer the last SESSDATA seen (usually the freshest after login redirect).
    let mut by_key: std::collections::HashMap<(String, String), Cookie> =
        std::collections::HashMap::new();
    for cookie in collected {
        by_key.insert((cookie.domain.clone(), cookie.name.clone()), cookie);
    }
    cookies::normalize_bilibili_cookies(&by_key.into_values().collect::<Vec<_>>())
}

fn try_persist_webview_cookies(app: &AppHandle, win: &tauri::WebviewWindow) -> Option<AuthStatus> {
    let converted = collect_bilibili_cookies(win);
    if !cookies::has_bilibili_sessdata(&converted) {
        return None;
    }
    let state = app.try_state::<AppState>()?;
    match state.auth.save_cookies_from_webview(converted) {
        Ok(status) => {
            let _ = state.auth.materialize_cookies_file();
            Some(status)
        }
        Err(_) => None,
    }
}

/// Open an embedded Bilibili login WebView, poll for SESSDATA, and return auth status.
///
/// Waits until cookies are captured, the login window is closed, or a timeout elapses.
/// Cookie scraping via WebView APIs is best-effort on some platforms; if status stays
/// logged out, use Settings → Advanced → import Netscape `cookies.txt`.
#[tauri::command]
pub async fn start_bilibili_login(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<AuthStatus> {
    if let Some(existing) = app.get_webview_window(BILIBILI_LOGIN_LABEL) {
        let _ = existing.set_focus();
        if let Some(status) = try_persist_webview_cookies(&app, &existing) {
            let _ = app.emit("auth://status", status.clone());
            return Ok(status);
        }
    } else {
        let login_url = Url::parse(BILIBILI_LOGIN_URL)
            .map_err(|e| AppError::Message(format!("invalid login url: {e}")))?;

        WebviewWindowBuilder::new(
            &app,
            BILIBILI_LOGIN_LABEL,
            WebviewUrl::External(login_url.clone()),
        )
        .title("登录 B 站 — 影取")
        .inner_size(980.0, 720.0)
        .build()
        .map_err(|e| AppError::Message(format!("无法打开登录窗口: {e}")))?;
    }

    let app_handle = app.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<AuthStatus>();
    let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));
    let tx_poll = std::sync::Arc::clone(&tx);

    tauri::async_runtime::spawn(async move {
        for _ in 0..150 {
            tokio::time::sleep(Duration::from_millis(1200)).await;

            let Some(win) = app_handle.get_webview_window(BILIBILI_LOGIN_LABEL) else {
                // Window closed before we captured cookies.
                if let Some(tx) = tx_poll.lock().ok().and_then(|mut g| g.take()) {
                    let status = app_handle
                        .try_state::<AppState>()
                        .map(|s| s.auth.auth_status())
                        .unwrap_or(AuthStatus::LoggedOut);
                    let _ = tx.send(status);
                }
                return;
            };

            if let Some(status) = try_persist_webview_cookies(&app_handle, &win) {
                let _ = win.close();
                let _ = app_handle.emit("auth://status", status.clone());
                if let Some(tx) = tx_poll.lock().ok().and_then(|mut g| g.take()) {
                    let _ = tx.send(status);
                }
                return;
            }
        }

        if let Some(tx) = tx_poll.lock().ok().and_then(|mut g| g.take()) {
            let status = app_handle
                .try_state::<AppState>()
                .map(|s| s.auth.auth_status())
                .unwrap_or(AuthStatus::LoggedOut);
            let _ = tx.send(status);
        }
    });

    match rx.await {
        Ok(status) => Ok(status),
        Err(_) => Ok(state.auth.auth_status()),
    }
}

#[tauri::command]
pub fn preview_name(
    template: String,
    title: String,
    id: String,
    uploader: String,
    ext: String,
    index: u32,
) -> String {
    naming::preview_filename(&template, &title, &id, &uploader, &ext, index)
}

#[tauri::command]
pub fn open_path(path: String) -> AppResult<()> {
    tauri_plugin_opener::open_path(&path, None::<&str>)
        .map_err(|e| AppError::Message(format!("无法打开路径: {e}")))?;
    Ok(())
}

#[tauri::command]
pub async fn pick_save_dir(app: AppHandle) -> AppResult<String> {
    let folder =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| AppError::Message(format!("对话框任务失败: {e}")))?;

    match folder {
        Some(path) => Ok(path.to_string()),
        None => Err(AppError::Message("已取消选择目录".into())),
    }
}

#[tauri::command]
pub async fn pick_cookies_file(app: AppHandle) -> AppResult<String> {
    let file = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Cookies", &["txt"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| AppError::Message(format!("对话框任务失败: {e}")))?;

    match file {
        Some(path) => Ok(path.to_string()),
        None => Err(AppError::Message("已取消选择文件".into())),
    }
}

pub fn build_app_state(app: &AppHandle) -> AppResult<AppState> {
    let app_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Message(format!("app data dir: {e}")))?;
    std::fs::create_dir_all(&app_dir)?;

    let cache_dir = app_dir.join("cache");
    std::fs::create_dir_all(&cache_dir)?;
    let cookies_file = cache_dir.join("cookies.txt");

    let mut settings = settings_store::load_settings(&app_dir)?;
    if settings.save_dir.is_empty() {
        settings.save_dir = default_save_dir(app, &app_dir).to_string_lossy().into();
        let _ = settings_store::save_settings(&app_dir, &settings);
    }
    if std::fs::create_dir_all(&settings.save_dir).is_err() {
        let fb = fallback_save_dir(&app_dir);
        std::fs::create_dir_all(&fb)?;
        settings.save_dir = fb.to_string_lossy().into();
        let _ = settings_store::save_settings(&app_dir, &settings);
    }

    let db = crate::db::Db::open(&app_dir.join("jobs.db"))?;
    let auth = AuthManager::new(cache_dir);
    let _ = auth.materialize_cookies_file();

    let work_root = app_dir.join("download-work");
    std::fs::create_dir_all(&work_root)?;

    let ytdlp = sidecar::resolve_ytdlp_config(app);
    let progress: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter::new(app.clone()));
    let downloads = DownloadManager::with_ytdlp(
        db,
        settings.clone(),
        ytdlp.clone(),
        Some(cookies_file.clone()),
        progress,
        work_root.clone(),
    )?;

    let active_ids: Vec<String> = downloads
        .list()?
        .into_iter()
        .filter(|j| {
            matches!(
                j.status,
                JobStatus::Pending | JobStatus::Running | JobStatus::Failed
            )
        })
        .map(|j| j.id)
        .collect();
    cleanup_orphan_work_dirs(&work_root, &active_ids);

    Ok(AppState {
        app_dir,
        auth,
        downloads,
        settings: std::sync::Mutex::new(settings),
        ytdlp,
        active_resolve_id: AtomicU64::new(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_request_id_is_not_current() {
        assert!(is_resolve_current(2, 2));
        assert!(!is_resolve_current(2, 1));
    }

    #[test]
    fn claim_resolve_id_is_monotonic() {
        let active = AtomicU64::new(0);
        assert_eq!(claim_resolve_id(&active, Some(2)), 2);
        assert_eq!(active.load(Ordering::SeqCst), 2);
        assert_eq!(claim_resolve_id(&active, Some(1)), 1);
        // Older id must not lower the active generation.
        assert_eq!(active.load(Ordering::SeqCst), 2);
        assert!(!is_resolve_current(active.load(Ordering::SeqCst), 1));
        assert!(is_resolve_current(active.load(Ordering::SeqCst), 2));
    }

    #[test]
    fn superseded_request_should_not_commit() {
        let active = AtomicU64::new(0);
        let first = claim_resolve_id(&active, Some(1));
        let second = claim_resolve_id(&active, Some(2));
        assert!(is_resolve_current(active.load(Ordering::SeqCst), second));
        assert!(!is_resolve_current(active.load(Ordering::SeqCst), first));
    }

    #[test]
    fn page_url_uses_one_based_bilibili_p() {
        let base = "https://www.bilibili.com/video/BV1xx";
        assert_eq!(page_url(base, 1), base);
        assert_eq!(
            page_url(base, 2),
            "https://www.bilibili.com/video/BV1xx?p=2"
        );
        assert_eq!(
            page_url("https://www.bilibili.com/video/BV1xx?p=9", 3),
            "https://www.bilibili.com/video/BV1xx?p=3"
        );
        assert_eq!(
            page_url("https://www.bilibili.com/video/BV1xx?p=2", 1),
            "https://www.bilibili.com/video/BV1xx"
        );
    }

    #[test]
    fn fallback_save_dir_joins_downloads_under_app_dir() {
        let app_dir = PathBuf::from("/tmp/videofetch-app-data");
        assert_eq!(fallback_save_dir(&app_dir), app_dir.join("downloads"));
    }

    #[test]
    fn ensure_playlist_index_appends_for_multi_page() {
        let t = "%(title)s [%(id)s].%(ext)s";
        assert_eq!(ensure_playlist_index_template(t, false), t);
        assert_eq!(
            ensure_playlist_index_template(t, true),
            "%(title)s [%(id)s] [P%(playlist_index)s].%(ext)s"
        );
        let already = "%(title)s [P%(playlist_index)s].%(ext)s";
        assert_eq!(ensure_playlist_index_template(already, true), already);
    }
}
