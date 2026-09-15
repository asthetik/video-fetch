import type { DownloadJob, JobStatus } from "../types";

/** Payload of the backend `download://progress` event. */
export interface DownloadProgressPayload {
  id: string;
  progress: number;
  status: JobStatus;
  error?: string | null;
  output_path?: string | null;
  speed?: number | null;
  eta?: number | null;
  downloaded_bytes?: number | null;
  total_bytes?: number | null;
}

/** Overlay a progress event onto a known job; live fields die on terminal status. */
export function mergeJob(
  existing: DownloadJob,
  patch: DownloadProgressPayload,
): DownloadJob {
  const terminal = patch.status === "done" || patch.status === "failed";
  return {
    ...existing,
    progress: patch.progress,
    status: patch.status,
    error: patch.error ?? existing.error,
    output_path: patch.output_path ?? existing.output_path,
    speed: terminal ? null : (patch.speed ?? existing.speed ?? null),
    eta: terminal ? null : (patch.eta ?? existing.eta ?? null),
    downloaded_bytes: terminal
      ? null
      : (patch.downloaded_bytes ?? existing.downloaded_bytes ?? null),
    total_bytes: terminal
      ? null
      : (patch.total_bytes ?? existing.total_bytes ?? null),
  };
}
