export type JobStatus = "pending" | "running" | "done" | "failed";
export type AuthStatus = "logged_out" | "logged_in" | "possibly_expired";

export interface PageItem {
  index: number;
  title: string;
  page_id: string;
}

export interface FormatOption {
  format_id: string;
  label: string;
  height?: number | null;
  fps?: number | null;
  /** Approximate total bitrate (kbps); used for sort/default. */
  tbr?: number | null;
}

export interface VideoMeta {
  id: string;
  title: string;
  uploader?: string | null;
  thumbnail?: string | null;
  webpage_url: string;
  pages: PageItem[];
  formats: FormatOption[];
  audio_formats: FormatOption[];
  platform: string;
}

/**
 * Job record from `list_jobs` / enqueue / cancel / retry.
 * `speed` / `eta` / byte fields are NOT persisted by the backend — they only
 * appear via `download://progress` events and are cleared on reload/done/failed.
 */
export interface DownloadJob {
  id: string;
  url: string;
  video_id: string;
  page_index: number;
  format_id: string;
  audio_format?: string | null;
  title: string;
  output_template: string;
  status: JobStatus;
  progress: number;
  error?: string | null;
  output_path?: string | null;
  /** Bytes/sec; live from progress events only. */
  speed?: number | null;
  /** Seconds remaining; live from progress events only. */
  eta?: number | null;
  downloaded_bytes?: number | null;
  total_bytes?: number | null;
}

export interface AppSettings {
  save_dir: string;
  concurrency: number;
  filename_template: string;
  skip_existing: boolean;
}

export interface CancelAllResult {
  cancelled: number;
  errors?: string[];
}

export interface ClearFinishedResult {
  cleared: number;
}

export interface LogFileInfo {
  name: string;
  size: number;
  modified_secs: number;
}
