import type { DownloadJob, JobStatus } from "../types";

const RECENT_FAILED_LIMIT = 3;
const DONE_FALLBACK_LIMIT = 8;

function rank(status: JobStatus): number {
  switch (status) {
    case "running":
      return 0;
    case "pending":
      return 1;
    case "failed":
      return 2;
    case "done":
      return 3;
  }
}

/** Stable status ordering; within the same status, keep input relative order. */
export function sortJobs(jobs: DownloadJob[]): DownloadJob[] {
  return [...jobs]
    .map((job, index) => ({ job, index }))
    .sort((a, b) => {
      const diff = rank(a.job.status) - rank(b.job.status);
      return diff !== 0 ? diff : a.index - b.index;
    })
    .map(({ job }) => job);
}

/**
 * Replace or insert `job`. If status is failed, place it at the front of the
 * failed group (immediately after active jobs) so it counts as most recent.
 */
export function upsertJob(jobs: DownloadJob[], job: DownloadJob): DownloadJob[] {
  const without = jobs.filter((j) => j.id !== job.id);
  if (job.status === "failed") {
    const active = without.filter(
      (j) => j.status === "pending" || j.status === "running",
    );
    const failed = without.filter((j) => j.status === "failed");
    const done = without.filter((j) => j.status === "done");
    return sortJobs(active).concat([job], failed, done);
  }
  return sortJobs(without.concat([job]));
}

export function partitionQueueJobs(jobs: DownloadJob[]): {
  active: DownloadJob[];
  recentFailed: DownloadJob[];
  doneFallback: DownloadJob[] | null;
} {
  const ordered = sortJobs(jobs);
  const active = ordered.filter(
    (j) => j.status === "pending" || j.status === "running",
  );
  // After sortJobs, failed keep relative order among themselves from `jobs`
  // except upsertJob already put newest failed first within that group.
  const failed = ordered.filter((j) => j.status === "failed");
  const recentFailed = failed.slice(0, RECENT_FAILED_LIMIT);

  if (active.length === 0 && failed.length === 0) {
    const done = ordered.filter((j) => j.status === "done");
    return {
      active,
      recentFailed,
      doneFallback: done.slice(0, DONE_FALLBACK_LIMIT),
    };
  }

  return { active, recentFailed, doneFallback: null };
}
