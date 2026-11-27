import type { DownloadJob } from "../types";

type FetchJobs = () => Promise<DownloadJob[]>;

// The in-flight loadJobs request. Concurrent callers share it; it is freed
// on settle (success or failure) so the next call always refetches fresh
// data — this is coalescing, not a staleness cache. Each request clears the
// slot only if it is still the registered one, so a forced load is never
// clobbered by an older request settling late.
let inFlight: Promise<DownloadJob[]> | null = null;

/**
 * Merge concurrent job-list reloads into one request.
 *
 * Terminal events trigger reloads from several places at once (progress
 * handler, cross-page change bump, refresh-token effect): without this, one
 * settled download fires 3-4 identical `list_jobs` IPC calls. `fetchJobs`
 * is injected by the caller so each surface can keep its own post-processing
 * (the queue sorts, history preserves backend order).
 *
 * `force: true` bypasses the slot and always issues a fresh request — for
 * reloads that FOLLOW a mutation (delete/undo/bulk ops): coalescing could
 * attach them to a snapshot taken before the mutation, resurrecting deleted
 * rows or hiding restored ones.
 */
export function loadJobsFromStore(
  fetchJobs: FetchJobs,
  options: { force?: boolean } = {},
): Promise<DownloadJob[]> {
  if (inFlight && !options.force) {
    return inFlight;
  }
  const request = fetchJobs().then(
    (list) => {
      if (inFlight === request) {
        inFlight = null;
      }
      return list;
    },
    (err) => {
      if (inFlight === request) {
        inFlight = null;
      }
      throw err;
    },
  );
  inFlight = request;
  return request;
}

/** Test seam: clear the in-flight slot between tests. */
export function resetJobListLoaderForTests(): void {
  inFlight = null;
}
