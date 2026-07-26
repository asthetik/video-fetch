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
  requires_login: boolean;
}

export interface VideoMeta {
  id: string;
  title: string;
  uploader?: string | null;
  thumbnail?: string | null;
  webpage_url: string;
  pages: PageItem[];
  formats: FormatOption[];
  platform: string;
}

export interface DownloadJob {
  id: string;
  url: string;
  video_id: string;
  page_index: number;
  format_id: string;
  title: string;
  output_template: string;
  status: JobStatus;
  progress: number;
  error?: string | null;
  output_path?: string | null;
}

export interface AppSettings {
  save_dir: string;
  default_format_preference: string;
  concurrency: number;
  filename_template: string;
  prefer_bundled_tools: boolean;
  skip_existing: boolean;
}
