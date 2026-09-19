import type { DownloadJob, JobStatus } from "../types.ts";

export type StatusMap = Map<string, JobStatus>;

export interface LandingDiff {
  next: StatusMap;
  newlyDone: string[];
}

/**
 * Diff the current job list against the tracked statuses to find jobs that
 * just transitioned into "done". Jobs seen for the first time (e.g. a page
 * load listing finished history) never count as landings — only a live
 * non-done -> done flip does. Removed jobs are pruned from the returned map.
 */
export function diffLanding(
  prev: StatusMap,
  jobs: DownloadJob[],
): LandingDiff {
  const next: StatusMap = new Map();
  const newlyDone: string[] = [];
  for (const job of jobs) {
    next.set(job.id, job.status);
    const before = prev.get(job.id);
    if (before && before !== "done" && job.status === "done") {
      newlyDone.push(job.id);
    }
  }
  return { next, newlyDone };
}
