import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";
import type { DownloadJob, JobStatus } from "../types";
import { pendingCancelStillCancelable } from "./pendingCancel.ts";

function job(status: JobStatus): DownloadJob {
  return {
    id: "j1",
    url: "https://example.com",
    video_id: "BV1",
    page_index: 1,
    format_id: "80",
    title: "t",
    output_template: "%(title)s.%(ext)s",
    status,
    progress: status === "done" ? 1 : 0,
    error: status === "failed" ? "用户取消下载" : null,
    output_path: null,
  };
}

describe("pendingCancelStillCancelable", () => {
  it("stays open while the job is still cancelable", () => {
    for (const status of ["pending", "running", "paused"] as JobStatus[]) {
      assert.equal(
        pendingCancelStillCancelable(job(status)),
        true,
        `${status} must keep the cancel dialog open`,
      );
    }
  });

  it("closes once the job reaches a terminal state", () => {
    for (const status of ["done", "failed"] as JobStatus[]) {
      assert.equal(
        pendingCancelStillCancelable(job(status)),
        false,
        `${status} must close the cancel dialog`,
      );
    }
  });

  it("closes when the tracked job left the list", () => {
    assert.equal(pendingCancelStillCancelable(undefined), false);
  });
});

// Wiring guard: both components merge live job updates into their list while
// the confirm dialog is open, so the dialog must close via a cleanup effect
// once the tracked job leaves the cancelable statuses. These checks read the
// component source and pin the shape of that effect: look up the live job by
// id in the component's list, and clear pendingCancel when it is no longer
// cancelable. Reverting the cleanup must turn this test red.
const WIRED_COMPONENTS = [
  {
    file: join(import.meta.dirname, "..", "components", "DownloadQueue.tsx"),
    list: "jobs",
  },
  {
    file: join(import.meta.dirname, "..", "pages", "HistoryPage.tsx"),
    list: "allJobs",
  },
];

describe("pending-cancel dialog cleanup wiring", () => {
  for (const { file, list } of WIRED_COMPONENTS) {
    it(`clears pendingCancel when the live job leaves cancelable statuses in ${list}`, () => {
      const source = readFileSync(file, "utf8");
      assert.match(
        source,
        /import \{[^}]*pendingCancelStillCancelable[^}]*\} from "\.\.\/lib\/pendingCancel"/,
        `${file}: must import pendingCancelStillCancelable`,
      );
      // The cleanup effect must (1) look up the live job from the merged
      // list — not the stale pendingCancel snapshot, which never changes —
      // and (2) gate on !pendingCancelStillCancelable before clearing.
      const cleanup = new RegExp(
        "useEffect\\(\\s*\\(\\)\\s*=>\\s*\\{[\\s\\S]{0,600}?" +
          `const (\\w+) = ${list}\\.find\\(\\(j\\) => j\\.id === pendingCancel\\??\\.id\\);\\s*` +
          "if \\(!pendingCancelStillCancelable\\(\\1\\)\\) \\{\\s*" +
          "setPendingCancel\\(null\\);",
      );
      assert.match(
        source,
        cleanup,
        `${file}: must clear pendingCancel in a useEffect when the live job is no longer cancelable`,
      );
    });
  }
});
