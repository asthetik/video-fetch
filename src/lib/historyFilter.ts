import type { DownloadJob } from "../types";

export type HistoryStatusFilter = "all" | "done" | "failed";

export interface HistoryFilterOptions {
  query: string;
  status: HistoryStatusFilter;
}

/** Client-side history filtering: case-insensitive match on title or output path. */
export function filterHistoryJobs(
  jobs: DownloadJob[],
  { query, status }: HistoryFilterOptions,
): DownloadJob[] {
  const q = query.trim().toLowerCase();
  return jobs.filter((job) => {
    if (status !== "all" && job.status !== status) {
      return false;
    }
    if (!q) {
      return true;
    }
    const title = job.title.toLowerCase();
    const path = (job.output_path ?? "").toLowerCase();
    return title.includes(q) || path.includes(q);
  });
}

/**
 * The jobs the history page renders and counts: everything except paused.
 * Paused jobs live only in the download queue (an active, resumable state);
 * the in-progress pinned group shows pending/running via its own filter.
 */
export function historyVisibleJobs(jobs: DownloadJob[]): DownloadJob[] {
  return jobs.filter((job) => job.status !== "paused");
}
