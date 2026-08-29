import { invoke } from "@tauri-apps/api/core";
import type {
  VideoMeta,
  DownloadJob,
  AppSettings,
  AuthStatus,
  CancelAllResult,
  ClearFinishedResult,
  LogFileInfo,
  UrlKind,
  SpacePage,
  SpaceInfo,
  BatchEnqueueResult,
} from "../types";

export interface EnqueueArgs {
  url: string;
  video_id: string;
  title?: string;
  page_indexes: number[];
  format_id: string;
  /** "m4a" | "mp3" | "flac" for audio-only jobs; omit/null for video. */
  audio_format?: string | null;
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
  audio_format?: string | null;
}

export interface DownloadConflict {
  downloading: boolean;
  exists: boolean;
  file_exists: boolean;
}

export interface ResolveMetaEvent {
  request_id: number;
  meta: VideoMeta;
}

export interface ResolveFormatsFailedEvent {
  request_id: number;
  error: string;
}

export const api = {
  resolveUrl: (url: string, force = false, requestId?: number) =>
    invoke<VideoMeta>("resolve_url", {
      url,
      force,
      requestId: requestId ?? null,
    }),
  checkDownloadConflict: (args: CheckConflictArgs) =>
    invoke<DownloadConflict>("check_download_conflict", { args }),
  enqueue: (args: EnqueueArgs) => invoke<DownloadJob>("enqueue_download", { args }),
  listJobs: () => invoke<DownloadJob[]>("list_jobs"),
  cancelJob: (id: string) => invoke<DownloadJob>("cancel_job", { id }),
  cancelAllJobs: () => invoke<CancelAllResult>("cancel_all_jobs"),
  clearFinishedJobs: () => invoke<ClearFinishedResult>("clear_finished_jobs"),
  listLogFiles: () => invoke<[string, LogFileInfo[]]>("list_log_files"),
  readLogTail: (name: string) => invoke<string[]>("read_log_tail", { name }),
  clearLogs: () => invoke<number>("clear_logs"),
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
  detectUrl: (url: string) => invoke<UrlKind>("detect_url", { url }),
  spaceInfo: (mid: number) => invoke<SpaceInfo>("space_info", { mid }),
  spaceListVideos: (mid: number, pn: number, keyword: string, order: string) =>
    invoke<SpacePage>("space_list_videos", { mid, pn, keyword, order }),
  spaceEnqueueBatch: (args: {
    items: { bvid: string; title: string }[];
    format_id: string;
    audio_format?: string | null;
  }) => invoke<BatchEnqueueResult>("space_enqueue_batch", { args }),
};
