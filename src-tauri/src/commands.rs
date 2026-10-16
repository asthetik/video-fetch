use std::collections::HashMap;
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
    cleanup_orphan_work_dirs, work_dir_keep_ids,
};
use crate::error::{AppError, AppResult};
use crate::models;
use crate::models::{
    AppSettings, AuthStatus, CancelAllResult, ClearFinishedResult, DownloadConflict, DownloadJob,
    JobStatus, PauseAllResult, ResumeAllResult, VideoMeta,
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

/// Window appearance requested by the UI. Parsed once at the command
/// boundary and carried as a typed value from there on, so both platform
/// backends and the AppState pin agree on what is valid.
#[derive(Clone, Copy)]
pub enum WindowTheme {
    /// Follow the system appearance (no override).
    System,
    Light,
    Dark,
}

impl WindowTheme {
    fn parse(theme: Option<&str>) -> Result<Self, String> {
        match theme {
            None => Ok(Self::System),
            Some("light") => Ok(Self::Light),
            Some("dark") => Ok(Self::Dark),
            Some(other) => Err(format!("unknown theme: {other}")),
        }
    }
}

pub struct AppState {
    pub app_dir: PathBuf,
    pub auth: AuthManager,
    pub downloads: DownloadManager,
    pub settings: std::sync::Mutex<AppSettings>,
    /// Startup notice for the UI: set when this session's downloads were
    /// redirected because the configured save dir was unusable.
    pub save_dir_notice: Option<String>,
    /// Startup notice for the UI: set when this session's login cookies could
    /// not be materialized at launch (the cookie store still reads logged in,
    /// but `cookies.txt` is missing so downloads cannot carry the session).
    pub cookie_notice: Option<String>,
    pub ytdlp: YtDlpConfig,
    pub wbi_keys: wbi::WbiKeyCache,
    /// Shared bilibili API client (UA + timeouts); reqwest clones are cheap.
    pub http_client: reqwest::Client,
    /// In-process space_info cache: mid -> (uploader name, written-at instant), TTL 10 minutes.
    pub space_info_cache: std::sync::Mutex<HashMap<u64, (String, std::time::Instant)>>,
    /// Latest resolve request id; stale in-flight results must not emit or write cache.
    pub active_resolve_id: AtomicU64,
    /// Local activity log writer (files under app_dir/logs).
    pub activity_log: crate::activity_log::ActivityLog,
    /// Window theme pinned by the UI; System follows the OS. Windows created
    /// later (the Bilibili login window) read it at creation so they do not
    /// lag one switch behind.
    pub window_theme: std::sync::Mutex<WindowTheme>,
    /// Cached openable roots for `open_path`; invalidated on settings save
    /// and history deletes.
    pub openable_roots_cache: OpenableRootsCache,
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
    on_progress: impl Fn(Vec<models::FormatOption>, Vec<models::FormatOption>),
) -> Option<(Vec<models::FormatOption>, Vec<models::FormatOption>)> {
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
    let mut observed_video: Vec<models::FormatOption> = Vec::new();
    let mut observed_audio: Vec<models::FormatOption> = Vec::new();
    while let Some(task) = tasks.join_next().await {
        let Ok(Some((video, audio))) = task else {
            continue;
        };
        playurl::merge_multi_page_options(&mut observed_video, video);
        playurl::merge_multi_page_options(&mut observed_audio, audio);
        on_progress(observed_video.clone(), observed_audio.clone());
    }
    (!observed_video.is_empty() || !observed_audio.is_empty())
        .then_some((observed_video, observed_audio))
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

/// Event contract for `resolve_url`: once formats are known, the request must
/// not return without a terminal `complete` or `formats_failed` emission for
/// its request_id. The cache-write failure path used to `?` out raw — Err with
/// no event — so a card already rendered from a partial kept its formats
/// spinner running forever (no error, no retry). The emitter is injected so
/// the contract is unit-testable without an `AppHandle`.
fn finish_resolve_on_cache_write(
    cache_write: AppResult<()>,
    emit_formats_failed: impl FnOnce(&str),
) -> AppResult<()> {
    match cache_write {
        Ok(()) => Ok(()),
        Err(e) => {
            emit_formats_failed(&e.to_string());
            Err(e)
        }
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
    #[serde(default)]
    pub audio_format: Option<String>,
    pub output_template: Option<String>,
    /// Save as a new numbered copy; never overwrite. Does not bypass active-queue lock.
    #[serde(default, alias = "force")]
    pub save_as_copy: bool,
    #[serde(default)]
    pub uploader: String,
    /// Cover image URL stored with the job for the history page.
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    /// Source duration in seconds stored with the job for the history page.
    #[serde(default)]
    pub duration_secs: Option<u64>,
}

/// Log a command-level failure (already surfaced to the UI as a toast) so it
/// also lands in the activity log; the formatter sanitizes the message.
fn warn_cmd(scope: &str, e: &AppError) {
    tracing::warn!(
        target: "core",
        "{scope}: {}",
        crate::activity_log::clean_log_message(&e.to_string())
    );
}

fn normalize_audio_format(value: Option<String>) -> AppResult<Option<String>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed {
        "m4a" | "mp3" | "flac" => Ok(Some(trimmed.to_string())),
        other => Err(AppError::Message(format!("不支持的音频格式: {other}"))),
    }
}

#[derive(Debug, Deserialize)]
pub struct CheckConflictArgs {
    pub video_id: String,
    pub page_indexes: Vec<u32>,
    pub format_id: String,
    #[serde(default)]
    pub audio_format: Option<String>,
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
    // Mirror enqueue_download: legacy bestaudio rows count as m4a jobs.
    let audio_format = match normalize_audio_format(args.audio_format)? {
        Some(fmt) => Some(fmt),
        None if args.format_id.starts_with("bestaudio") => Some("m4a".into()),
        None => None,
    };
    let multi_page = args.page_indexes.len() > 1;
    let template = ensure_playlist_index_template(&settings.filename_template, multi_page);

    state.downloads.check_conflict(
        &args.video_id,
        &args.page_indexes,
        &args.format_id,
        audio_format.as_deref(),
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

/// Surfaced in the UI when this session's downloads are redirected because the
/// configured save directory could not be used.
pub(crate) const SAVE_DIR_UNAVAILABLE_NOTICE: &str = "保存目录不可用";

/// Surfaced in the UI when startup cookie materialization failed: the login
/// chip keeps reading "logged in" from the cookie store, but `cookies.txt` is
/// missing so this session's downloads cannot carry the login until a rewrite
/// succeeds (resume/retry rematerialize and refuse rather than silently run
/// logged-out).
pub(crate) const COOKIE_MATERIALIZE_NOTICE: &str = "登录 Cookie 准备失败，下载将无法启动";

/// Where this session's downloads write, and any startup notice about it.
pub(crate) struct SaveDirPlan {
    /// Directory downloads use this session (may differ from
    /// `settings.save_dir` when the configured one is unusable).
    pub effective: PathBuf,
    /// UI notice to show when `effective` is not the configured directory.
    pub notice: Option<String>,
}

/// Fill in a first-run `save_dir` and choose the directory this session's
/// downloads write to.
pub(crate) fn apply_save_dir(
    settings: &mut AppSettings,
    default_dir: PathBuf,
    app_dir: &Path,
) -> AppResult<SaveDirPlan> {
    let intended = if settings.save_dir.is_empty() {
        // First run: record the default, but only when it survives a UTF-8
        // round-trip. settings.json stores a String; a lossy write would pin a
        // U+FFFD path the real directory can never match, and every later
        // startup would keep falling back from that melt.
        if let Ok(utf8) = default_dir.clone().into_os_string().into_string() {
            settings.save_dir = utf8;
            let _ = settings_store::save_settings(app_dir, settings);
        }
        default_dir
    } else {
        PathBuf::from(&settings.save_dir)
    };

    if std::fs::create_dir_all(&intended).is_ok() {
        return Ok(SaveDirPlan {
            effective: intended,
            notice: None,
        });
    }
    // A directory that cannot be created right now (external drive unmounted,
    // network share gone) must not rewrite the configured save_dir: the
    // fallback is for this session only, and the UI is told where downloads
    // actually went.
    let fallback = fallback_save_dir(app_dir);
    std::fs::create_dir_all(&fallback)?;
    Ok(SaveDirPlan {
        effective: fallback,
        notice: Some(SAVE_DIR_UNAVAILABLE_NOTICE.to_string()),
    })
}

fn rematerialize_cookies(state: &AppState) -> AppResult<Option<PathBuf>> {
    state.auth.materialize_cookies_file().map_err(|e| {
        // Explicit text: the raw io string ("Is a directory") would reach the
        // user's toast as-is with no mention of what actually failed.
        AppError::Message(format!("Cookie 文件准备失败，请稍后重试: {e}"))
    })
}

/// Materialize `cookies.txt` for this session and report the UI notice when
/// the rewrite fails. A locked/ACL'd file must not block launch: the failure
/// is logged and a notice is recorded instead — and every resume/retry
/// re-materializes the file and refuses to restart while the rewrite still
/// fails, rather than silently running logged-out.
fn startup_cookie_notice(auth: &AuthManager) -> Option<String> {
    if let Err(e) = auth.materialize_cookies_file() {
        tracing::warn!(target: "core", "app: Cookie 文件准备失败: {e}");
        return Some(COOKIE_MATERIALIZE_NOTICE.to_string());
    }
    None
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

/// Pure URL classification, no network. Space URLs enter the list view;
/// everything else keeps the existing resolve flow (and its errors).
#[tauri::command]
pub fn detect_url(url: String) -> models::UrlKind {
    if let Some(mid) = platform::parse_space_mid(&url) {
        return models::UrlKind::Space { mid };
    }
    if let Ok(parsed) = Url::parse(url.trim())
        && parsed.host_str() == Some("space.bilibili.com")
    {
        return models::UrlKind::InvalidSpace;
    }
    let canonical = platform::canonicalize_video_url(&url);
    if platform::detect_platform(&canonical).is_some() {
        models::UrlKind::Video
    } else {
        models::UrlKind::Unknown
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
    let url = platform::require_supported(&url).map_err(|e| AppError::Message(e.into()))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Load cookies once: the same snapshot feeds the yt-dlp fallback file and the
    // playurl/nav Cookie header (avoids two cookie-file reads per resolve).
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
                tracing::debug!(target: "core", "resolve: 缓存命中 {}", meta.id);
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

    let playurl_formats: Option<(Vec<models::FormatOption>, Vec<models::FormatOption>)> =
        if let Some(view) = &view_meta {
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
                        Ok((video, audio)) => Some((video, audio)),
                        Err(e) => {
                            tracing::warn!(target: "core", "resolve: playurl 失败，回退 yt-dlp: {e}");
                            state.wbi_keys.invalidate();
                            None
                        }
                    }
                }
                (Some(bvid), Some(keys)) => {
                    let app_handle = app.clone();
                    let progress_emit =
                        move |formats: Vec<models::FormatOption>,
                              audio_formats: Vec<models::FormatOption>| {
                            let _ = app_handle.emit(
                                RESOLVE_FORMATS_PROGRESS_EVENT,
                                &ResolveMetaEvent {
                                    request_id,
                                    meta: models::VideoMeta {
                                        formats,
                                        audio_formats,
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

    let final_meta = match (view_meta, playurl_formats) {
        (Some(mut view), Some((formats, audio_formats))) => {
            view.formats = formats;
            view.audio_formats = audio_formats;
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
                        crate::activity_log::clean_log_message(&e.to_string())
                    );
                    return Err(e);
                }
            }
        }
    };

    if !is_current() {
        return Err(AppError::Message("解析已取消（有更新的请求）".into()));
    }
    for key in resolve_cache::store_cache_keys(&url, &final_meta.id, scope) {
        finish_resolve_on_cache_write(
            state.downloads.upsert_resolve_cache(&key, &final_meta, now),
            |message| {
                let _ = app.emit(
                    RESOLVE_FORMATS_FAILED_EVENT,
                    &ResolveFormatsFailedEvent {
                        request_id,
                        error: message.to_string(),
                    },
                );
            },
        )?;
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
pub async fn space_list_videos(
    state: State<'_, AppState>,
    mid: u64,
    pn: u32,
    keyword: Option<String>,
    order: Option<String>,
) -> AppResult<models::SpacePage> {
    let pn = pn.max(1);
    let order = order
        .as_deref()
        .and_then(crate::space::SpaceOrder::parse)
        .unwrap_or(crate::space::SpaceOrder::Pubdate);
    let keyword = keyword.unwrap_or_default();

    let stored_cookies = state.auth.cookies()?;
    let cookie_header = stored_cookies
        .as_deref()
        .map(cookies::cookie_header_for_bilibili)
        .filter(|s| !s.is_empty());
    let client = state.http_client.clone();

    let primary = match state
        .wbi_keys
        .get_or_fetch(&client, cookie_header.as_deref())
        .await
    {
        Ok(keys) => {
            crate::space::fetch_arc_search(
                &client,
                &keys,
                mid,
                pn,
                &keyword,
                order,
                cookie_header.as_deref(),
            )
            .await
        }
        Err(e) => Err(AppError::Message(format!("wbi keys 获取失败: {e}"))),
    };

    match primary {
        Ok(page) => Ok(page),
        Err(primary_err) => {
            tracing::warn!(
                target: "core",
                "space: 主路径失败，尝试降级: {}",
                primary_err
            );
            state.wbi_keys.invalidate();
            if !crate::space::fallback_allowed(&keyword, order) {
                return Err(crate::space::degraded_reject_error(&keyword));
            }
            // Only the yt-dlp fallback reads the cookies file; the primary
            // path talks to the API with the header alone.
            let cookies_path = state
                .auth
                .materialize_cookies_file_from(stored_cookies.as_deref())?;
            let page = pn as u64;
            let size = crate::space::SPACE_PAGE_SIZE as u64;
            let start = (page - 1) * size + 1;
            let end = page * size;
            crate::space::fetch_via_ytdlp(
                &state.ytdlp.yt_dlp_path,
                mid,
                start,
                end,
                cookies_path.as_deref(),
            )
            .await
        }
    }
}

#[tauri::command]
pub async fn space_info(state: State<'_, AppState>, mid: u64) -> AppResult<models::SpaceInfo> {
    const TTL: std::time::Duration = std::time::Duration::from_secs(600);
    if let Ok(cache) = state.space_info_cache.lock()
        && let Some((name, at)) = cache.get(&mid)
        && at.elapsed() < TTL
    {
        return Ok(models::SpaceInfo { name: name.clone() });
    }

    let stored_cookies = state.auth.cookies()?;
    let cookie_header = stored_cookies
        .as_deref()
        .map(cookies::cookie_header_for_bilibili)
        .filter(|s| !s.is_empty());
    let client = state.http_client.clone();

    let fetched = async {
        let keys = state
            .wbi_keys
            .get_or_fetch(&client, cookie_header.as_deref())
            .await?;
        crate::space::fetch_acc_info(&client, &keys, mid, cookie_header.as_deref()).await
    }
    .await;

    match fetched {
        Ok(name) => {
            // An empty name means an anomalous payload, not a real value; do
            // not pin it in the cache where it would outlive the anomaly.
            if !name.is_empty()
                && let Ok(mut cache) = state.space_info_cache.lock()
            {
                cache.insert(mid, (name.clone(), std::time::Instant::now()));
            }
            Ok(models::SpaceInfo { name })
        }
        // Spec: an acc/info failure must not block the list; the UI falls back to the "N videos" header.
        Err(e) => {
            tracing::warn!(target: "core", "space: acc/info 失败（UI 缺省）: {e}");
            Ok(models::SpaceInfo {
                name: String::new(),
            })
        }
    }
}

/// Async so the per-item DB/FS loop of a multi-P selection runs on the async
/// runtime (the `space_enqueue_batch` precedent) instead of the main thread.
#[tauri::command]
pub async fn enqueue_download(
    state: State<'_, AppState>,
    args: EnqueueArgs,
) -> AppResult<DownloadJob> {
    if args.page_indexes.is_empty() {
        return Err(AppError::Message("请至少选择一个分 P".into()));
    }
    // Checked here, not just in resolve_url: this URL becomes an argv entry
    // for yt-dlp, and the two commands are reachable independently (the home
    // page passes the input box's current text, which outruns its own resolve).
    let base_url =
        platform::require_supported(&args.url).map_err(|e| AppError::Message(e.into()))?;

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
    let audio_format = match normalize_audio_format(args.audio_format.clone())? {
        Some(fmt) => Some(fmt),
        None if args.format_id.starts_with("bestaudio") => Some("m4a".into()),
        None => None,
    };
    // FLAC is only real on the uncapped Hi-Res tier; a capped tier would
    // silently upsample a lossy stream into a "lossless" file.
    if audio_format.as_deref() == Some("flac")
        && crate::ytdlp::audio_tier_cap(&args.format_id).is_some()
    {
        return Err(AppError::Message("FLAC 仅支持 Hi-Res 无损档位".into()));
    }
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
    // Conflict naming must look where this session actually writes: the
    // fallback dir while the configured save_dir is unavailable.
    let save_dir = state.downloads.effective_save_dir()?;
    let page_count = args.page_indexes.len();

    let mut last = None;
    for page_index in args.page_indexes {
        let page_template = if save_as_copy {
            naming::next_available_output_template(
                &save_dir,
                &template,
                &title,
                &args.video_id,
                &uploader,
                page_index,
                audio_format.as_deref(),
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
            audio_format: audio_format.clone(),
            title: title.clone(),
            uploader: uploader.clone(),
            output_template: page_template,
            status: JobStatus::Pending,
            progress: 0.0,
            error: None,
            output_path: None,
            thumbnail_url: args.thumbnail_url.clone(),
            duration_secs: args.duration_secs,
            file_size: None,
            created_at: None,
        };
        last = Some(
            state
                .downloads
                .enqueue(job, save_as_copy)
                .inspect_err(|e| {
                    warn_cmd(
                        &format!("download: 入队失败（{} P{page_index}）", args.video_id),
                        e,
                    );
                })?,
        );
    }
    tracing::info!(
        target: "core",
        "download: 入队 {}（{}，格式 {}，{} P{}）",
        crate::activity_log::clean_log_message(&title),
        args.video_id,
        args.format_id,
        page_count,
        if save_as_copy { "，另存副本" } else { "" }
    );
    last.ok_or_else(|| AppError::Message("未能创建下载任务".into()))
}

#[derive(Debug, Deserialize)]
pub struct SpaceBatchArgs {
    pub items: Vec<models::BatchEnqueueItem>,
    pub format_id: String,
    #[serde(default)]
    pub audio_format: Option<String>,
    /// Space owner name, filled by the frontend from `space_info`. Batch
    /// enqueue has no per-video resolve, but every video in a space shares
    /// this uploader, and the skip-existing prediction needs it to expand
    /// `%(uploader)s` the same way the single-video path does. Empty (older
    /// callers / serde default) falls back to the empty-string expansion.
    #[serde(default)]
    pub uploader: String,
}

fn record_batch_outcome(
    result: &mut models::BatchEnqueueResult,
    kind: crate::download::EnqueueKind,
) {
    match kind {
        crate::download::EnqueueKind::Enqueued => result.enqueued += 1,
        crate::download::EnqueueKind::AlreadyExists => result.skipped_existing += 1,
        crate::download::EnqueueKind::DuplicateActive => result.skipped_active += 1,
    }
}

/// Fixed tier set offered by the batch dialog (spec §4.4). Anything else is
/// rejected instead of flowing an arbitrary selector string into yt-dlp -f.
const BATCH_FORMAT_TIERS: &[&str] = &[
    "bestvideo+bestaudio/best",
    "vh1080",
    "vh720",
    "vh480",
    "bestaudio",
];

/// Pure batch-argument guards, extracted for testability: known tier, non-empty
/// items, audio mode carries an audio format, FLAC only on uncapped tiers.
fn validate_space_batch_args(
    items: &[models::BatchEnqueueItem],
    format_id: &str,
    audio_format: Option<&str>,
) -> AppResult<()> {
    if !BATCH_FORMAT_TIERS.contains(&format_id) {
        return Err(AppError::Message(format!("不支持的批量档位: {format_id}")));
    }
    if items.is_empty() {
        return Err(AppError::Message("请至少选择一个视频".into()));
    }
    if audio_format.is_none() && format_id.starts_with("bestaudio") {
        return Err(AppError::Message("音频批量必须指定音频格式".into()));
    }
    if audio_format == Some("flac") && crate::ytdlp::audio_tier_cap(format_id).is_some() {
        return Err(AppError::Message("FLAC 仅支持 Hi-Res 无损档位".into()));
    }
    Ok(())
}

/// Build the DownloadJob for one batch item from the shared batch args. Pure
/// so tests can pin the exact skip-existing prediction inputs — the uploader
/// included — that the batch path feeds `enqueue_classified`.
pub(crate) fn batch_item_to_job(
    args: &SpaceBatchArgs,
    item: &models::BatchEnqueueItem,
    audio_format: &Option<String>,
    output_template: &str,
) -> DownloadJob {
    let title = if item.title.is_empty() {
        item.bvid.clone()
    } else {
        item.title.clone()
    };
    DownloadJob {
        id: String::new(),
        url: format!("https://www.bilibili.com/video/{}", item.bvid),
        video_id: item.bvid.clone(),
        page_index: 1,
        format_id: args.format_id.clone(),
        audio_format: audio_format.clone(),
        title,
        // The space owner's name from the batch args — the same prediction
        // input the single-video path records, so a `%(uploader)s` template
        // expands identically for the dedup and the conflict pre-check.
        uploader: args.uploader.clone(),
        output_template: output_template.to_string(),
        status: JobStatus::Pending,
        progress: 0.0,
        error: None,
        output_path: None,
        thumbnail_url: item.cover.clone(),
        duration_secs: item.duration_secs,
        file_size: None,
        created_at: None,
    }
}

/// Batch enqueue: no per-video resolve; the unified tier goes straight to yt-dlp (spec §4.4).
/// Async so the per-item DB/disk loop never blocks the main thread on large batches.
#[tauri::command]
pub async fn space_enqueue_batch(
    state: State<'_, AppState>,
    args: SpaceBatchArgs,
) -> AppResult<models::BatchEnqueueResult> {
    let audio_format = normalize_audio_format(args.audio_format.clone())?;
    validate_space_batch_args(&args.items, &args.format_id, audio_format.as_deref())?;
    let settings = state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))?
        .clone();
    naming::validate_output_template(&settings.filename_template).map_err(AppError::Message)?;
    let _ = rematerialize_cookies(&state)?;

    let mut result = models::BatchEnqueueResult {
        enqueued: 0,
        skipped_existing: 0,
        skipped_active: 0,
        failed: Vec::new(),
    };
    for item in &args.items {
        if !crate::space::is_valid_bvid(&item.bvid) {
            result.failed.push(models::BatchEnqueueFailed {
                bvid: item.bvid.clone(),
                error: "无效的视频 ID".into(),
            });
            continue;
        }
        let mut job = batch_item_to_job(&args, item, &audio_format, &settings.filename_template);
        match state.downloads.enqueue_classified(&mut job, false) {
            Ok(kind) => record_batch_outcome(&mut result, kind),
            Err(e) => result.failed.push(models::BatchEnqueueFailed {
                bvid: item.bvid.clone(),
                error: e.to_string(),
            }),
        }
    }
    tracing::info!(
        target: "core",
        "space: 批量入队 成功 {} / 已存在 {} / 队列重复 {} / 失败 {}",
        result.enqueued,
        result.skipped_existing,
        result.skipped_active,
        result.failed.len()
    );
    Ok(result)
}

#[tauri::command]
pub fn list_jobs(state: State<'_, AppState>) -> AppResult<Vec<DownloadJob>> {
    state.downloads.list()
}

#[tauri::command]
pub async fn cancel_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 取消 {id}");
    state
        .downloads
        .cancel(&id)
        .await
        .inspect_err(|e| warn_cmd(&format!("download: 取消失败 {id}"), e))
}

#[tauri::command]
pub async fn cancel_all_jobs(state: State<'_, AppState>) -> AppResult<CancelAllResult> {
    let result = state
        .downloads
        .cancel_all()
        .await
        .inspect_err(|e| warn_cmd("download: 取消全部失败", e))?;
    tracing::info!(target: "core", "download: 取消全部（{} 个）", result.cancelled);
    Ok(result)
}

#[tauri::command]
pub fn clear_finished_jobs(state: State<'_, AppState>) -> AppResult<ClearFinishedResult> {
    clear_finished_jobs_impl(&state)
}

/// [`clear_finished_jobs`] body, split out so tests can drive the clear path
/// without a Tauri `State`.
fn clear_finished_jobs_impl(state: &AppState) -> AppResult<ClearFinishedResult> {
    let result = state
        .downloads
        .clear_finished()
        .inspect_err(|e| warn_cmd("history: 清空失败", e))?;
    // The rows are gone: their output directories must stop being openable
    // roots. Drop the cache so the next `open_path` recomputes from the
    // surviving history (same invalidate-on-write as `save_settings`).
    state.openable_roots_cache.invalidate();
    tracing::info!(target: "core", "history: 清空已完成（{} 条）", result.cleared);
    Ok(result)
}

#[tauri::command]
pub fn retry_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    retry_job_impl(&state, &id)
}

/// [`retry_job`] body, split out so tests can drive the restart path without
/// a Tauri `State`.
fn retry_job_impl(state: &AppState, id: &str) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 重试 {id}");
    // Same cookie remedy as enqueue: rewrite cookies.txt before the runner
    // spawns yt-dlp, and refuse to restart when the rewrite fails — silently
    // continuing as guest while the UI shows logged in is the bug this blocks.
    let _ = rematerialize_cookies(state)?;
    state
        .downloads
        .retry(id)
        .inspect_err(|e| warn_cmd(&format!("download: 重试失败 {id}"), e))
}

#[tauri::command]
pub async fn pause_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 暂停 {id}");
    state
        .downloads
        .pause(&id)
        .await
        .inspect_err(|e| warn_cmd(&format!("download: 暂停失败 {id}"), e))
}

#[tauri::command]
pub async fn resume_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    resume_job_impl(&state, &id).await
}

/// [`resume_job`] body, split out so tests can drive the restart path without
/// a Tauri `State`.
async fn resume_job_impl(state: &AppState, id: &str) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 继续 {id}");
    // Same cookie remedy as enqueue: rewrite cookies.txt before the runner
    // spawns yt-dlp, and refuse to restart when the rewrite fails — silently
    // continuing as guest while the UI shows logged in is the bug this blocks.
    let _ = rematerialize_cookies(state)?;
    state
        .downloads
        .resume(id)
        .await
        .inspect_err(|e| warn_cmd(&format!("download: 继续失败 {id}"), e))
}

#[tauri::command]
pub async fn pause_all_jobs(state: State<'_, AppState>) -> AppResult<PauseAllResult> {
    let result = state
        .downloads
        .pause_all()
        .await
        .inspect_err(|e| warn_cmd("download: 暂停全部失败", e))?;
    tracing::info!(target: "core", "download: 暂停全部（{} 个）", result.paused);
    Ok(result)
}

#[tauri::command]
pub async fn resume_all_jobs(state: State<'_, AppState>) -> AppResult<ResumeAllResult> {
    resume_all_jobs_impl(&state).await
}

/// [`resume_all_jobs`] body, split out so tests can drive the restart path
/// without a Tauri `State`.
async fn resume_all_jobs_impl(state: &AppState) -> AppResult<ResumeAllResult> {
    // Same cookie remedy as enqueue: rewrite cookies.txt before the runners
    // spawn yt-dlp, and refuse to restart when the rewrite fails.
    let _ = rematerialize_cookies(state)?;
    let result = state
        .downloads
        .resume_all()
        .await
        .inspect_err(|e| warn_cmd("download: 继续全部失败", e))?;
    tracing::info!(target: "core", "download: 继续全部（{} 个）", result.resumed);
    Ok(result)
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
pub async fn delete_job(state: State<'_, AppState>, args: DeleteJobArgs) -> AppResult<()> {
    delete_job_impl(&state, &args.id, args.delete_file).await
}

/// [`delete_job`] body, split out so tests can drive the delete path without
/// a Tauri `State`.
async fn delete_job_impl(state: &AppState, id: &str, delete_file: bool) -> AppResult<()> {
    tracing::info!(target: "core", "download: 删除 {}（含文件={}）", id, delete_file);
    state
        .downloads
        .delete(id, delete_file)
        .await
        .inspect_err(|e| warn_cmd(&format!("download: 删除失败 {id}"), e))?;
    // The row is gone: its output directory must stop being an openable root.
    // Drop the cache so the next `open_path` recomputes from the surviving
    // history (same invalidate-on-write as `save_settings`).
    state.openable_roots_cache.invalidate();
    Ok(())
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

/// Startup notice about the save directory: set when the configured one was
/// unusable and this session's downloads were redirected to the fallback.
/// `None` when nothing needs saying. Shown once by the UI at startup.
#[tauri::command]
pub fn get_save_dir_notice(state: State<'_, AppState>) -> Option<String> {
    // Gated on the session override still being active: once the user
    // changes the save dir, downloads go to the configured path and a
    // persistent hint claiming the app-data fallback would lie for the
    // rest of the session.
    let override_active = state.downloads.save_dir_override_active().unwrap_or(false);
    save_dir_notice_for(state.save_dir_notice.clone(), override_active)
}

/// Derivation of the save-dir hint: the startup notice is only shown while
/// the session fallback it describes is still in effect.
fn save_dir_notice_for(notice: Option<String>, override_active: bool) -> Option<String> {
    if !override_active {
        return None;
    }
    notice
}

/// Startup notice about login cookies: set when `cookies.txt` could not be
/// materialized at launch while the cookie store still holds a login. `None`
/// when nothing needs saying. Shown once by the UI at startup.
#[tauri::command]
pub fn get_cookie_notice(state: State<'_, AppState>) -> Option<String> {
    state.cookie_notice.clone()
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: AppSettings) -> AppResult<()> {
    naming::validate_output_template(&settings.filename_template)
        .map_err(AppError::Message)
        .inspect_err(|e| warn_cmd("settings: 保存失败", e))?;
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
    settings_store::save_settings(&state.app_dir, &settings)
        .inspect_err(|e| warn_cmd("settings: 保存失败", e))?;
    *state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))? = settings.clone();
    state
        .downloads
        .update_settings(settings)
        .inspect_err(|e| warn_cmd("settings: 保存失败", e))?;
    // The save folder may have moved: drop the cached openable roots so the
    // next open_path recomputes them from the new settings plus the history.
    state.openable_roots_cache.invalidate();
    // A no-op save logs nothing: "nothing happened" fails the record criteria.
    if !changed.is_empty() {
        tracing::info!(target: "core", "settings: 保存 {}", changed.join("、"));
    }
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
    let status = state
        .auth
        .import_cookies_file(Path::new(&path))
        .inspect_err(|e| warn_cmd("auth: 导入 Cookie 失败", e))?;
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
            // Best-effort here (unlike resume/retry, which raise): the login is
            // already stored, so a cookies.txt write failure must not flip the
            // result — but it is logged, not swallowed (same message family as
            // the startup path in build_app_state).
            if let Err(e) = state.auth.materialize_cookies_file() {
                tracing::warn!(target: "core", "app: Cookie 文件准备失败: {e}");
            }
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
            tracing::warn!(target: "core", "auth: 清理浏览数据失败: {e}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        if !cookies::has_bilibili_sessdata(&collect_bilibili_cookies(win)) {
            return;
        }
        tracing::debug!(
            target: "core",
            "auth: 清理后 SESSDATA 仍存在（第 {} 次尝试）",
            attempt + 1
        );
    }
    tracing::warn!(target: "core", "auth: 放弃等待浏览数据清理，继续导航");
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
        // The login page is the one place the app's own cookie handling does
        // not reach: a WebView keeps session cookies in the platform's
        // browsing-data store, and `clear_auth` cannot clean it once this
        // window is gone — which it always is, because a successful login
        // closes it. So "退出登录" removed cookies.txt and left the SESSDATA
        // on disk. Incognito keeps that store in memory, which also covers a
        // crash: there is nothing written to leave behind.
        .incognito(true)
        // The default WebView UA (bare AppleWebKit/WebKitGTK string) is classified by
        // Bilibili's login risk control as an outdated browser.
        .user_agent(bilibili_view::USER_AGENT)
        .build()
        .map_err(|e| AppError::Message(format!("无法打开登录窗口: {e}")))?;

        // The theme switch only reached windows open at the time; theme this
        // new window as well. This command is async (off the main thread), so
        // the AppKit work is dispatched to the main thread.
        let pinned = state
            .window_theme
            .lock()
            .map(|guard| *guard)
            .unwrap_or(WindowTheme::System);
        let themed = win.clone();
        let _ = win.run_on_main_thread(move || {
            if let Err(e) = apply_window_theme(&themed, pinned) {
                tracing::warn!(target: "core", "auth: 登录窗口主题同步失败: {e}");
            }
        });

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

/// Extensions that make a *directory* something the OS launches rather than
/// reveals. A `.app` bundle opens as an application, not as a folder; the rest
/// are the same idea elsewhere in LaunchServices.
const LAUNCHABLE_DIR_EXTS: &[&str] = &[
    "app",
    "bundle",
    "framework",
    "kext",
    "xpc",
    "plugin",
    "prefPane",
    "appex",
    "qlgenerator",
    "mdimporter",
    "saver",
    "workflow",
    "action",
];

fn has_ext_from(path: &Path, known: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| known.iter().any(|k| k.eq_ignore_ascii_case(ext)))
}

fn is_app_output(path: &Path) -> bool {
    has_ext_from(path, naming::OUTPUT_EXTS) || has_ext_from(path, naming::AUDIO_OUTPUT_EXTS)
}

/// Resolve `raw` and require it to be something the app itself produced, under
/// a directory the app itself writes to.
///
/// Handing a path to the system's default handler is the same call as "run it"
/// for a `.app`, a shell script or a `.desktop` entry, and a scheme-bearing
/// string (`https:`, `ms-msdt:`) reaches a protocol handler instead. The
/// plugin's own scope does not help here: it validates the plugin's `open-path`
/// *command*, while the free `open_path` used below goes straight to the opener.
fn ensure_openable(raw: &str, roots: &[PathBuf]) -> AppResult<PathBuf> {
    let refused = || AppError::Message("无法打开：只允许打开应用保存的下载与日志".into());
    let resolved = Path::new(raw).canonicalize().map_err(|_| refused())?;
    if !roots.iter().any(|root| resolved.starts_with(root)) {
        return Err(refused());
    }
    let openable = if resolved.is_dir() {
        !has_ext_from(&resolved, LAUNCHABLE_DIR_EXTS)
    } else {
        is_app_output(&resolved)
    };
    if !openable {
        return Err(refused());
    }
    Ok(resolved)
}

/// Cached `openable_roots`. Computing the roots canonicalizes every history
/// output's parent — O(history) disk probes — which is far too heavy to redo
/// on every `open_path` (it froze the window on large histories). Recomputed
/// on first use and invalidated on writes that move the roots: a settings
/// save (the save folder is the root that moves, and a save-folder change
/// re-reads the whole history anyway) and a history delete (the deleted row's
/// output directory must drop out of the openable set).
#[derive(Default)]
pub struct OpenableRootsCache {
    roots: std::sync::Mutex<Option<Vec<PathBuf>>>,
}

impl OpenableRootsCache {
    /// Cached roots, or run `compute` once and remember the result.
    pub fn get_or_compute(&self, compute: impl FnOnce() -> Vec<PathBuf>) -> Vec<PathBuf> {
        let mut slot = match self.roots.lock() {
            Ok(guard) => guard,
            // Poisoned by a panic mid-update: the value is a pure cache, so
            // recompute instead of failing the open.
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(roots) = slot.as_ref() {
            return roots.clone();
        }
        let roots = compute();
        *slot = Some(roots.clone());
        roots
    }

    /// Drop the cache so the next open recomputes (a settings save may have
    /// moved the save folder out of the old root set, or a history delete may
    /// have removed the row that made a directory openable).
    pub fn invalidate(&self) {
        let mut slot = match self.roots.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = None;
    }
}

/// The directories the app writes into: the log folder, the current save
/// folder, and wherever earlier downloads landed — the save folder can have
/// been changed since those rows were written.
fn openable_roots(state: &AppState) -> Vec<PathBuf> {
    fn resolve(dir: &Path, what: &str) -> Option<PathBuf> {
        match dir.canonicalize() {
            Ok(path) => Some(path),
            Err(e) => {
                // Dropped rather than guessed at, but never silently: a root
                // that falls out here makes the paths under it unopenable with
                // no other explanation.
                tracing::warn!(
                    "open_path: 无法解析{what} {}（{e}），其下的路径将无法打开",
                    dir.display()
                );
                None
            }
        }
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    roots.extend(resolve(state.activity_log.logs_dir(), "日志目录"));
    let configured = match state.settings.lock() {
        Ok(settings) => Some(PathBuf::from(&settings.save_dir)),
        Err(_) => {
            tracing::warn!("open_path: settings 锁中毒，保存目录不计入可打开范围");
            None
        }
    };
    if let Some(dir) = &configured {
        roots.extend(resolve(dir, "保存目录"));
    }
    // While a startup fallback is active, this session's downloads land in it
    // even though the configured save_dir is unchanged; both stay openable.
    if let Ok(effective) = state.downloads.effective_save_dir()
        && Some(&effective) != configured.as_ref()
    {
        roots.extend(resolve(&effective, "本次会话的保存目录"));
    }
    let jobs = state.downloads.list().unwrap_or_else(|e| {
        tracing::warn!("open_path: 读取任务列表失败（{e}），历史任务的目录不计入");
        Vec::new()
    });
    for job in jobs {
        if let Some(parent) = job
            .output_path
            .as_deref()
            .and_then(|p| Path::new(p).parent())
        {
            roots.extend(resolve(parent, "历史任务的保存目录"));
        }
    }
    roots
}

/// Async like the other heavy commands; the roots themselves come from the
/// cache, so the per-call cost is one `canonicalize` of the target path.
#[tauri::command]
pub async fn open_path(state: State<'_, AppState>, path: String) -> AppResult<()> {
    // Validated as resolved, opened as given. `canonicalize` is what makes the
    // root check sound, but on Windows it also returns a `\\?\`-prefixed path,
    // and the opener hands that to ShellExecuteW, which does not understand the
    // form — it would fail to open, and report success while doing it.
    let roots = state
        .openable_roots_cache
        .get_or_compute(|| openable_roots(&state));
    ensure_openable(&path, &roots)?;
    tauri_plugin_opener::open_path(&path, None::<&str>)
        .map_err(|e| AppError::Message(format!("无法打开路径: {e}")))?;
    Ok(())
}

/// Mirror the theme choice onto the native window. macOS renders window
/// chrome from the window's own appearance only — an app-level setAppearance
/// is accepted by AppKit but ignored for the titlebar — so the window gets
/// the appearance set directly; other platforms take tao's per-window theme.
#[tauri::command]
pub fn set_window_theme(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    theme: Option<String>,
) -> Result<(), String> {
    let theme = WindowTheme::parse(theme.as_deref())?;
    *state
        .window_theme
        .lock()
        .map_err(|_| "theme lock poisoned".to_string())? = theme;
    apply_window_theme_to_all(&window, theme)
}

/// Theme every open window — the main one plus the embedded Bilibili login
/// window when it is up; a failure on one window still themes the rest.
fn apply_window_theme_to_all(
    window: &tauri::WebviewWindow,
    theme: WindowTheme,
) -> Result<(), String> {
    let mut first_error = None;
    for (_, open) in window.app_handle().webview_windows() {
        if let Err(e) = apply_window_theme(&open, theme) {
            first_error.get_or_insert(e);
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Whether the user enabled Increase Contrast in Accessibility settings.
/// AppKit applies the high-contrast appearance variant automatically to
/// system-following windows; a pinned appearance must pick it itself, or
/// those users lose the variant.
#[cfg(target_os = "macos")]
fn increase_contrast_enabled() -> bool {
    use objc2::{class, msg_send, runtime::AnyObject};

    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let enabled: objc2::runtime::Bool =
            msg_send![workspace, accessibilityDisplayShouldIncreaseContrast];
        enabled.as_bool()
    }
}

/// Applies the appearance straight to the NSWindow's `appearance` property.
///
/// This deliberately bypasses tao's theme bookkeeping (tao only records
/// themes set through its own path): `Window::theme()` and the
/// `tauri://theme-changed` event keep reporting the app-level value, so the
/// window theme must never be read back through tao — `AppState.window_theme`
/// is the source of truth, e.g. for windows created later.
#[cfg(target_os = "macos")]
fn apply_window_theme(window: &tauri::WebviewWindow, theme: WindowTheme) -> Result<(), String> {
    use objc2::{class, msg_send, runtime::AnyObject};

    let high_contrast = increase_contrast_enabled();
    let appearance_name: Option<&std::ffi::CStr> = match theme {
        WindowTheme::Dark => Some(if high_contrast {
            c"NSAppearanceNameAccessibilityDarkAqua"
        } else {
            c"NSAppearanceNameDarkAqua"
        }),
        WindowTheme::Light => Some(if high_contrast {
            c"NSAppearanceNameAccessibilityAqua"
        } else {
            c"NSAppearanceNameAqua"
        }),
        // System: clear the override so the window tracks the system again.
        WindowTheme::System => None,
    };

    unsafe {
        let ns_window = window.ns_window().map_err(|e| e.to_string())? as *mut AnyObject;
        if ns_window.is_null() {
            return Err("ns_window unavailable".into());
        }
        let appearance: *mut AnyObject = match appearance_name {
            Some(name) => {
                let name: *mut AnyObject =
                    msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
                msg_send![class!(NSAppearance), appearanceNamed: name]
            }
            None => std::ptr::null_mut(),
        };
        let _: () = msg_send![ns_window, setAppearance: appearance];
        // macOS 26+ accepts the appearance change but defers the titlebar
        // recomposite until the next full content commit (page navigation
        // repaints it); the appearance state itself updates immediately.
        // Force the shadow and the theme-frame view to redraw in case that
        // brings the recomposite forward — a full fix needs an upstream
        // tao/macOS change.
        let _: () = msg_send![ns_window, invalidateShadow];
        let frame: *mut AnyObject = msg_send![ns_window, contentView];
        let frame: *mut AnyObject = msg_send![frame, superview];
        let _: () = msg_send![frame, setNeedsDisplay: objc2::runtime::Bool::new(true)];
        let _: () = msg_send![ns_window, displayIfNeeded];
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn apply_window_theme(window: &tauri::WebviewWindow, theme: WindowTheme) -> Result<(), String> {
    use tauri::Theme;

    let theme = match theme {
        WindowTheme::Dark => Some(Theme::Dark),
        WindowTheme::Light => Some(Theme::Light),
        WindowTheme::System => None,
    };
    window.set_theme(theme).map_err(|e| e.to_string())
}

/// Convert a user-picked directory into the String settings.json stores.
/// The config holds a String, so a non-UTF-8 pick cannot round-trip: melting it
/// with `to_string` would persist a U+FFFD path the real directory can never
/// match (the same class `apply_save_dir` guards for the default dir). Reject
/// instead so the user picks another folder.
fn picked_dir_to_string(dir: PathBuf) -> Result<String, AppError> {
    dir.into_os_string()
        .into_string()
        .map_err(|_| AppError::Message("该路径包含无法保存的字符，请另选目录".into()))
}

#[tauri::command]
pub async fn pick_save_dir(app: AppHandle) -> AppResult<String> {
    let folder =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .map_err(|e| AppError::Message(format!("对话框任务失败: {e}")))?;

    match folder {
        Some(path) => {
            let dir = path
                .into_path()
                .map_err(|e| AppError::Message(format!("对话框路径无效: {e}")))?;
            picked_dir_to_string(dir)
        }
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

// Log-shape invariant (see the comment on `run` in lib.rs): a healthy
// startup produces ZERO log lines. The three tracing sites in this function
// are anomaly-gated (they fire only when a previous session left orphans or
// interrupted rows behind, or the save-dir fallback is active); do not add
// routine lifecycle logging here. The cookie materialization warn lives in
// `startup_cookie_notice` (also anomaly-gated). `startup_silence_guard` in
// lib.rs pins the call sites and their guards.
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
    let save_dir_plan = apply_save_dir(&mut settings, default_save_dir(app, &app_dir), &app_dir)?;

    let logs_dir = app_dir.join("logs");
    let activity_log = match crate::activity_log::install(
        logs_dir.clone(),
        crate::activity_log::DEFAULT_MAX_FILE_SIZE,
        crate::activity_log::DEFAULT_RETENTION_DAYS,
    ) {
        Ok(activity_log) => activity_log,
        Err(e) => {
            // Logging is best-effort infrastructure: never block app launch on
            // a log-directory failure. The viewer commands still work and will
            // surface the missing files in the UI.
            eprintln!("videofetch: activity log unavailable: {e}");
            crate::activity_log::ActivityLog::disabled(logs_dir)
        }
    };
    // Open the DB only after the subscriber exists so migration records land.
    let db = crate::db::Db::open(&app_dir.join("jobs.db"))?;
    let auth = AuthManager::new(cache_dir);
    // A locked/ACL'd cookies.txt must not block launch, but the failure is not
    // swallowed either: the UI notice is recorded here (and the failure logged),
    // and every resume/retry re-materializes the file and refuses to restart
    // when the rewrite still fails.
    let cookie_notice = startup_cookie_notice(&auth);

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
    // The startup fallback lives only in this session's download path; the
    // configured save_dir in settings stays as the user wrote it.
    if save_dir_plan.effective != Path::new(&settings.save_dir) {
        downloads.set_save_dir_override(Some(save_dir_plan.effective.clone()))?;
        tracing::warn!(
            target: "core",
            "app: 保存目录不可用，本次下载改存 {}",
            save_dir_plan.effective.display()
        );
    }

    let active_ids = work_dir_keep_ids(&downloads.list()?);
    let removed_orphans = cleanup_orphan_work_dirs(&work_root, &active_ids);
    if removed_orphans > 0 {
        tracing::info!(target: "core", "app: 清理孤儿工作目录 {removed_orphans} 个");
    }
    // A row left Running by the previous session has no runner: surface it as
    // Paused so the UI offers resume instead of a frozen "downloading" bar.
    let reconciled = downloads.reconcile_interrupted_runs()?;
    if reconciled > 0 {
        tracing::info!(target: "core", "app: 中断的下载已转为暂停 {reconciled} 个");
    }

    let http_client = reqwest::Client::builder()
        .user_agent(bilibili_view::USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| AppError::Message(format!("创建 HTTP 客户端失败: {e}")))?;

    Ok(AppState {
        app_dir,
        auth,
        downloads,
        settings: std::sync::Mutex::new(settings),
        save_dir_notice: save_dir_plan.notice,
        cookie_notice,
        ytdlp,
        wbi_keys: wbi::WbiKeyCache::new(),
        http_client,
        space_info_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        active_resolve_id: AtomicU64::new(0),
        activity_log,
        window_theme: std::sync::Mutex::new(WindowTheme::System),
        openable_roots_cache: OpenableRootsCache::default(),
    })
}

/// Versions of the engine sidecars, fixed at build time from the pin files
/// that fetch_sidecars.py downloads from (single source of truth).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineVersions {
    pub yt_dlp: String,
    pub ffmpeg: String,
}

#[tauri::command]
pub fn get_engine_versions() -> EngineVersions {
    EngineVersions {
        yt_dlp: env!("SIDECAR_YTDLP_VERSION").to_string(),
        ffmpeg: env!("SIDECAR_FFMPEG_VERSION").to_string(),
    }
}

#[cfg(test)]
mod engine_version_tests {
    use super::*;

    /// Independent re-implementation of the build.rs extraction: re-reads the
    /// pin files and recomputes the expected values, so any drift between the
    /// injected env values and the pin files turns these tests red.
    fn ytdlp_tag_from_disk() -> String {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/requirements-sidecars.txt"),
        )
        .expect("scripts/requirements-sidecars.txt must exist next to the crate");
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            // Same tolerance envelope as build.rs (whitespace around `==`,
            // trailing comments) so a legitimate pin reformat stays green.
            let Some((name, rest)) = line.split_once("==") else {
                continue;
            };
            if name.trim() != "yt-dlp" {
                continue;
            }
            let raw = rest.split('#').next().unwrap_or("").trim();
            let parts: Vec<&str> = raw.split('.').collect();
            assert_eq!(parts.len(), 3, "unexpected yt-dlp pin {raw:?}");
            assert_eq!(parts[0].len(), 4, "unexpected yt-dlp pin {raw:?}");
            return format!("{}.{:0>2}.{:0>2}", parts[0], parts[1], parts[2]);
        }
        panic!("no yt-dlp pin found in scripts/requirements-sidecars.txt");
    }

    fn ffmpeg_version_from_disk() -> String {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/fetch_sidecars.py"),
        )
        .expect("scripts/fetch_sidecars.py must exist next to the crate");
        for line in text.lines() {
            let Some((_, rest)) = line.split_once("FFMPEG_VERSION = \"") else {
                continue;
            };
            let end = rest.find('"').expect("unterminated FFMPEG_VERSION value");
            return rest[..end].to_string();
        }
        panic!("no FFMPEG_VERSION assignment found in scripts/fetch_sidecars.py");
    }

    #[test]
    fn injected_ytdlp_version_matches_pin_file() {
        assert_eq!(env!("SIDECAR_YTDLP_VERSION"), ytdlp_tag_from_disk());
    }

    #[test]
    fn injected_ffmpeg_version_matches_fetch_script() {
        assert_eq!(env!("SIDECAR_FFMPEG_VERSION"), ffmpeg_version_from_disk());
    }

    #[test]
    fn injected_versions_have_expected_shapes() {
        let ytdlp = env!("SIDECAR_YTDLP_VERSION");
        let parts: Vec<&str> = ytdlp.split('.').collect();
        assert_eq!(parts.len(), 3, "yt-dlp tag {ytdlp:?}");
        assert_eq!(parts[0].len(), 4, "yt-dlp tag {ytdlp:?}");
        assert_eq!(parts[1].len(), 2, "yt-dlp tag {ytdlp:?}");
        assert_eq!(parts[2].len(), 2, "yt-dlp tag {ytdlp:?}");
        assert!(parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())));

        let ffmpeg = env!("SIDECAR_FFMPEG_VERSION");
        let parts: Vec<&str> = ffmpeg.split('.').collect();
        assert_eq!(parts.len(), 3, "ffmpeg version {ffmpeg:?}");
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "ffmpeg version {ffmpeg:?}"
        );
    }
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
    let cleared = crate::activity_log::clear_all_logs(state.activity_log.logs_dir(), today)
        .inspect_err(|e| warn_cmd("log: 清空失败", e))?;
    tracing::info!(target: "core", "log: 清空全部日志（{cleared} 个文件）");
    Ok(cleared)
}

#[tauri::command]
pub fn log_ui_events(events: Vec<crate::activity_log::UiLogEvent>) {
    for event in events.into_iter().take(500) {
        let message: String = event
            .message
            .chars()
            .take(crate::activity_log::MAX_LOG_MESSAGE_CHARS)
            .collect();
        let message = crate::activity_log::clean_log_message(&message);
        let level = crate::activity_log::level_from_str(&event.level);
        match level {
            tracing::Level::ERROR => {
                tracing::error!(target: "ui", "ui:{}: {}", event.category, message)
            }
            tracing::Level::WARN => {
                tracing::warn!(target: "ui", "ui:{}: {}", event.category, message)
            }
            tracing::Level::INFO => {
                tracing::info!(target: "ui", "ui:{}: {}", event.category, message)
            }
            _ => tracing::debug!(target: "ui", "ui:{}: {}", event.category, message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `open_path` hands its argument straight to the system's default
    /// handler, so the path is resolved against the roots the app itself
    /// writes to before anything is opened.
    #[test]
    fn open_path_reaches_only_the_apps_own_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("save");
        std::fs::create_dir_all(&root).unwrap();
        let inside = root.join("clip.mp4");
        std::fs::write(&inside, b"x").unwrap();
        // A media file outside the roots: without the root check this one
        // passes every other guard, so it is the case that pins it.
        let elsewhere = dir.path().join("elsewhere.mp4");
        std::fs::write(&elsewhere, b"x").unwrap();
        let roots = vec![root.canonicalize().unwrap()];

        assert!(ensure_openable(inside.to_str().unwrap(), &roots).is_ok());
        assert!(ensure_openable(root.to_str().unwrap(), &roots).is_ok());
        for refused in [
            elsewhere.to_str().unwrap(),
            "/etc/passwd",
            "/tmp/../../etc/hosts",
            "https://example.com/evil.mp4",
            "ms-msdt:/id",
            "--version",
            "does-not-exist.mp4",
        ] {
            assert!(
                ensure_openable(refused, &roots).is_err(),
                "should have been refused: {refused}"
            );
        }
    }

    /// Opening and executing are the same call: a `.app` directory is launched
    /// rather than revealed, and a `.sh` / `.command` / `.desktop` / `.lnk` is
    /// run. Only what the app itself writes is openable.
    #[test]
    fn open_path_refuses_something_the_app_did_not_produce() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("save");
        let bundle = root.join("payload.app");
        std::fs::create_dir_all(&bundle).unwrap();
        let script = root.join("evil.sh");
        std::fs::write(&script, b"x").unwrap();
        let roots = vec![root.canonicalize().unwrap()];

        assert!(ensure_openable(bundle.to_str().unwrap(), &roots).is_err());
        assert!(ensure_openable(script.to_str().unwrap(), &roots).is_err());
    }

    /// A9: `openable_roots` canonicalizes every history output's parent — O(history)
    /// disk probes per call, which froze the window on every `open_path` in large
    /// histories. The cache must compute once and recompute only when invalidated
    /// (a settings save — the save folder is the root that moves — or a history
    /// delete; the delete paths are pinned in `openable_roots_delete_tests`).
    #[test]
    fn openable_roots_cache_computes_once_until_invalidated() {
        let cache = OpenableRootsCache::default();
        let computes = std::cell::Cell::new(0usize);
        // Copy-able closure (captures only a Cell borrow) so the cache can take
        // it by value per call.
        let compute = || {
            computes.set(computes.get() + 1);
            vec![PathBuf::from("/save")]
        };
        assert_eq!(cache.get_or_compute(compute), vec![PathBuf::from("/save")]);
        assert_eq!(cache.get_or_compute(compute), vec![PathBuf::from("/save")]);
        assert_eq!(computes.get(), 1, "the second read must reuse the cache");
        cache.invalidate();
        assert_eq!(cache.get_or_compute(compute), vec![PathBuf::from("/save")]);
        assert_eq!(
            computes.get(),
            2,
            "a settings change must force a recompute"
        );
    }

    /// A9: the heavy commands must be `async fn` — Tauri runs sync commands on
    /// the main thread, where a cancel-all of 20 jobs (serial taskkill + multi-GB
    /// wipes) or a multi-P enqueue freezes the window. The `AsyncFn` bound fails
    /// to build if a command reverts to a blocking `fn` returning `AppResult`.
    /// `space_enqueue_batch` is the already-async canary: if the bound itself
    /// were wrong, that line would fail too.
    #[test]
    fn heavy_commands_are_async() {
        fn assert_async1<A, T, F>(_f: F)
        where
            F: AsyncFn(A) -> T,
        {
        }
        fn assert_async2<A, B, T, F>(_f: F)
        where
            F: AsyncFn(A, B) -> T,
        {
        }
        assert_async2(space_enqueue_batch);
        assert_async2(enqueue_download);
        assert_async2(cancel_job);
        assert_async1(cancel_all_jobs);
        assert_async2(pause_job);
        assert_async2(resume_job);
        assert_async1(pause_all_jobs);
        assert_async1(resume_all_jobs);
        assert_async2(delete_job);
        assert_async2(open_path);
    }

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

    /// Event contract: a cache-write failure must end the request in a
    /// `formats_failed` emission before the Err propagates. Returning Err with
    /// no event (the pre-fix `?`) left a card already rendered from a partial
    /// on its formats spinner forever: no error, no retry, download disabled.
    #[test]
    fn cache_write_failure_emits_formats_failed_before_err() {
        let emitted: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let result =
            finish_resolve_on_cache_write(Err(AppError::Message("db locked".into())), |message| {
                emitted.lock().unwrap().push(message.to_string())
            });
        assert!(result.is_err());
        assert_eq!(*emitted.lock().unwrap(), vec!["db locked".to_string()]);
    }

    /// Success must not fake a failure event; `complete` is emitted by the
    /// caller once every cache key is written.
    #[test]
    fn cache_write_success_emits_no_formats_failed() {
        let emitted: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let result = finish_resolve_on_cache_write(Ok(()), |message| {
            emitted.lock().unwrap().push(message.to_string())
        });
        assert!(result.is_ok());
        assert!(emitted.lock().unwrap().is_empty());
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

/// A9 follow-up: history deletes are writes that shrink the openable set. A
/// deleted job's output directory is a root only because its row named it, so
/// once the row is gone the cached roots must drop it — the same
/// invalidate-on-write `save_settings` does, pinned here for both delete
/// command paths.
#[cfg(test)]
mod openable_roots_delete_tests {
    use super::*;
    use crate::db::Db;
    use crate::download::{ChannelProgressEmitter, Downloader};
    use crate::ytdlp::ProgressUpdate;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Never invoked: these tests insert terminal rows and delete them, so no
    /// runner ever starts.
    struct UnusedDownloader;

    #[async_trait]
    impl Downloader for UnusedDownloader {
        async fn run(
            &self,
            _job: &DownloadJob,
            _token: u64,
            _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
        ) -> Result<PathBuf, String> {
            panic!("no download may start in these tests");
        }
    }

    /// A finished history row: the kind whose output directory enters the
    /// openable roots through the history scan.
    fn finished_job(id: &str, output_path: &Path) -> DownloadJob {
        DownloadJob {
            uploader: String::new(),
            id: id.into(),
            url: format!("https://www.bilibili.com/video/{id}"),
            video_id: id.into(),
            page_index: 1,
            format_id: "80".into(),
            audio_format: None,
            title: "demo".into(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            status: JobStatus::Done,
            progress: 1.0,
            error: None,
            output_path: Some(output_path.to_string_lossy().into()),
            thumbnail_url: None,
            duration_secs: None,
            file_size: None,
            created_at: None,
        }
    }

    /// App state whose history holds the rows `make_jobs` builds from the
    /// temp root. The rows land in the DB before any runner could see them.
    fn state_with_history(
        make_jobs: impl FnOnce(&Path) -> Vec<DownloadJob>,
    ) -> (tempfile::TempDir, AppState) {
        let root = tempfile::tempdir().unwrap();
        let db = Db::open(&root.path().join("jobs.db")).unwrap();
        for job in make_jobs(root.path()) {
            db.insert_job(&job).unwrap();
        }
        let (emitter, _rx) = ChannelProgressEmitter::new();
        let settings = AppSettings {
            save_dir: root.path().join("save").to_string_lossy().into(),
            ..AppSettings::default()
        };
        let downloads = DownloadManager::new(
            db,
            settings.clone(),
            Arc::new(UnusedDownloader),
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            root.path().join("work"),
        )
        .unwrap();
        let state = AppState {
            app_dir: root.path().to_path_buf(),
            auth: AuthManager::new(root.path().join("cache")),
            downloads,
            settings: Mutex::new(settings),
            save_dir_notice: None,
            cookie_notice: None,
            ytdlp: YtDlpConfig {
                yt_dlp_path: root.path().join("yt-dlp"),
                ffmpeg_path: None,
            },
            wbi_keys: wbi::WbiKeyCache::new(),
            http_client: reqwest::Client::new(),
            space_info_cache: Mutex::new(HashMap::new()),
            active_resolve_id: AtomicU64::new(0),
            activity_log: crate::activity_log::ActivityLog::disabled(root.path().join("logs")),
            window_theme: Mutex::new(WindowTheme::System),
            openable_roots_cache: OpenableRootsCache::default(),
        };
        (root, state)
    }

    /// One job whose output lands outside every other root: `elsewhere` is
    /// openable only while its history row exists.
    fn stray_output_job() -> (tempfile::TempDir, AppState) {
        state_with_history(|root| {
            let stray = root.join("elsewhere");
            std::fs::create_dir_all(&stray).unwrap();
            vec![finished_job("job-1", &stray.join("clip.mp4"))]
        })
    }

    #[tokio::test]
    async fn delete_job_invalidates_openable_roots_cache() {
        let (_root, state) = stray_output_job();
        let computes = std::cell::Cell::new(0usize);
        let compute = || {
            computes.set(computes.get() + 1);
            openable_roots(&state)
        };
        let before = state.openable_roots_cache.get_or_compute(compute);
        assert!(
            before.iter().any(|r| r.ends_with("elsewhere")),
            "precondition: the history row's directory is an openable root"
        );
        assert_eq!(computes.get(), 1);

        delete_job_impl(&state, "job-1", false).await.unwrap();

        let after = state.openable_roots_cache.get_or_compute(compute);
        assert_eq!(
            computes.get(),
            2,
            "delete_job must drop the cached roots so they recompute"
        );
        assert!(
            !after.iter().any(|r| r.ends_with("elsewhere")),
            "the deleted job's directory must leave the openable set"
        );
    }

    #[test]
    fn clear_finished_jobs_invalidates_openable_roots_cache() {
        let (_root, state) = stray_output_job();
        let computes = std::cell::Cell::new(0usize);
        let compute = || {
            computes.set(computes.get() + 1);
            openable_roots(&state)
        };
        let before = state.openable_roots_cache.get_or_compute(compute);
        assert!(
            before.iter().any(|r| r.ends_with("elsewhere")),
            "precondition: the history row's directory is an openable root"
        );
        assert_eq!(computes.get(), 1);

        clear_finished_jobs_impl(&state).unwrap();

        let after = state.openable_roots_cache.get_or_compute(compute);
        assert_eq!(
            computes.get(),
            2,
            "clear_finished_jobs must drop the cached roots so they recompute"
        );
        assert!(
            !after.iter().any(|r| r.ends_with("elsewhere")),
            "the cleared job's directory must leave the openable set"
        );
    }
}

#[cfg(test)]
mod detect_url_tests {
    use super::*;

    #[test]
    fn detect_url_classifies_three_kinds() {
        assert_eq!(
            detect_url("https://space.bilibili.com/470995011?spm_id_from=x".into()),
            models::UrlKind::Space { mid: 470995011 }
        );
        assert_eq!(
            detect_url("https://www.bilibili.com/video/BV1xx411c7mD".into()),
            models::UrlKind::Video
        );
        assert_eq!(
            detect_url("https://example.com/nope".into()),
            models::UrlKind::Unknown
        );
    }

    #[test]
    fn detect_url_invalid_space_mid() {
        assert_eq!(
            detect_url("https://space.bilibili.com/abc".into()),
            models::UrlKind::InvalidSpace
        );
        assert_eq!(
            detect_url("https://space.bilibili.com/".into()),
            models::UrlKind::InvalidSpace
        );
        assert_eq!(
            detect_url("https://space.bilibili.com/470995011/video".into()),
            models::UrlKind::Space { mid: 470995011 }
        );
    }
}

#[cfg(test)]
mod space_batch_tests {
    use super::*;
    use crate::download::EnqueueKind;

    #[test]
    fn record_batch_outcome_counts_kinds() {
        let mut r = models::BatchEnqueueResult {
            enqueued: 0,
            skipped_existing: 0,
            skipped_active: 0,
            failed: Vec::new(),
        };
        record_batch_outcome(&mut r, EnqueueKind::Enqueued);
        record_batch_outcome(&mut r, EnqueueKind::AlreadyExists);
        record_batch_outcome(&mut r, EnqueueKind::DuplicateActive);
        record_batch_outcome(&mut r, EnqueueKind::Enqueued);
        assert_eq!(r.enqueued, 2);
        assert_eq!(r.skipped_existing, 1);
        assert_eq!(r.skipped_active, 1);
        assert!(r.failed.is_empty());
    }

    #[test]
    fn space_batch_args_validation() {
        let items = |n: usize| {
            (0..n)
                .map(|i| models::BatchEnqueueItem {
                    bvid: format!("BV{i}"),
                    title: "t".into(),
                    cover: None,
                    duration_secs: None,
                })
                .collect::<Vec<_>>()
        };
        assert!(validate_space_batch_args(&items(1), "vh1080", None).is_ok());
        assert!(validate_space_batch_args(&items(1), "bestvideo+bestaudio/best", None).is_ok());
        assert!(validate_space_batch_args(&items(1), "worstvideo", None).is_err());
        assert!(validate_space_batch_args(&items(0), "vh1080", None).is_err());
        assert!(validate_space_batch_args(&items(1), "bestaudio", None).is_err());
        assert!(validate_space_batch_args(&items(1), "bestaudio", Some("flac")).is_ok());
        assert!(validate_space_batch_args(&items(1), "bestaudio[abr<=192]", Some("flac")).is_err());
    }

    #[test]
    fn batch_rejects_bad_audio_format_values() {
        assert!(normalize_audio_format(Some("flac".into())).is_ok());
        assert!(normalize_audio_format(Some("ogg".into())).is_err());
    }

    /// Older callers omit `uploader`; serde must default it to the empty
    /// string and the batch job must fall back to that empty expansion
    /// instead of failing to deserialize.
    #[test]
    fn batch_item_uploader_defaults_to_empty_when_absent() {
        let args: SpaceBatchArgs = serde_json::from_value(serde_json::json!({
            "items": [{ "bvid": "BV1xx", "title": "demo" }],
            "format_id": "80",
        }))
        .unwrap();
        assert_eq!(args.uploader, "");
        let job = batch_item_to_job(&args, &args.items[0], &None, "%(title)s [%(id)s].%(ext)s");
        assert_eq!(job.uploader, "");
    }

    /// The supplied uploader must reach the job so the skip-existing
    /// prediction expands `%(uploader)s` like the single-video path.
    #[test]
    fn batch_item_job_carries_the_supplied_uploader() {
        let args: SpaceBatchArgs = serde_json::from_value(serde_json::json!({
            "items": [{ "bvid": "BV1xx", "title": "demo" }],
            "format_id": "80",
            "uploader": "up",
        }))
        .unwrap();
        let job = batch_item_to_job(
            &args,
            &args.items[0],
            &None,
            "%(uploader)s/%(title)s [%(id)s].%(ext)s",
        );
        assert_eq!(job.uploader, "up");
    }
}

#[cfg(test)]
mod window_theme_tests {
    use super::*;

    #[test]
    fn parse_accepts_only_known_modes() {
        assert!(matches!(WindowTheme::parse(None), Ok(WindowTheme::System)));
        assert!(matches!(
            WindowTheme::parse(Some("dark")),
            Ok(WindowTheme::Dark)
        ));
        assert!(matches!(
            WindowTheme::parse(Some("light")),
            Ok(WindowTheme::Light)
        ));
        assert!(WindowTheme::parse(Some("auto")).is_err());
    }

    /// Guards the NSWorkspace selector spelling: the query runs on every
    /// theme switch, so a typo would crash the app at runtime rather than
    /// fail visibly.
    #[cfg(target_os = "macos")]
    #[test]
    fn increase_contrast_query_hits_a_real_selector() {
        let _ = increase_contrast_enabled();
    }
}

#[cfg(test)]
mod save_dir_tests {
    use super::*;

    fn app_dir_under(root: &Path) -> PathBuf {
        let app_dir = root.join("app-data");
        std::fs::create_dir_all(&app_dir).unwrap();
        app_dir
    }

    /// A path whose parent is a regular file: `create_dir_all` always fails,
    /// the way a temporarily unmounted external/network drive does.
    fn never_creatable(root: &Path) -> PathBuf {
        let blocker = root.join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        blocker.join("save")
    }

    /// A transiently unavailable save_dir must not rewrite the persisted
    /// config: if it did, a drive unmounted at startup would silently move
    /// every future download into app data until the user noticed.
    /// The pure derivation behind `get_save_dir_notice`: the startup notice
    /// shows only while the session fallback is still active; once the user
    /// changes the save dir the hint must disappear with the condition
    /// instead of claiming the app-data fallback for the rest of the session.
    #[test]
    fn save_dir_notice_hides_once_the_override_is_dropped() {
        let notice = Some(SAVE_DIR_UNAVAILABLE_NOTICE.to_string());
        assert_eq!(save_dir_notice_for(notice.clone(), true), notice);
        assert_eq!(
            save_dir_notice_for(notice, false),
            None,
            "a dropped override means the fallback is over; the hint must not survive it"
        );
        assert_eq!(save_dir_notice_for(None, true), None);
    }

    #[test]
    fn transient_save_dir_failure_does_not_rewrite_or_persist_settings() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = app_dir_under(dir.path());
        let configured = never_creatable(dir.path());
        let mut settings = AppSettings {
            save_dir: configured.to_string_lossy().into_owned(),
            ..Default::default()
        };
        // A real user config on disk: the fallback must not replace it there.
        settings_store::save_settings(&app_dir, &settings).unwrap();

        let plan = apply_save_dir(&mut settings, dir.path().join("Downloads"), &app_dir)
            .expect("session fallback must still start the app");

        // The configured value survives in memory and on disk...
        assert_eq!(settings.save_dir, configured.to_string_lossy());
        let on_disk = settings_store::load_settings(&app_dir).expect("settings must load");
        assert_eq!(on_disk.save_dir, configured.to_string_lossy());
        // ...while this session downloads into the app-data fallback.
        assert_eq!(plan.effective, app_dir.join("downloads"));
        assert!(plan.effective.is_dir());
        assert_eq!(plan.notice.as_deref(), Some(SAVE_DIR_UNAVAILABLE_NOTICE));
    }

    /// A path segment valid in the OS's native encoding but not in UTF-8:
    /// raw bytes on Unix, an unpaired UTF-16 surrogate on Windows. `to_string_lossy`
    /// melts either into U+FFFD.
    fn non_utf8_name() -> std::ffi::OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(b"Downloads-\xff".to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[u16::from(b'D'), 0xD800])
        }
    }

    /// `to_string_lossy` melts a non-UTF-8 Downloads path into a U+FFFD string
    /// that never matches the real directory; persisting that melt (or the
    /// fallback it then collides with) makes the wrong path sticky across
    /// startups.
    #[test]
    fn non_utf8_default_save_dir_is_not_melted_into_settings() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = app_dir_under(dir.path());
        let raw = dir.path().join(non_utf8_name());
        // Whether the fs accepts the name is platform-dependent (APFS
        // requires valid UTF-8); the bug is about what gets recorded and used
        // either way.
        let raw_usable = std::fs::create_dir_all(&raw).is_ok();
        // First run: no configured save_dir, the default must not be melted.
        let mut settings = AppSettings::default();

        let plan =
            apply_save_dir(&mut settings, raw.clone(), &app_dir).expect("startup must not fail");

        // Nothing melted is recorded, in memory or on disk.
        assert!(settings.save_dir.is_empty());
        let on_disk = settings_store::load_settings(&app_dir).expect("settings must load");
        assert!(on_disk.save_dir.is_empty());
        assert!(!on_disk.save_dir.contains('\u{FFFD}'));
        // The session uses the real path bytes (when the fs accepts them) or
        // the app-data fallback — never a melted U+FFFD copy.
        let melted = PathBuf::from(raw.to_string_lossy().into_owned());
        assert_ne!(
            plan.effective, melted,
            "save path must not be melted into U+FFFD"
        );
        // The empty-save_dir state the settings placeholder stands in for
        // has exactly two shapes, and the notice distinguishes them: no
        // notice means the session downloads to the unprintable default,
        // a notice means it fell back to the app-data dir — the fact the
        // frontend's placeholder suppression keys on (saveDirDisplay.ts).
        if raw_usable {
            assert_eq!(plan.effective, raw);
            assert_eq!(plan.notice, None);
        } else {
            assert_eq!(plan.effective, app_dir.join("downloads"));
            assert!(plan.notice.is_some());
        }
    }

    /// The melt guard must not regress into never-persisting: a first-run
    /// default that IS valid UTF-8 is still recorded (memory + disk), so the
    /// next startup reuses the recorded directory instead of re-deriving a
    /// default that may no longer be the user's Downloads folder.
    #[test]
    fn utf8_first_run_default_save_dir_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let app_dir = app_dir_under(dir.path());
        let default_dir = dir.path().join("Downloads");
        // First run: no configured save_dir.
        let mut settings = AppSettings::default();

        let plan = apply_save_dir(&mut settings, default_dir.clone(), &app_dir)
            .expect("startup must not fail");

        // The default is recorded in memory and on disk...
        assert_eq!(settings.save_dir, default_dir.to_string_lossy());
        let on_disk = settings_store::load_settings(&app_dir).expect("settings must load");
        assert_eq!(on_disk.save_dir, default_dir.to_string_lossy());
        // ...and this session downloads there without a fallback notice.
        assert_eq!(plan.effective, default_dir);
        assert!(plan.effective.is_dir());
        assert_eq!(plan.notice, None);
    }

    /// A UTF-8 pick must survive the conversion unchanged: this exact string
    /// is what the frontend persists as `save_dir`.
    #[test]
    fn picked_dir_to_string_keeps_utf8_path() {
        let dir = PathBuf::from("/tmp/下载目录");
        assert_eq!(
            picked_dir_to_string(dir).expect("utf-8 path must convert"),
            "/tmp/下载目录"
        );
    }

    /// Melting a non-UTF-8 pick with `to_string` would hand the frontend a
    /// U+FFFD path to persist as `save_dir`; the picker must reject instead so
    /// the user chooses another folder.
    #[test]
    fn picked_dir_to_string_rejects_non_utf8_path_without_melting() {
        let raw = PathBuf::from(non_utf8_name());
        let err = picked_dir_to_string(raw)
            .expect_err("non-UTF-8 pick must not be converted via lossy melt");
        let msg = err.to_string();
        assert_eq!(msg, "该路径包含无法保存的字符，请另选目录");
        // The error text must not carry a melted path either.
        assert!(!msg.contains('\u{FFFD}'));
    }
}

/// Restart entry points (resume/retry) must rematerialize `cookies.txt` before
/// a runner spawns yt-dlp: a session whose startup materialization failed (AV
/// lock / ACL) would otherwise run the whole download logged-out while the UI
/// still shows logged in.
#[cfg(test)]
mod restart_cookie_tests {
    use super::*;
    use crate::download::{ChannelProgressEmitter, Downloader};
    use crate::ytdlp::ProgressUpdate;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tokio::time::sleep;

    /// Records whether `cookies.txt` existed at each `run()` start (in run
    /// order) and optionally parks or fails runs so tests can shape the job
    /// lifecycle around the restart entry points.
    struct CookieProbeDownloader {
        cookies_file: PathBuf,
        seen: Arc<Mutex<Vec<bool>>>,
        /// Number of initial runs that park forever (dropped when aborted).
        park_budget: Arc<Mutex<usize>>,
        fail: bool,
        scratch: tempfile::TempDir,
    }

    #[async_trait]
    impl Downloader for CookieProbeDownloader {
        async fn run(
            &self,
            job: &DownloadJob,
            _token: u64,
            _on_progress: Box<dyn Fn(ProgressUpdate) + Send>,
        ) -> Result<PathBuf, String> {
            self.seen.lock().unwrap().push(self.cookies_file.exists());
            let park = {
                let mut budget = self.park_budget.lock().unwrap();
                let park = *budget > 0;
                if park {
                    *budget -= 1;
                }
                park
            };
            if park {
                std::future::pending::<()>().await;
            }
            if self.fail {
                return Err("mock failure".into());
            }
            let path = self.scratch.path().join(format!("{}.mp4", job.id));
            std::fs::write(&path, b"mock").map_err(|e| e.to_string())?;
            Ok(path)
        }
    }

    struct TestApp {
        state: AppState,
        seen: Arc<Mutex<Vec<bool>>>,
        cookies_file: PathBuf,
        _root: tempfile::TempDir,
    }

    fn test_app(park_budget: usize, fail: bool) -> TestApp {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let save_dir = root.path().join("save");
        std::fs::create_dir_all(&save_dir).unwrap();
        let cookies_file = cache_dir.join("cookies.txt");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let downloader = Arc::new(CookieProbeDownloader {
            cookies_file: cookies_file.clone(),
            seen: Arc::clone(&seen),
            park_budget: Arc::new(Mutex::new(park_budget)),
            fail,
            scratch: tempfile::tempdir().unwrap(),
        });
        let db = crate::db::Db::open(&root.path().join("jobs.db")).unwrap();
        let (emitter, _rx) = ChannelProgressEmitter::new();
        let settings = AppSettings {
            save_dir: save_dir.to_string_lossy().into(),
            concurrency: 1,
            skip_existing: false,
            ..AppSettings::default()
        };
        let downloads = DownloadManager::new(
            db,
            settings.clone(),
            downloader,
            Arc::new(emitter),
            Arc::new(Mutex::new(HashMap::new())),
            root.path().join("work"),
        )
        .unwrap();
        let state = AppState {
            openable_roots_cache: Default::default(),
            save_dir_notice: None,
            cookie_notice: None,
            app_dir: root.path().to_path_buf(),
            auth: AuthManager::new(cache_dir),
            downloads,
            settings: Mutex::new(settings),
            ytdlp: YtDlpConfig {
                yt_dlp_path: root.path().join("yt-dlp"),
                ffmpeg_path: None,
            },
            wbi_keys: wbi::WbiKeyCache::new(),
            http_client: reqwest::Client::new(),
            space_info_cache: Mutex::new(HashMap::new()),
            active_resolve_id: AtomicU64::new(0),
            activity_log: crate::activity_log::ActivityLog::disabled(root.path().join("logs")),
            window_theme: Mutex::new(WindowTheme::System),
        };
        TestApp {
            state,
            seen,
            cookies_file,
            _root: root,
        }
    }

    /// Store a logged-in SESSDATA without writing `cookies.txt`: exactly the
    /// state a launch leaves behind when startup materialization fails.
    fn login_without_cookies_file(state: &AppState) {
        state
            .auth
            .save_cookies_from_webview(vec![Cookie {
                domain: ".bilibili.com".into(),
                include_subdomains: true,
                path: "/".into(),
                secure: true,
                expiration: 0,
                name: "SESSDATA".into(),
                value: "abc123".into(),
            }])
            .unwrap();
    }

    fn probe_job(id: &str, video_id: &str) -> DownloadJob {
        DownloadJob {
            uploader: String::new(),
            id: id.into(),
            url: format!("https://www.bilibili.com/video/{video_id}"),
            video_id: video_id.into(),
            page_index: 1,
            format_id: "80".into(),
            audio_format: None,
            title: "demo".into(),
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            status: JobStatus::Pending,
            progress: 0.0,
            error: None,
            output_path: None,
            thumbnail_url: None,
            duration_secs: None,
            file_size: None,
            created_at: None,
        }
    }

    async fn wait_for_status(state: &AppState, id: &str, status: JobStatus) {
        for _ in 0..200 {
            if let Ok(jobs) = state.downloads.list()
                && let Some(job) = jobs.iter().find(|j| j.id == id)
                && job.status == status
            {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
        panic!("job {id} never reached the expected status");
    }

    async fn wait_for_runs(seen: &Arc<Mutex<Vec<bool>>>, count: usize) {
        for _ in 0..200 {
            if seen.lock().unwrap().len() >= count {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
        panic!("downloader never reached {count} runs");
    }

    fn job_status(state: &AppState, id: &str) -> JobStatus {
        state
            .downloads
            .list()
            .unwrap()
            .iter()
            .find(|j| j.id == id)
            .unwrap()
            .status
            .clone()
    }

    /// Enqueue one job, let its first run park as Running, then pause it: the
    /// Paused row a crash-and-restart leaves behind.
    async fn paused_job(app: &TestApp, id: &str, video_id: &str) {
        app.state
            .downloads
            .enqueue(probe_job(id, video_id), false)
            .unwrap();
        wait_for_status(&app.state, id, JobStatus::Running).await;
        app.state.downloads.pause(id).await.unwrap();
        assert_eq!(job_status(&app.state, id), JobStatus::Paused);
    }

    /// Enqueue one job that fails on its own: the Failed row retry picks up.
    async fn failed_job(app: &TestApp, id: &str, video_id: &str) {
        app.state
            .downloads
            .enqueue(probe_job(id, video_id), false)
            .unwrap();
        wait_for_status(&app.state, id, JobStatus::Failed).await;
    }

    #[tokio::test]
    async fn resume_job_rematerializes_cookies_before_spawning_ytdlp() {
        let app = test_app(1, false);
        login_without_cookies_file(&app.state);
        paused_job(&app, "job-r1", "BV1r1").await;
        assert!(
            !app.cookies_file.exists(),
            "precondition: startup materialization left no cookies.txt"
        );

        let resumed = resume_job_impl(&app.state, "job-r1").await.unwrap();
        assert_eq!(resumed.status, JobStatus::Pending);
        assert!(
            app.cookies_file.exists(),
            "resume must rematerialize cookies.txt before spawning yt-dlp"
        );

        wait_for_runs(&app.seen, 2).await;
        let seen = app.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(!seen[0], "setup run must record the missing cookies file");
        assert!(
            seen[1],
            "the resumed run must observe the rematerialized cookies file"
        );
    }

    #[tokio::test]
    async fn resume_job_surfaces_cookie_materialize_failure_instead_of_running_guest() {
        let app = test_app(1, false);
        login_without_cookies_file(&app.state);
        paused_job(&app, "job-r2", "BV1r2").await;
        // Stand-in for an AV lock / ACL: the cookies path cannot be written.
        std::fs::create_dir(&app.cookies_file).unwrap();

        let err = resume_job_impl(&app.state, "job-r2").await.unwrap_err();
        assert!(
            err.to_string().contains("Cookie 文件准备失败"),
            "expected an explicit cookie error, got: {err}"
        );
        // The job must not have restarted: no guest run, row untouched.
        assert_eq!(job_status(&app.state, "job-r2"), JobStatus::Paused);
        assert_eq!(app.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn retry_job_rematerializes_cookies_before_spawning_ytdlp() {
        let app = test_app(0, true);
        login_without_cookies_file(&app.state);
        failed_job(&app, "job-t1", "BV1t1").await;
        assert!(!app.cookies_file.exists());

        let retried = retry_job_impl(&app.state, "job-t1").unwrap();
        assert_eq!(retried.status, JobStatus::Pending);
        assert!(
            app.cookies_file.exists(),
            "retry must rematerialize cookies.txt before spawning yt-dlp"
        );

        wait_for_runs(&app.seen, 2).await;
        let seen = app.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert!(!seen[0], "setup run must record the missing cookies file");
        assert!(
            seen[1],
            "the retried run must observe the rematerialized cookies file"
        );
    }

    #[tokio::test]
    async fn retry_job_surfaces_cookie_materialize_failure_instead_of_running_guest() {
        let app = test_app(0, true);
        login_without_cookies_file(&app.state);
        failed_job(&app, "job-t2", "BV1t2").await;
        std::fs::create_dir(&app.cookies_file).unwrap();

        let err = retry_job_impl(&app.state, "job-t2").unwrap_err();
        assert!(
            err.to_string().contains("Cookie 文件准备失败"),
            "expected an explicit cookie error, got: {err}"
        );
        assert_eq!(job_status(&app.state, "job-t2"), JobStatus::Failed);
        assert_eq!(app.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn resume_all_jobs_rematerializes_cookies_before_spawning_ytdlp() {
        let app = test_app(2, false);
        login_without_cookies_file(&app.state);
        paused_job(&app, "job-a1", "BV1a1").await;
        paused_job(&app, "job-a2", "BV1a2").await;
        assert!(!app.cookies_file.exists());

        let result = resume_all_jobs_impl(&app.state).await.unwrap();
        assert_eq!(result.resumed, 2);
        assert!(
            app.cookies_file.exists(),
            "resume-all must rematerialize cookies.txt before spawning yt-dlp"
        );

        wait_for_runs(&app.seen, 4).await;
        let seen = app.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 4);
        assert!(
            seen[2..].iter().all(|&s| s),
            "every resumed run must observe the rematerialized cookies file: {seen:?}"
        );
    }

    #[tokio::test]
    async fn resume_all_jobs_surfaces_cookie_materialize_failure_instead_of_running_guest() {
        let app = test_app(2, false);
        login_without_cookies_file(&app.state);
        paused_job(&app, "job-a3", "BV1a3").await;
        paused_job(&app, "job-a4", "BV1a4").await;
        std::fs::create_dir(&app.cookies_file).unwrap();

        let err = resume_all_jobs_impl(&app.state).await.unwrap_err();
        assert!(
            err.to_string().contains("Cookie 文件准备失败"),
            "expected an explicit cookie error, got: {err}"
        );
        assert_eq!(job_status(&app.state, "job-a3"), JobStatus::Paused);
        assert_eq!(job_status(&app.state, "job-a4"), JobStatus::Paused);
        assert_eq!(app.seen.lock().unwrap().len(), 2);
    }
}

/// Startup cookie materialization must leave a user-visible notice when it
/// fails: the login chip keeps reading "logged in" from the cookie store while
/// `cookies.txt` is missing, and the user would otherwise only discover the
/// problem when a download attempt errors out. The notice text is the pin —
/// dropping the set in `startup_cookie_notice` must turn these red.
#[cfg(test)]
mod startup_cookie_notice_tests {
    use super::*;

    fn logged_in_auth(root: &Path) -> AuthManager {
        let cache_dir = root.join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let auth = AuthManager::new(cache_dir);
        auth.save_cookies_from_webview(vec![Cookie {
            domain: ".bilibili.com".into(),
            include_subdomains: true,
            path: "/".into(),
            secure: true,
            expiration: 0,
            name: "SESSDATA".into(),
            value: "abc123".into(),
        }])
        .unwrap();
        auth
    }

    #[test]
    fn materialize_failure_sets_the_startup_notice() {
        let root = tempfile::tempdir().unwrap();
        let auth = logged_in_auth(root.path());
        // A directory where cookies.txt belongs makes the rewrite fail the
        // same way an AV lock does: launch must set the notice, not swallow it.
        std::fs::create_dir_all(root.path().join("cache").join("cookies.txt")).unwrap();
        let notice =
            startup_cookie_notice(&auth).expect("materialize failure must set the startup notice");
        assert_eq!(
            notice, COOKIE_MATERIALIZE_NOTICE,
            "the notice must carry the agreed UI copy"
        );
    }

    #[test]
    fn materialize_success_sets_no_notice() {
        let root = tempfile::tempdir().unwrap();
        let auth = logged_in_auth(root.path());
        assert_eq!(startup_cookie_notice(&auth), None);
        assert!(
            root.path().join("cache").join("cookies.txt").exists(),
            "precondition: the rewrite succeeded"
        );
    }

    #[test]
    fn logged_out_sets_no_notice() {
        let root = tempfile::tempdir().unwrap();
        let cache_dir = root.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let auth = AuthManager::new(cache_dir);
        assert_eq!(startup_cookie_notice(&auth), None);
    }
}

/// Tripwire for the cookie-materialize handling in `try_persist_webview_cookies`.
/// The login-window persist path is best-effort (unlike resume/retry, which
/// raise explicit errors): a cookies.txt write failure must not flip a login
/// that already succeeded, but it must not be swallowed silently either — the
/// startup path logs the same "app: Cookie 文件准备失败" family, and this
/// path has to match. A capture-based test cannot pin this (the function needs
/// a real Wry `AppHandle`/`WebviewWindow`), so this asserts the call sites in
/// the function body, same style as `startup_silence_guard` in lib.rs.
#[cfg(test)]
mod webview_cookie_persist_guard {
    /// Brace-counted body of the function declared at `signature` (a string
    /// that must appear exactly at a line start, leading `\n` included).
    /// Same helper as `startup_silence_guard` in lib.rs.
    fn fn_body<'a>(source: &'a str, signature: &str) -> &'a str {
        let start = source
            .find(signature)
            .unwrap_or_else(|| panic!("signature not found: {signature}"));
        let open = source[start..]
            .find('{')
            .map(|i| start + i)
            .unwrap_or_else(|| panic!("no body after: {signature}"));
        let mut depth = 0usize;
        for (i, c) in source[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[open..open + i + 1];
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces after: {signature}");
    }

    #[test]
    fn materialize_failure_is_logged_not_swallowed() {
        let commands = include_str!("commands.rs");
        let body = fn_body(commands, "\nfn try_persist_webview_cookies");

        assert!(
            !body.contains("let _ = state.auth.materialize_cookies_file()"),
            "a materialize_cookies_file failure must not be swallowed with `let _ =` \
             (external review #162 Minor 3): log it like the startup path does."
        );
        assert!(
            body.contains("if let Err(e) = state.auth.materialize_cookies_file()"),
            "materialize_cookies_file failure must be handled on its Err arm \
             (external review #162 Minor 3)."
        );
        assert!(
            body.contains("tracing::warn!") && body.contains("app: Cookie 文件准备失败"),
            "the failure must log a tracing::warn! in the same message family as the \
             startup path (\"app: Cookie 文件准备失败\"); keep it consistent with the \
             startup warn in build_app_state and the explicit errors in resume/retry."
        );
        // Control flow stays best-effort: the Ok arm still reports login success
        // even when the cookies file cannot be written after the store save.
        // Whitespace-collapsed so rustfmt line breaks do not matter: the gated
        // warn must sit in front of an unconditional `Some(status)` tail.
        let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains(
                "if let Err(e) = state.auth.materialize_cookies_file() { \
                 tracing::warn!(target: \"core\", \"app: Cookie 文件准备失败: {e}\"); \
                 } Some(status)"
            ),
            "materialize stays best-effort here: the saved login must still be \
             reported via Some(status) when the file write fails, with the warn \
             gated on the Err arm in between."
        );
    }
}
