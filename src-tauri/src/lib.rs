mod auth;
mod commands;
mod cookies;
mod db;
mod download;
mod error;
mod fsutil;
mod models;
mod naming;
mod platform;
mod resolve_cache;
mod settings;
mod sidecar;
mod ytdlp;

use commands::{
    build_app_state, cancel_all_jobs, cancel_job, check_download_conflict, clear_auth,
    clear_finished_jobs, delete_job, enqueue_download, get_auth_status, get_settings,
    import_cookies_path, list_jobs, open_path, pick_cookies_file, pick_save_dir, preview_name,
    resolve_url, retry_job, save_settings, start_bilibili_login,
};
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            let state = build_app_state(app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            resolve_url,
            check_download_conflict,
            enqueue_download,
            list_jobs,
            cancel_job,
            cancel_all_jobs,
            clear_finished_jobs,
            retry_job,
            delete_job,
            get_settings,
            save_settings,
            get_auth_status,
            import_cookies_path,
            clear_auth,
            start_bilibili_login,
            preview_name,
            open_path,
            pick_save_dir,
            pick_cookies_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
