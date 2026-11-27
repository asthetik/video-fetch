import type { DownloadJob } from "../types";

type FetchJobs = () => Promise<DownloadJob[]>;

// The in-flight loadJobs request. Concurrent callers share it; it is freed
// on settle (success or failure) so the next call always refetches fresh
// data — this is coalescing, not a staleness cache.
let inFlight: Promise<DownloadJob[]> | null = null;

/**
 * Merge concurrent job-list reloads into one request.
 *
 * Terminal events trigger reloads from several places at once (progress
 * handler, cross-page change bump, refresh-token effect): without this, one
 * settled download fires 3-4 identical `list_jobs` IPC calls. `fetchJobs`
 * is injected by the caller so each surface can keep its own post-processing
 * (the queue sorts, history preserves backend order).
 */
export function loadJobsFromStore(fetchJobs: FetchJobs): Promise<DownloadJob[]> {
  if (inFlight) {
    return inFlight;
  }
  inFlight = fetchJobs().then(
    (list) => {
      inFlight = null;
      return list;
    },
    (err) => {
      inFlight = null;
      throw err;
    },
  );
  return inFlight;
}

/** Test seam: clear the in-flight slot between tests. */
export function resetJobListLoaderForTests(): void {
  inFlight = null;
}
