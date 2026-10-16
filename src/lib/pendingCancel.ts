import type { DownloadJob } from "../types";
import { isActiveStatus } from "./queueJobs.ts";

/**
 * Whether a tracked pending-cancel target can still be cancelled. Live job
 * patches merge into the component's job list while the confirm dialog is
 * open; once the job reaches a terminal state (done/failed) or leaves the
 * list, cancelling is a no-op and the dialog must close instead of showing
 * stale copy for a job that is already finished.
 */
export function pendingCancelStillCancelable(
  job: DownloadJob | undefined,
): boolean {
  return job != null && isActiveStatus(job.status);
}
