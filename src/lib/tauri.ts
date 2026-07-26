import { invoke } from "@tauri-apps/api/core";
import type { VideoMeta, DownloadJob, AppSettings, AuthStatus } from "../types";

export interface EnqueueArgs {
  url: string;
  video_id: string;
  title?: string;
  page_indexes: number[];
  format_id: string;
  output_template?: string | null;
  /** Save another copy with (n) suffix; never overwrite. */
  save_as_copy?: boolean;
  uploader?: string;
}

export interface CheckConflictArgs {
  video_id: string;
  page_indexes: number[];
  format_id: string;
  title?: string;
  uploader?: string;
}

export interface DownloadConflict {
  downloading: boolean;
  exists: boolean;
  file_exists: boolean;
}

export const api = {
  resolveUrl: (url: string, force = false) =>
    invoke<VideoMeta>("resolve_url", { url, force }),
  checkDownloadConflict: (args: CheckConflictArgs) =>
    invoke<DownloadConflict>("check_download_conflict", { args }),
  enqueue: (args: EnqueueArgs) => invoke<DownloadJob>("enqueue_download", { args }),
  listJobs: () => invoke<DownloadJob[]>("list_jobs"),
  cancelJob: (id: string) => invoke<DownloadJob>("cancel_job", { id }),
  retryJob: (id: string) => invoke<DownloadJob>("retry_job", { id }),
  deleteJob: (id: string, deleteFile = false) =>
    invoke<void>("delete_job", { args: { id, deleteFile } }),
  getSettings: () => invoke<AppSettings>("get_settings"),
  saveSettings: (settings: AppSettings) => invoke<void>("save_settings", { settings }),
  getAuthStatus: () => invoke<AuthStatus>("get_auth_status"),
  importCookiesPath: (path: string) =>
    invoke<AuthStatus>("import_cookies_path", { path }),
  clearAuth: () => invoke<void>("clear_auth"),
  startBilibiliLogin: () => invoke<AuthStatus>("start_bilibili_login"),
  previewName: (
    template: string,
    title: string,
    id: string,
    uploader: string,
    ext: string,
    index: number,
  ) =>
    invoke<string>("preview_name", {
      template,
      title,
      id,
      uploader,
      ext,
      index,
    }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  pickSaveDir: () => invoke<string>("pick_save_dir"),
  pickCookiesFile: () => invoke<string>("pick_cookies_file"),
};
