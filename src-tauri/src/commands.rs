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
    /// Memoized openable-root resolution, keyed by its raw inputs.
    pub openable_roots_cache: std::sync::Mutex<Option<OpenableRootsCache>>,
    /// Set when startup fell back to another save dir for this session only:
    /// downloads run here while settings keep (and persist) the user's choice.
    pub save_dir_fallback: std::sync::Mutex<Option<String>>,
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

/// Split the startup save-dir decision into what the user keeps (state and
/// disk) and what this session runs on. A transient failure's fallback must
/// never replace the configured value: the settings UI round-trips the whole
/// struct through its autosave, so anything handed out here gets persisted on
/// the next unrelated save — a flaky mount used to relocate every future
/// download that way. Bootstrap (persist) adopts its choice for good.
pub(crate) fn split_save_dir(
    mut settings: AppSettings,
    session_dir: String,
    persist: bool,
) -> (AppSettings, Option<String>) {
    if !persist && session_dir != settings.save_dir {
        return (settings, Some(session_dir));
    }
    settings.save_dir = session_dir;
    (settings, None)
}

/// Map a panicked blocking task to the same error family as command failures.
fn join_err(e: tokio::task::JoinError) -> AppError {
    AppError::Message(format!("任务异常终止: {e}"))
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

/// Decide the startup save directory and whether the settings file changes.
/// Returns `(save_dir for this session, persist)`.
///
/// Bootstrap (empty setting) adopts the OS default — or the app fallback when
/// the default cannot round-trip through the JSON settings (non-UTF-8 path) —
/// and persists. A configured directory that cannot be created falls back for
/// THIS SESSION ONLY and must not persist: a transient failure (unplugged
/// external drive, network share down) rewriting the user's save location is
/// exactly the silent data-shift this shape prevents.
fn startup_save_dir(app_dir: &Path, configured: &str, default_dir: &Path) -> (String, bool) {
    if configured.is_empty() {
        let chosen = match default_dir.to_str() {
            Some(dir) => dir.to_string(),
            None => {
                // Explicitly not to_string_lossy: baking U+FFFD garbage into
                // the setting would fail every following startup. eprintln,
                // not tracing: no subscriber is installed yet at this point.
                eprintln!("videofetch: 默认保存目录不是合法 UTF-8，改用回退目录");
                fallback_save_dir(app_dir).to_string_lossy().into_owned()
            }
        };
        return (chosen, true);
    }
    if std::fs::create_dir_all(configured).is_err() {
        let fb = fallback_save_dir(app_dir);
        let _ = std::fs::create_dir_all(&fb);
        // eprintln, not tracing: this runs before the subscriber exists.
        eprintln!("videofetch: 保存目录不可用（{configured}），本次会话使用回退目录");
        return (fb.to_string_lossy().into_owned(), false);
    }
    (configured.to_string(), false)
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
                    // Also announce on the event channel: the invoke rejects,
                    // but a UI whose spinner keys on formats_failed must not
                    // be left waiting.
                    let _ = app.emit(
                        RESOLVE_FORMATS_FAILED_EVENT,
                        &ResolveFormatsFailedEvent {
                            request_id,
                            error: e.to_string(),
                        },
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
        if let Err(e) = state.downloads.upsert_resolve_cache(&key, &final_meta, now) {
            // Best-effort: the resolve itself succeeded and the meta below is
            // fully usable — failing here used to surface as "清晰度获取失败"
            // and disable every download button over a cache-table error.
            warn_cmd("resolve: 缓存写入失败", &e);
        }
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

#[tauri::command]
// Async so the per-item DB/disk loop never blocks the main thread on large
// batches (same shape as space_enqueue_batch).
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
    // The copy-numbering probe must scan the directory downloads actually
    // land in: when startup fell back for this session, the configured dir is
    // unusable and probing it would hand out an already-taken name.
    let effective_save_dir = state
        .save_dir_fallback
        .lock()
        .ok()
        .and_then(|fb| fb.clone())
        .unwrap_or_else(|| settings.save_dir.clone());
    let save_dir = Path::new(&effective_save_dir);
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
            uploader: (!uploader.is_empty()).then(|| uploader.clone()),
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
    /// The space's UP主 name, captured by the caller; the skip-existing
    /// prediction needs it for templates containing `%(uploader)s`.
    #[serde(default)]
    pub uploader: Option<String>,
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
    for item in args.items {
        if !crate::space::is_valid_bvid(&item.bvid) {
            result.failed.push(models::BatchEnqueueFailed {
                bvid: item.bvid,
                error: "无效的视频 ID".into(),
            });
            continue;
        }
        let title = if item.title.is_empty() {
            item.bvid.clone()
        } else {
            item.title.clone()
        };
        let mut job = DownloadJob {
            id: String::new(),
            url: format!("https://www.bilibili.com/video/{}", item.bvid),
            video_id: item.bvid.clone(),
            page_index: 1,
            format_id: args.format_id.clone(),
            audio_format: audio_format.clone(),
            title,
            // Empty means unknown (space_info can fail): store None so the
            // prediction treats it exactly like a legacy row.
            uploader: (!args.uploader.as_deref().unwrap_or("").is_empty())
                .then(|| args.uploader.clone())
                .flatten(),
            output_template: settings.filename_template.clone(),
            status: JobStatus::Pending,
            progress: 0.0,
            error: None,
            output_path: None,
            thumbnail_url: item.cover.clone(),
            duration_secs: item.duration_secs,
            file_size: None,
            created_at: None,
        };
        match state.downloads.enqueue_classified(&mut job, false) {
            Ok(kind) => record_batch_outcome(&mut result, kind),
            Err(e) => result.failed.push(models::BatchEnqueueFailed {
                bvid: item.bvid,
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
    // Blocking work (taskkill waits, work-dir deletes) runs on the blocking
    // pool: as a sync command this ran on the main thread and froze the whole
    // window for the duration.
    let downloads = state.downloads.clone();
    let scoped_id = id.clone();
    tokio::task::spawn_blocking(move || downloads.cancel(&scoped_id))
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd(&format!("download: 取消失败 {id}"), e))
}

#[tauri::command]
pub async fn cancel_all_jobs(state: State<'_, AppState>) -> AppResult<CancelAllResult> {
    let downloads = state.downloads.clone();
    let result = tokio::task::spawn_blocking(move || downloads.cancel_all())
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd("download: 取消全部失败", e))?;
    tracing::info!(target: "core", "download: 取消全部（{} 个）", result.cancelled);
    Ok(result)
}

#[tauri::command]
pub fn clear_finished_jobs(state: State<'_, AppState>) -> AppResult<ClearFinishedResult> {
    let result = state
        .downloads
        .clear_finished()
        .inspect_err(|e| warn_cmd("history: 清空失败", e))?;
    tracing::info!(target: "core", "history: 清空已完成（{} 条）", result.cleared);
    Ok(result)
}

#[tauri::command]
pub async fn retry_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 重试 {id}");
    // Same as enqueue: a stale or missing cookies.txt would silently send
    // yt-dlp out as a guest (360p / login errors) while the UI shows LoggedIn.
    // A materialize failure is this command's error, never a guest downgrade.
    let _ = rematerialize_cookies(&state)?;
    let downloads = state.downloads.clone();
    let scoped_id = id.clone();
    tokio::task::spawn_blocking(move || downloads.retry(&scoped_id))
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd(&format!("download: 重试失败 {id}"), e))
}

#[tauri::command]
pub async fn pause_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 暂停 {id}");
    let downloads = state.downloads.clone();
    let scoped_id = id.clone();
    tokio::task::spawn_blocking(move || downloads.pause(&scoped_id))
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd(&format!("download: 暂停失败 {id}"), e))
}

#[tauri::command]
pub async fn resume_job(state: State<'_, AppState>, id: String) -> AppResult<DownloadJob> {
    tracing::info!(target: "core", "download: 继续 {id}");
    // Same as enqueue: see retry_job for why the jar is re-materialized here.
    let _ = rematerialize_cookies(&state)?;
    let downloads = state.downloads.clone();
    let scoped_id = id.clone();
    tokio::task::spawn_blocking(move || downloads.resume(&scoped_id))
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd(&format!("download: 继续失败 {id}"), e))
}

#[tauri::command]
pub async fn pause_all_jobs(state: State<'_, AppState>) -> AppResult<PauseAllResult> {
    let downloads = state.downloads.clone();
    let result = tokio::task::spawn_blocking(move || downloads.pause_all())
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd("download: 暂停全部失败", e))?;
    tracing::info!(target: "core", "download: 暂停全部（{} 个）", result.paused);
    Ok(result)
}

#[tauri::command]
pub async fn resume_all_jobs(state: State<'_, AppState>) -> AppResult<ResumeAllResult> {
    // Same as resume_job: every runner this starts must see a fresh jar.
    let _ = rematerialize_cookies(&state)?;
    let downloads = state.downloads.clone();
    let result = tokio::task::spawn_blocking(move || downloads.resume_all())
        .await
        .map_err(join_err)?
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
    tracing::info!(target: "core", "download: 删除 {}（含文件={}）", args.id, args.delete_file);
    let downloads = state.downloads.clone();
    let scoped_id = args.id.clone();
    let delete_file = args.delete_file;
    tokio::task::spawn_blocking(move || downloads.delete(&scoped_id, delete_file))
        .await
        .map_err(join_err)?
        .inspect_err(|e| warn_cmd(&format!("download: 删除失败 {}", args.id), e))
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> AppResult<AppSettings> {
    let mut settings = state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))?
        .clone();
    if let Ok(fallback) = state.save_dir_fallback.lock()
        && let Some(path) = fallback.as_deref()
    {
        settings.save_dir_warning = Some(format!(
            "保存目录当前不可用；本次会话的下载会落到 {path}，配置的目录未被改动"
        ));
    }
    Ok(settings)
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, mut settings: AppSettings) -> AppResult<()> {
    naming::validate_output_template(&settings.filename_template)
        .map_err(AppError::Message)
        .inspect_err(|e| warn_cmd("settings: 保存失败", e))?;
    // Computed output, never part of the stored file.
    settings.save_dir_warning = None;
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
    let save_dir_changed = old.save_dir != settings.save_dir;
    settings_store::save_settings(&state.app_dir, &settings)
        .inspect_err(|e| warn_cmd("settings: 保存失败", e))?;
    *state
        .settings
        .lock()
        .map_err(|_| AppError::Message("settings lock poisoned".into()))? = settings.clone();
    if save_dir_changed {
        // An explicit new save dir wins for this session too.
        if let Ok(mut fallback) = state.save_dir_fallback.lock() {
            *fallback = None;
        }
    }
    let mut effective = settings.clone();
    if let Ok(fallback) = state.save_dir_fallback.lock()
        && let Some(path) = fallback.as_deref()
    {
        effective.save_dir = path.to_string();
    }
    state
        .downloads
        .update_settings(effective)
        .inspect_err(|e| warn_cmd("settings: 保存失败", e))?;
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

/// Resolved openable roots, kept while their raw inputs are unchanged. The
/// inputs themselves are the cache key: a changed save dir or a different set
/// of historical output directories re-resolves immediately, while repeated
/// opens skip the per-job canonicalize round-trips.
pub struct OpenableRootsCache {
    inputs: Vec<String>,
    roots: Vec<PathBuf>,
}

fn roots_from_cache(
    cache: &std::sync::Mutex<Option<OpenableRootsCache>>,
    inputs: Vec<String>,
    compute: impl FnOnce() -> (Vec<PathBuf>, bool),
) -> Vec<PathBuf> {
    if let Ok(mut slot) = cache.lock() {
        if let Some(hit) = slot.as_ref()
            && hit.inputs == inputs
        {
            return hit.roots.clone();
        }
        // `cacheable = false` means the resolution was incomplete (a root that
        // would not resolve is mount state, not a stable fact): return what
        // resolved but cache nothing, so the next call retries instead of
        // serving a false "not openable" until some unrelated change.
        let (roots, cacheable) = compute();
        if cacheable {
            *slot = Some(OpenableRootsCache {
                inputs,
                roots: roots.clone(),
            });
        }
        return roots;
    }
    compute().0
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
    // The cache key is the raw input set, gathered before resolution.
    let mut inputs: Vec<String> =
        vec![state.activity_log.logs_dir().to_string_lossy().into_owned()];
    match state.settings.lock() {
        Ok(settings) => inputs.push(settings.save_dir.clone()),
        Err(_) => tracing::warn!("open_path: settings 锁中毒，保存目录不计入可打开范围"),
    }
    let jobs = state.downloads.list().unwrap_or_else(|e| {
        tracing::warn!("open_path: 读取任务列表失败（{e}），历史任务的目录不计入");
        Vec::new()
    });
    for job in &jobs {
        if let Some(parent) = job
            .output_path
            .as_deref()
            .and_then(|p| Path::new(p).parent())
        {
            inputs.push(parent.to_string_lossy().into_owned());
        }
    }
    roots_from_cache(&state.openable_roots_cache, inputs, move || {
        let mut roots: Vec<PathBuf> = Vec::new();
        let mut complete = true;
        let push =
            |dir: &Path, what: &str, roots: &mut Vec<PathBuf>, complete: &mut bool| match resolve(
                dir, what,
            ) {
                Some(path) => roots.push(path),
                None => *complete = false,
            };
        push(
            state.activity_log.logs_dir(),
            "日志目录",
            &mut roots,
            &mut complete,
        );
        if let Ok(settings) = state.settings.lock() {
            push(
                Path::new(&settings.save_dir),
                "保存目录",
                &mut roots,
                &mut complete,
            );
        }
        for job in &jobs {
            if let Some(parent) = job
                .output_path
                .as_deref()
                .and_then(|p| Path::new(p).parent())
            {
                push(parent, "历史任务的保存目录", &mut roots, &mut complete);
            }
        }
        (roots, complete)
    })
}

#[tauri::command]
pub async fn open_path(state: State<'_, AppState>, path: String) -> AppResult<()> {
    // Validated as resolved, opened as given. `canonicalize` is what makes the
    // root check sound, but on Windows it also returns a `\\?\`-prefixed path,
    // and the opener hands that to ShellExecuteW, which does not understand the
    // form — it would fail to open, and report success while doing it.
    ensure_openable(&path, &openable_roots(&state))?;
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

    let settings = settings_store::load_settings(&app_dir)?;
    let (session_save_dir, persist_save_dir) = startup_save_dir(
        &app_dir,
        &settings.save_dir,
        &default_save_dir(app, &app_dir),
    );
    let (settings, save_dir_fallback) =
        split_save_dir(settings, session_save_dir.clone(), persist_save_dir);
    if persist_save_dir {
        let _ = settings_store::save_settings(&app_dir, &settings);
    }
    let _ = std::fs::create_dir_all(&session_save_dir);
    // The session downloads into the effective dir even when a fallback is in
    // play; get_settings keeps reporting (and saves keep storing) the user's
    // configured one.
    let mut session_settings = settings.clone();
    if let Some(fb) = &save_dir_fallback {
        session_settings.save_dir = fb.clone();
    }

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
    // Loud, not fatal: the app must still start, but a failure here used to
    // disappear entirely and leave the session downloading as a guest.
    if let Err(e) = auth.materialize_cookies_file() {
        tracing::warn!(
            target: "core",
            "cookies: 启动物化失败（下载前将重试）: {}",
            crate::activity_log::clean_log_message(&e.to_string())
        );
    }

    let work_root = app_dir.join("download-work");
    std::fs::create_dir_all(&work_root)?;

    let ytdlp = sidecar::resolve_ytdlp_config(app);
    let progress: Arc<dyn ProgressEmitter> = Arc::new(TauriProgressEmitter::new(app.clone()));
    let downloads = DownloadManager::with_ytdlp(
        db,
        session_settings,
        ytdlp.clone(),
        Some(cookies_file.clone()),
        progress,
        work_root.clone(),
    )?;

    let active_ids = work_dir_keep_ids(&downloads.list()?);
    let removed_orphans = cleanup_orphan_work_dirs(&work_root, &active_ids);
    if removed_orphans > 0 {
        tracing::info!(target: "core", "app: 清理孤儿工作目录 {removed_orphans} 个");
    }
    // Rows left Running or Pending by the previous session have no runner: surface it as
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
        ytdlp,
        wbi_keys: wbi::WbiKeyCache::new(),
        http_client,
        space_info_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
        active_resolve_id: AtomicU64::new(0),
        activity_log,
        window_theme: std::sync::Mutex::new(WindowTheme::System),
        openable_roots_cache: std::sync::Mutex::new(None),
        save_dir_fallback: std::sync::Mutex::new(save_dir_fallback),
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

    /// Value: protects=the save-dir startup decision: a configured location
    /// that fails only transiently must not be rewritten; fails_when=the
    /// fallback path persists (the old bug — an unplugged drive permanently
    /// moved every download into app data) or the session does not fall back;
    /// why_new=the rewrite was silent, sticky and untested; seam=none
    #[test]
    fn a_transiently_unusable_save_dir_falls_back_for_the_session_only() {
        let app_dir = tempfile::tempdir().unwrap();
        let default_dir = app_dir.path().join("Downloads");
        // A regular file in the way: create_dir_all under it must fail.
        let blocker = app_dir.path().join("blocker");
        std::fs::write(&blocker, b"x").unwrap();
        let configured = blocker.join("downloads").to_string_lossy().into_owned();
        let (session, persist) = startup_save_dir(app_dir.path(), &configured, &default_dir);
        assert!(!persist, "a transient failure must not rewrite the setting");
        assert_eq!(session, fallback_save_dir(app_dir.path()).to_string_lossy());
    }

    #[test]
    fn an_empty_save_dir_bootstraps_from_the_default_and_persists() {
        let app_dir = tempfile::tempdir().unwrap();
        let default_dir = app_dir.path().join("Downloads");
        let (session, persist) = startup_save_dir(app_dir.path(), "", &default_dir);
        assert!(persist);
        assert_eq!(session, default_dir.to_string_lossy());
    }

    #[test]
    fn an_existing_save_dir_is_kept_and_never_rewritten() {
        let app_dir = tempfile::tempdir().unwrap();
        let configured_dir = tempfile::tempdir().unwrap();
        let configured = configured_dir.path().to_string_lossy().into_owned();
        let default_dir = app_dir.path().join("Downloads");
        let (session, persist) = startup_save_dir(app_dir.path(), &configured, &default_dir);
        assert!(!persist);
        assert_eq!(session, configured);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_default_dir_takes_the_fallback_instead_of_lossy_garbage() {
        use std::os::unix::ffi::OsStringExt;
        let app_dir = tempfile::tempdir().unwrap();
        let weird = PathBuf::from(std::ffi::OsString::from_vec(b"bad/\xff\xfe".to_vec()));
        let (session, persist) = startup_save_dir(app_dir.path(), "", &weird);
        assert!(persist);
        assert!(!session.contains('\u{FFFD}'), "no lossy garbage: {session}");
        assert_eq!(session, fallback_save_dir(app_dir.path()).to_string_lossy());
    }

    /// Value: protects=the user's configured save dir surviving a transient
    /// startup failure; fails_when=split_save_dir puts the session fallback
    /// into the kept settings (the settings UI would persist it on its next
    /// autosave and permanently relocate every download); why_new=A7's first
    /// fix kept the file correct at startup but leaked the fallback through
    /// get_settings + autosave; seam=none
    #[test]
    fn a_transient_fallback_never_replaces_the_kept_save_dir() {
        let settings = AppSettings {
            save_dir: "/media/usb/downloads".into(),
            ..AppSettings::default()
        };
        let (kept, fallback) = split_save_dir(settings, "/app-data/downloads".into(), false);
        assert_eq!(kept.save_dir, "/media/usb/downloads");
        assert_eq!(fallback.as_deref(), Some("/app-data/downloads"));
    }

    #[test]
    fn bootstrap_adopts_the_default_dir_without_a_fallback() {
        let settings = AppSettings {
            save_dir: String::new(),
            ..AppSettings::default()
        };
        let (kept, fallback) = split_save_dir(settings, "/home/u/Downloads".into(), true);
        assert_eq!(kept.save_dir, "/home/u/Downloads");
        assert!(fallback.is_none());
    }

    #[test]
    fn a_usable_save_dir_passes_through_unchanged() {
        let settings = AppSettings {
            save_dir: "/media/usb/downloads".into(),
            ..AppSettings::default()
        };
        let (kept, fallback) =
            split_save_dir(settings.clone(), "/media/usb/downloads".into(), false);
        assert_eq!(kept.save_dir, settings.save_dir);
        assert!(fallback.is_none());
    }

    /// Value: protects=openable_roots caching against its raw inputs;
    /// fails_when=the cache ignores an input change (stale roots refuse fresh
    /// paths) or skips caching (every open re-canonicalizes the full history,
    /// the sync-command freeze A9 removes); why_new=open_path resolved every
    /// historical job's directory on every click; seam=none
    #[test]
    fn openable_roots_recompute_only_when_their_inputs_change() {
        let cache = std::sync::Mutex::new(None);
        let calls = std::cell::Cell::new(0);
        let compute = || {
            calls.set(calls.get() + 1);
            (vec![PathBuf::from("/resolved")], true)
        };
        let inputs = vec!["/logs".to_string(), "/downloads".to_string()];
        assert_eq!(roots_from_cache(&cache, inputs.clone(), compute).len(), 1);
        assert_eq!(roots_from_cache(&cache, inputs.clone(), compute).len(), 1);
        assert_eq!(calls.get(), 1, "same inputs must not re-resolve");
        let mut changed = inputs.clone();
        changed.push("/old-job-dir".to_string());
        assert_eq!(roots_from_cache(&cache, changed, compute).len(), 1);
        assert_eq!(calls.get(), 2, "changed inputs must re-resolve");
        // An incomplete resolution is mount state: never cached.
        assert_eq!(
            roots_from_cache(&cache, inputs.clone(), || {
                calls.set(calls.get() + 1);
                (vec![PathBuf::from("/partial")], false)
            })
            .len(),
            1
        );
        assert_eq!(
            roots_from_cache(&cache, inputs, || {
                calls.set(calls.get() + 1);
                (vec![PathBuf::from("/resolved")], true)
            })
            .len(),
            1
        );
        assert_eq!(
            calls.get(),
            4,
            "an uncached result must recompute next time"
        );
    }

    /// Value: protects=every command that starts or restarts a download
    /// re-materializing the cookie jar first; fails_when=any listed command's
    /// body drops its materialize call (resume/retry once launched yt-dlp as
    /// a guest after a restart while the UI showed LoggedIn); why_new=command
    /// bodies wrap a Tauri State and are not unit-testable, and the wiring
    /// already broke silently once; seam=none
    /// Value: protects=the save-dir fallback staying out of the kept settings
    /// (get_settings hands the configured dir out; save_settings strips the
    /// computed warning before the store write); fails_when=either command's
    /// body drops one of those steps (the leak path was get_settings returning
    /// the fallback, which the UI autosave then persisted); why_new=the leak
    /// lived in this wiring, which only source inspection can pin; seam=none
    #[test]
    fn settings_commands_never_leak_the_session_fallback_into_the_kept_save_dir() {
        let src = include_str!("commands.rs");
        let get_start = src.find("pub fn get_settings(").expect("get_settings");
        let save_start = src.find("pub fn save_settings(").expect("save_settings");
        let get = &src[get_start..save_start];
        assert!(
            !get.contains("settings.save_dir ="),
            "get_settings must not overwrite save_dir with the fallback"
        );
        assert!(
            get.contains("save_dir_warning = Some"),
            "get_settings must surface the fallback as a warning"
        );
        let save_end = src[save_start..]
            .find("\npub ")
            .map(|i| save_start + i)
            .unwrap_or(src.len());
        let save = &src[save_start..save_end];
        assert!(
            save.contains("save_dir_warning = None"),
            "save_settings must strip the computed warning before persisting"
        );
        assert!(
            save.contains("save_dir_fallback"),
            "save_settings must reconcile the session fallback (clear on explicit change, keep otherwise)"
        );
    }

    #[test]
    fn every_download_start_command_rematerializes_cookies() {
        let src = include_str!("commands.rs");
        for name in [
            "enqueue_download",
            "space_enqueue_batch",
            "resolve_url",
            "retry_job",
            "resume_job",
            "resume_all_jobs",
        ] {
            let start = src
                .find(&format!("\npub fn {name}("))
                .or_else(|| src.find(&format!("\npub async fn {name}(")))
                .unwrap_or_else(|| panic!("{name} not found"));
            let rest = &src[start + 1..];
            let body = match rest.find("\npub ") {
                Some(end) => &rest[..end],
                None => rest,
            };
            // Match the CALL, not the word: explanatory comments must not
            // satisfy the guard (mutation-pinning requirement).
            let call_token = if name == "resolve_url" {
                "materialize_cookies_file_from("
            } else {
                "rematerialize_cookies("
            };
            assert!(
                body.contains(call_token),
                "{name} must call {call_token}; comment text must not satisfy the guard"
            );
        }
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
