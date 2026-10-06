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

/** Overlay a progress event onto a known job; live fields die on terminal or paused status. */
export function mergeJob(
  existing: DownloadJob,
  patch: DownloadProgressPayload,
): DownloadJob {
  // A late `running` patch must not flip a paused row back to "downloading":
  // after a pause the backend stops emitting, but in-flight events can still
  // land. The reverse guard matters equally: a stale `paused` patch delivered
  // after the row's done/failed event would strand it with a resume button
  // that can only error (the backend guarantees terminal rows never resume).
  if (existing.status === "paused" && patch.status === "running") {
    return existing;
  }
  if (
    (existing.status === "done" || existing.status === "failed") &&
    patch.status === "paused"
  ) {
    return existing;
  }
  const terminal = patch.status === "done" || patch.status === "failed";
  const paused = patch.status === "paused";
  const dropLive = terminal || paused;
  return {
    ...existing,
    progress: patch.progress,
    status: patch.status,
    error: patch.error ?? existing.error,
    output_path: patch.output_path ?? existing.output_path,
    speed: dropLive ? null : (patch.speed ?? existing.speed ?? null),
    eta: dropLive ? null : (patch.eta ?? existing.eta ?? null),
    downloaded_bytes: dropLive
      ? null
      : (patch.downloaded_bytes ?? existing.downloaded_bytes ?? null),
    total_bytes: dropLive
      ? null
      : (patch.total_bytes ?? existing.total_bytes ?? null),
  };
}
