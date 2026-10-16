mod activity_log;
mod auth;
mod bilibili_view;
#[cfg(test)]
mod build_pins;
mod commands;
mod cookies;
mod db;
mod download;
mod error;
mod fsutil;
mod models;
mod naming;
mod platform;
mod playurl;
mod resolve_cache;
mod settings;
mod sidecar;
mod space;
mod wbi;
mod ytdlp;

use commands::{
    build_app_state, cancel_all_jobs, cancel_job, check_download_conflict, clear_auth,
    clear_finished_jobs, clear_logs, delete_job, detect_url, enqueue_download, get_auth_status,
    get_cookie_notice, get_engine_versions, get_save_dir_notice, get_settings, import_cookies_path,
    list_jobs, list_log_files, log_ui_events, open_path, pause_all_jobs, pause_job,
    pick_cookies_file, pick_save_dir, preview_name, read_log_tail, resolve_url, resume_all_jobs,
    resume_job, retry_job, save_settings, set_window_theme, space_enqueue_batch, space_info,
    space_list_videos, start_bilibili_login,
};
use tauri::Manager;

/// Test-only surface for the integration suite under `tests/`. The modules
/// themselves stay private; this list is the deliberate, minimal set of pub
/// items the harness drives. Grow it only when a test needs more.
#[cfg(feature = "test-utils")]
pub mod testing {
    pub use crate::db::Db;
    pub use crate::download::{
        DownloadManager, DownloadProgressEvent, EnqueueKind, ProgressEmitter,
        cleanup_orphan_work_dirs, work_dir_keep_ids,
    };
    pub use crate::error::{AppError, AppResult};
    pub use crate::fsutil::work_dir_for;
    pub use crate::models::{AppSettings, DownloadJob, JobStatus, PauseAllResult, ResumeAllResult};
    pub use crate::ytdlp::YtDlpConfig;
}

// Log-shape invariant (product decision 2026-09-15): a healthy startup
// produces ZERO log lines. Routine lifecycle events (start/exit, engine
// versions, timing) carry no troubleshooting value in a single-user desktop
// app and were deliberately removed — do not add them back (a startup timing
// line crept back in via PR #133 and was dropped again in #138). The only
// allowed tracing sites on this path are the 启动失败 error line in setup
// below, plus the two anomaly-gated maintenance lines in build_app_state
// (orphans / interrupted rows left by a previous session, both `> 0`
// guarded). `startup_silence_guard` fails if the call sites change without a
// deliberate decision.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // A second instance would fight over jobs.db and the log rotator;
        // focus the existing window instead.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let state =
                build_app_state(app.handle()).inspect_err(|e| {
                    tracing::error!(
                        target: "core",
                        "app: 启动失败: {}",
                        crate::activity_log::clean_log_message(&e.to_string())
                    );
                })?;
            app.manage(state);

            // The main window starts hidden and the frontend reveals it once
            // the stored theme is applied (no cold-start titlebar flash). If
            // the frontend never gets that far, reveal it anyway so a broken
            // bundle cannot leave the app invisible.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(3));
                if let Some(window) = handle.get_webview_window("main")
                    // An erroring probe counts as not visible: this thread
                    // exists so the window cannot stay hidden, so it errs
                    // toward showing it.
                    && !window.is_visible().unwrap_or(false)
                {
                    let _ = window.show();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            detect_url,
            resolve_url,
            space_list_videos,
            space_info,
            check_download_conflict,
            enqueue_download,
            space_enqueue_batch,
            list_jobs,
            cancel_job,
            cancel_all_jobs,
            clear_finished_jobs,
            clear_logs,
            retry_job,
            pause_job,
            resume_job,
            pause_all_jobs,
            resume_all_jobs,
            delete_job,
            list_log_files,
            read_log_tail,
            log_ui_events,
            get_settings,
            get_save_dir_notice,
            get_cookie_notice,
            get_engine_versions,
            save_settings,
            get_auth_status,
            import_cookies_path,
            clear_auth,
            start_bilibili_login,
            preview_name,
            open_path,
            set_window_theme,
            pick_save_dir,
            pick_cookies_file,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if matches!(event, tauri::RunEvent::Exit)
                && let Some(state) = app_handle.try_state::<commands::AppState>()
            {
                // Quitting must not leave the download tree behind: the
                // drop-time kill only reaps the direct stage, so the
                // PyInstaller second stage (and ffmpeg) can keep writing. The
                // teardown also abandons the runners, or the permit that kill
                // frees starts one more download behind it.
                state.downloads.shutdown_for_exit();
                state.activity_log.flush();
            }
        });
}

/// Tripwire for the log-shape invariant on `run` / `build_app_state`: a
/// healthy startup produces ZERO log lines (product decision 2026-09-15,
/// re-broken once by PR #133's startup timing line). A capture-based test
/// cannot pin this: `build_app_state` takes the Wry `AppHandle` that only the
/// real app constructs, so this asserts the tracing call sites in the two
/// function bodies instead. New routine lifecycle logging fails here
/// deliberately — extend the allow-list only for anomaly/error paths.
#[cfg(test)]
mod startup_silence_guard {
    /// Brace-counted body of the function declared at `signature` (a string
    /// that must appear exactly at a line start, leading `\n` included).
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
    fn healthy_startup_call_sites_stay_anomaly_only() {
        let lib = include_str!("lib.rs");
        let commands = include_str!("commands.rs");
        let run_body = fn_body(lib, "\npub fn run()");
        let state_body = fn_body(commands, "\npub fn build_app_state");

        // run(): only the startup-failure error path may log.
        let run_sites = run_body.matches("tracing::").count();
        assert_eq!(
            run_sites, 1,
            "healthy startup must produce zero log lines (product decision 2026-09-15; \
             a startup timing line already re-broke this via PR #133): `run` has \
             {run_sites} tracing call sites, only the 启动失败 error path is allowed. \
             Keep routine lifecycle logging (start/exit, engine versions, timing) out; \
             extend this test deliberately for new anomaly/error paths."
        );
        assert!(run_body.contains("tracing::error!"));
        assert!(run_body.contains("app: 启动失败"));

        // build_app_state(): exactly the two anomaly-gated maintenance lines
        // plus the anomaly-gated save_dir fallback warn. The cookie
        // materialization failure warn moved into `startup_cookie_notice`
        // (pinned below) so the same path can record the UI notice.
        let state_sites = state_body.matches("tracing::").count();
        assert_eq!(
            state_sites, 3,
            "healthy startup must produce zero log lines (product decision 2026-09-15): \
             `build_app_state` has {state_sites} tracing call sites, only the two \
             anomaly-gated maintenance lines (orphans / interrupted rows) and the \
             anomaly-gated save_dir fallback warn are allowed; the cookie materialization \
             warn is pinned on `startup_cookie_notice`. \
             Extend this test deliberately for new anomaly/error paths."
        );
        assert!(state_body.contains("tracing::info!"));
        assert!(state_body.contains("tracing::warn!"));
        assert!(state_body.contains("app: 清理孤儿工作目录"));
        assert!(state_body.contains("app: 中断的下载已转为暂停"));
        assert!(state_body.contains("app: 保存目录不可用"));
        // Both sites must stay gated on a non-empty recovery batch: on a
        // healthy startup both counters are 0 and neither line may fire.
        assert!(
            state_body.contains("if removed_orphans > 0"),
            "the orphan-cleanup line must stay anomaly-gated (> 0)"
        );
        assert!(
            state_body.contains("if reconciled > 0"),
            "the interrupted-runs line must stay anomaly-gated (> 0)"
        );
        assert!(
            state_body.contains("if save_dir_plan.effective != Path::new(&settings.save_dir)"),
            "the save_dir warning must stay anomaly-gated (fallback active)"
        );
        // The cookie materialization failure must stay anomaly-gated in
        // `startup_cookie_notice`, and startup must feed its notice into
        // AppState (dropping the set or the wiring must fail here or in the
        // unit tests on the helper).
        let cookie_body = fn_body(commands, "\nfn startup_cookie_notice");
        assert!(
            cookie_body.contains("if let Err(e) = auth.materialize_cookies_file()"),
            "the cookie warning must stay anomaly-gated (materialize failure)"
        );
        assert!(
            cookie_body.contains("app: Cookie 文件准备失败"),
            "the cookie warn line must stay in startup_cookie_notice"
        );
        assert_eq!(
            cookie_body.matches("tracing::").count(),
            1,
            "the helper may carry exactly the one anomaly-gated warning; \
             routine logging inside it would slip past the state-body count"
        );
        assert!(
            state_body.contains("let cookie_notice = startup_cookie_notice(&auth)"),
            "startup must collect the cookie materialization notice into AppState"
        );
    }
}
