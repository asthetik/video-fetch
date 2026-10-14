import type { JobStatus } from "../types";

export interface CancelConfirmCopy {
  title: string;
  message: string;
  confirmLabel: string;
}

/**
 * Confirm copy for cancelling a job, or null when the job is finished and has
 * nothing to cancel. Cancelling an unfinished job deletes the work directory
 * (partial download included), so every cancellable status must confirm first.
 */
export function cancelConfirmFor(status: JobStatus): CancelConfirmCopy | null {
  switch (status) {
    case "pending":
    case "running":
      return {
        title: "取消下载",
        message: "将取消该任务，已下载的部分会被删除。",
        confirmLabel: "取消任务",
      };
    case "paused":
      return {
        title: "取消暂停的任务",
        message: "将取消该暂停任务，已下载的部分会被删除。",
        confirmLabel: "取消任务",
      };
    case "done":
    case "failed":
      return null;
  }
}
