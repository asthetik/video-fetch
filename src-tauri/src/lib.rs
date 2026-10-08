mod activity_log;
mod auth;
mod bilibili_view;
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
    get_engine_versions, get_settings, import_cookies_path, list_jobs, list_log_files,
    log_ui_events, open_path, pause_all_jobs, pause_job, pick_cookies_file, pick_save_dir,
    preview_name, read_log_tail, resolve_url, resume_all_jobs, resume_job, retry_job,
    save_settings, set_window_theme, space_enqueue_batch, space_info, space_list_videos,
    start_bilibili_login,
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
                // PyInstaller second stage (and ffmpeg) can keep writing.
                state.downloads.kill_all_children();
                state.activity_log.flush();
            }
        });
}
