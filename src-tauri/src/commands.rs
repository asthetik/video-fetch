use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::models;
use crate::models::{
    AppSettings, AuthStatus, CancelAllResult, ClearFinishedResult, DownloadConflict, DownloadJob,
    JobStatus, VideoMeta,
};
use crate::naming;
use crate::platform;
use crate::playurl;
use crate::resolve_cache;
use crate::settings as settings_store;
use crate::sidecar;
use crate::wbi;
use crate::ytdlp::{self, YtDlpConfig};

const BILIBILI_LOGIN_LABEL: &str = "bilibili-login";
const BILIBILI_LOGIN_URL: &str = "https://passport.bilibili.com/login";

pub const RESOLVE_PARTIAL_EVENT: &str = "resolve://partial";
pub const RESOLVE_COMPLETE_EVENT: &str = "resolve://complete";
pub const RESOLVE_FORMATS_FAILED_EVENT: &str = "resolve://formats_failed";
pub const RESOLVE_FORMATS_PROGRESS_EVENT: &str = "resolve://formats_progress";

pub struct AppState {
    pub app_dir: PathBuf,
    pub auth: AuthManager,
    pub downloads: DownloadManager,
    pub settings: std::sync::Mutex<AppSettings>,
    pub ytdlp: YtDlpConfig,
    pub wbi_keys: wbi::WbiKeyCache,
    /// Latest resolve request id; stale in-flight results must not emit or write cache.
    pub active_resolve_id: AtomicU64,
    /// Local activity log writer (files under app_dir/logs).
    pub activity_log: crate::activity_log::ActivityLog,
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

/// Cap on concurrent playurl requests during multi-P sampling so bursts of API
/// calls don't trip Bilibili risk control.
const PLAYURL_SAMPLE_CONCURRENCY: usize = 4;

/// Multi-page sampling: query all pages when <=16 parts, otherwise 8 sampled
/// pages, at most `PLAYURL_SAMPLE_CONCURRENCY` at a time. Each completed page
/// invokes `on_progress` with the cumulative result in completion order (the
/// frontend replaces the whole list, so it stays idempotent).
async fn multi_page_playurl_formats(
    client: &reqwest::Client,
    keys: &wbi::WbiKeys,
    bvid: &str,
    pages: &[models::PageItem],
    cookie_header: Option<&str>,
    on_progress: impl Fn(Vec<models::FormatOption>),
) -> Option<Vec<models::FormatOption>> {
    let max_samples = if pages.len() <= 16 { pages.len() } else { 8 };
    let indices = ytdlp::sample_page_indices(pages.len() as u32, max_samples);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(PLAYURL_SAMPLE_CONCURRENCY));
    let mut tasks = tokio::task::JoinSet::new();
    for index in indices {
        let Some(page) = pages.get(index.saturating_sub(1) as usize) else {
            continue;
        };
        let cid = page.page_id.clone();
        let bvid = bvid.to_string();
        let cookie_header = cookie_header.map(str::to_string);
        let client = client.clone();
        let keys = keys.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("playurl sample semaphore closed");
            playurl::fetch_formats(&client, &keys, &bvid, &cid, cookie_header.as_deref(), true)
                .await
                .ok()
        });
    }
    let mut observed = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let Ok(Some(formats)) = task else {
            continue;
        };
        playurl::merge_multi_page_options(&mut observed, formats);
        on_progress(observed.clone());
    }
    (!observed.is_empty()).then_some(observed)
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

    // Load cookies once: the same snapshot feeds the yt-dlp fallback file and the
    // playurl/nav Cookie header (avoids two keyring reads per resolve).
    let stored_cookies = state.auth.cookies()?;
    let cookies = state
        .auth
        .materialize_cookies_file_from(stored_cookies.as_deref())?;
    let scope = resolve_cache::cache_scope(cookies.is_some());

    if !force {
        for key in resolve_cache::lookup_cache_keys(&url, scope) {
            if let Some((mut meta, fetched_at)) = state.downloads.get_resolve_cache(&key)?
                && resolve_cache::is_fresh(fetched_at, now, resolve_cache::RESOLVE_CACHE_TTL_SECS)
            {
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

    let log_bvid = resolve_cache::extract_bilibili_id(&url);
    tracing::info!(target: "core", "resolve: 开始 {}", log_bvid.as_deref().unwrap_or("未知"));

    let client = reqwest::Client::builder()
        .user_agent(bilibili_view::USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| AppError::Message(format!("创建 HTTP 客户端失败: {e}")))?;
    let cookie_header = stored_cookies
        .as_deref()
        .map(cookies::cookie_header_for_bilibili)
        .filter(|s| !s.is_empty());

    // Fast path: single-part videos resolve via view -> playurl; failures fall back to yt-dlp.
    let view_meta = match bilibili_view::resolve_view(&url).await {
        Ok(partial) if is_current() => {
            let _ = app.emit(
                RESOLVE_PARTIAL_EVENT,
                &ResolveMetaEvent {
                    request_id,
                    meta: partial.clone(),
                },
            );
            Some(partial)
        }
        _ => None,
    };

    let playurl_formats: Option<Vec<models::FormatOption>> = if let Some(view) = &view_meta {
        let bvid = resolve_cache::extract_bilibili_id(&url);
        let keys = match state
            .wbi_keys
            .get_or_fetch(&client, cookie_header.as_deref())
            .await
        {
            Ok(keys) => Some(keys),
            Err(e) => {
                tracing::warn!(target: "core", "resolve: wbi keys 失败，回退 yt-dlp: {e}");
                None
            }
        };
        match (bvid, keys) {
            (Some(bvid), Some(keys)) if view.pages.len() == 1 => {
                let cid = view.pages[0].page_id.clone();
                match playurl::fetch_formats(
                    &client,
                    &keys,
                    &bvid,
                    &cid,
                    cookie_header.as_deref(),
                    false,
                )
                .await
                {
                    Ok(formats) => Some(formats),
                    Err(e) => {
                        tracing::warn!(target: "core", "resolve: playurl 失败，回退 yt-dlp: {e}");
                        state.wbi_keys.invalidate();
                        None
                    }
                }
            }
            (Some(bvid), Some(keys)) => {
                let app_handle = app.clone();
                let progress_emit = move |formats: Vec<models::FormatOption>| {
                    let _ = app_handle.emit(
                        RESOLVE_FORMATS_PROGRESS_EVENT,
                        &ResolveMetaEvent {
                            request_id,
                            meta: models::VideoMeta {
                                formats,
                                ..view.clone()
                            },
                        },
                    );
                };
                let result = multi_page_playurl_formats(
                    &client,
                    &keys,
                    &bvid,
                    &view.pages,
                    cookie_header.as_deref(),
                    progress_emit,
                )
                .await;
                if result.is_none() {
                    state.wbi_keys.invalidate();
                }
                result
            }
            _ => None,
        }
    } else {
        None
    };

    let used_playurl = playurl_formats.is_some();
    let final_meta = match (view_meta, playurl_formats) {
        (Some(mut view), Some(formats)) => {
            view.formats = formats;
            view
        }
        (view_meta, _) => {
            // Fallback: the existing full yt-dlp path.
            let ytdlp_cfg = state.ytdlp.clone();
            let ytdlp_url = url.clone();
            let cookies_path = cookies.clone();
            match ytdlp::resolve_meta(&ytdlp_cfg, &ytdlp_url, cookies_path.as_deref()).await {
                Ok(ytdlp_meta) => match view_meta {
                    Some(view) => bilibili_view::merge_view_with_formats(view, ytdlp_meta),
                    None => ytdlp_meta,
                },
                Err(e) => {
                    if let Some(partial) = view_meta {
                        // Keep the rendered card and tell the UI formats finished loading.
                        let _ = app.emit(
                            RESOLVE_FORMATS_FAILED_EVENT,
                            &ResolveFormatsFailedEvent {
                                request_id,
                                error: e.to_string(),
                            },
                        );
                        return Ok(partial);
                    }
                    tracing::error!(
                        target: "core",
                        "resolve: 失败 {}",
                        crate::activity_log::redact_urls(&e.to_string())
                    );
                    return Err(e);
                }
            }
        }
    };

    if !is_current() {
        return Err(AppError::Message("解析已取消（有更新的请求）".into()));
    }
    let source = if used_playurl { "playurl" } else { "yt-dlp" };
    tracing::info!(
        target: "core",
        "resolve: 成功 {}（{} P，{} 个清晰度，{source}）",
        final_meta.id,
        final_meta.pages.len(),
        final_meta.formats.len()
    );
    for key in resolve_cache::store_cache_keys(&url, &final_meta.id, scope) {
        state
            .downloads
            .upsert_resolve_cache(&key, &final_meta, now)?;
    }
    let _ = app.emit(
        RESOLVE_COMPLETE_EVENT,
        &ResolveMetaEvent {
            request_id,
            meta: final_meta.clone(),
        },
    );
    Ok(final_meta)
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
    let page_count = args.page_indexes.len();

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
    tracing::info!(
        target: "core",
        "download: 入队 {title}（{}，格式 {}，{} P）",
        args.video_id,
        args.format_id,
        page_count
    );
    last.ok_or_else(|| AppError::Message("未能创建下载任务".into()))
}

#[tauri::command]
pub fn list_jobs(state: State<'_, AppState>) -> AppResult<Vec<DownloadJob>> {
    state.downloads.list()
}

#[tauri::command]
pub fn cancel_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 取消 {id}");
    state.downloads.cancel(&id)
}

#[tauri::command]
pub fn cancel_all_jobs(state: State<'_, AppState>) -> AppResult<CancelAllResult> {
    let result = state.downloads.cancel_all()?;
    tracing::info!(target: "core", "download: 取消全部（{} 个）", result.cancelled);
    Ok(result)
}

#[tauri::command]
pub fn clear_finished_jobs(state: State<'_, AppState>) -> AppResult<ClearFinishedResult> {
    let result = state.downloads.clear_finished()?;
    tracing::info!(target: "core", "history: 清空已完成（{} 条）", result.cleared);
    Ok(result)
}

#[tauri::command]
pub fn retry_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 重试 {id}");
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
    tracing::info!(target: "core", "download: 删除 {}（含文件={}）", args.id, args.delete_file);
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
    let old = state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))?
        .clone();
    let mut changed = Vec::new();
    if old.save_dir != settings.save_dir {
        changed.push("save_dir");
    }
    if old.concurrency != settings.concurrency {
        changed.push("concurrency");
    }
    if old.filename_template != settings.filename_template {
        changed.push("filename_template");
    }
    if old.skip_existing != settings.skip_existing {
        changed.push("skip_existing");
    }
    settings_store::save_settings(&state.app_dir, &settings)?;
    *state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))? = settings.clone();
    state.downloads.update_settings(settings)?;
    tracing::info!(
        target: "core",
        "settings: 保存 {}",
        if changed.is_empty() { "（无变化）".to_string() } else { changed.join("、") }
    );
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
    tracing::info!(target: "core", "auth: 导入 Cookie（文件）");
    Ok(status)
}

#[tauri::command]
pub fn clear_auth(app: AppHandle, state: State<'_, AppState>) -> AppResult<()> {
    tracing::info!(target: "core", "auth: 退出登录");
    state.auth.clear_auth()?;
    // Also drop the login WebView session: an open window would otherwise let the
    // cookie poll re-capture its lingering SESSDATA and flip back to logged in.
    if let Some(win) = app.get_webview_window(BILIBILI_LOGIN_LABEL) {
        let _ = win.clear_all_browsing_data();
        let _ = win.close();
    }
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

/// Clear the shared WebView browsing data and wait until no Bilibili SESSDATA
/// remains, so a previous session cannot auto-login. `clear_all_browsing_data`
/// is async on every platform with no completion signal, so poll the cookie
/// store and retry the clear until it actually takes effect. Best-effort: if it
/// never completes, navigate anyway after a bounded number of attempts.
async fn clear_webview_session(win: &tauri::WebviewWindow) {
    const MAX_ATTEMPTS: usize = 6;
    for attempt in 0..MAX_ATTEMPTS {
        if let Err(e) = win.clear_all_browsing_data() {
            tracing::warn!(target: "core", "auth: clear browsing data failed: {e}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !cookies::has_bilibili_sessdata(&collect_bilibili_cookies(win)) {
            return;
        }
        tracing::warn!(
            target: "core",
            "auth: SESSDATA still present after clear (attempt {})",
            attempt + 1
        );
    }
    tracing::warn!(
        target: "core",
        "auth: gave up waiting for browsing data clear; navigating anyway"
    );
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
    tracing::info!(target: "core", "auth: 登录开始");
    if let Some(existing) = app.get_webview_window(BILIBILI_LOGIN_LABEL) {
        let _ = existing.set_focus();
        if let Some(status) = try_persist_webview_cookies(&app, &existing) {
            let _ = existing.close();
            let _ = app.emit("auth://status", status.clone());
            tracing::info!(target: "core", "auth: 登录成功");
            return Ok(status);
        }
    } else {
        let login_url = Url::parse(BILIBILI_LOGIN_URL)
            .map_err(|e| AppError::Message(format!("invalid login url: {e}")))?;
        let blank = Url::parse("about:blank")
            .map_err(|e| AppError::Message(format!("invalid blank url: {e}")))?;

        let win = WebviewWindowBuilder::new(
            &app,
            BILIBILI_LOGIN_LABEL,
            WebviewUrl::External(blank),
        )
        .title("登录 B 站 — 影取")
        .inner_size(980.0, 720.0)
        // The default WebView UA (bare AppleWebKit/WebKitGTK string) is classified by
        // Bilibili's login risk control as an outdated browser.
        .user_agent(bilibili_view::USER_AGENT)
        .build()
        .map_err(|e| AppError::Message(format!("无法打开登录窗口: {e}")))?;

        // The WebView cookie store survives across windows, so a previous session
        // would auto-complete login the moment the page opens. Clear browsing data
        // and verify it took effect before navigating, so every login starts from
        // the QR / password step.
        clear_webview_session(&win).await;
        win.navigate(login_url)
            .map_err(|e| AppError::Message(format!("无法打开登录页: {e}")))?;
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

    let status = match rx.await {
        Ok(status) => status,
        Err(_) => state.auth.auth_status(),
    };
    match &status {
        AuthStatus::LoggedIn => tracing::info!(target: "core", "auth: 登录成功"),
        _ => tracing::info!(target: "core", "auth: 登录未完成（窗口关闭或超时）"),
    }
    Ok(status)
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

    let activity_log = crate::activity_log::install(
        app_dir.join("logs"),
        crate::activity_log::DEFAULT_MAX_FILE_SIZE,
        crate::activity_log::DEFAULT_RETENTION_DAYS,
    )?;
    tracing::info!(target: "core", "app: 启动 v{}", env!("CARGO_PKG_VERSION"));

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
        wbi_keys: wbi::WbiKeyCache::new(),
        active_resolve_id: AtomicU64::new(0),
        activity_log,
    })
}

#[tauri::command]
pub fn list_log_files(
    state: State<'_, AppState>,
) -> AppResult<(String, Vec<crate::activity_log::LogFileInfo>)> {
    let dir = state.activity_log.logs_dir().to_path_buf();
    let files = crate::activity_log::list_log_files(&dir)?;
    Ok((dir.to_string_lossy().into_owned(), files))
}

#[tauri::command]
pub fn read_log_tail(state: State<'_, AppState>, name: String) -> AppResult<Vec<String>> {
    let name = Path::new(&name);
    let ok = name.components().count() == 1
        && matches!(
            name.components().next(),
            Some(std::path::Component::Normal(_))
        );
    if !ok {
        return Err(AppError::Message("非法日志文件名".into()));
    }
    let path = state.activity_log.logs_dir().join(name);
    crate::activity_log::read_log_tail(
        &path,
        crate::activity_log::TAIL_MAX_LINES,
        crate::activity_log::TAIL_MAX_BYTES,
    )
}

#[tauri::command]
pub fn clear_logs(state: State<'_, AppState>) -> AppResult<usize> {
    let today = chrono::Local::now().date_naive();
    let cleared = crate::activity_log::clear_all_logs(state.activity_log.logs_dir(), today)?;
    tracing::info!(target: "core", "log: 清空全部日志（{cleared} 个文件）");
    Ok(cleared)
}

#[tauri::command]
pub fn log_ui_events(events: Vec<crate::activity_log::UiLogEvent>) {
    for event in events {
        let level = crate::activity_log::level_from_str(&event.level);
        match level {
            tracing::Level::ERROR => {
                tracing::error!(target: "ui", "{}: {}", event.category, event.message)
            }
            tracing::Level::WARN => {
                tracing::warn!(target: "ui", "{}: {}", event.category, event.message)
            }
            tracing::Level::INFO => {
                tracing::info!(target: "ui", "{}: {}", event.category, event.message)
            }
            _ => tracing::debug!(target: "ui", "{}: {}", event.category, event.message),
        }
    }
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
