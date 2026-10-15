import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { JobStatus } from "../types";
import { cancelConfirmFor } from "./cancelConfirm.ts";

// Every status whose cancel deletes the partial download must confirm first;
// finished jobs have nothing to cancel and get no dialog.
const CANCELLABLE: JobStatus[] = ["pending", "running", "paused"];

describe("cancelConfirmFor", () => {
  it("confirms before cancelling every unfinished status", () => {
    for (const status of CANCELLABLE) {
      assert.ok(
        cancelConfirmFor(status),
        `cancel of ${status} must require confirmation`,
      );
    }
  });

  it("warns that the downloaded part will be deleted", () => {
    for (const status of CANCELLABLE) {
      const copy = cancelConfirmFor(status);
      assert.ok(copy);
      assert.match(copy.message, /已下载的部分会被删除/);
    }
  });

  it("keeps the existing paused-task copy", () => {
    assert.deepEqual(cancelConfirmFor("paused"), {
      title: "取消暂停的任务",
      message: "将取消该暂停任务，已下载的部分会被删除。",
      confirmLabel: "取消任务",
    });
  });

  it("uses one shared copy for pending and running", () => {
    const running = cancelConfirmFor("running");
    const pending = cancelConfirmFor("pending");
    assert.ok(running && pending);
    assert.deepEqual(pending, running);
    assert.equal(running.confirmLabel, "取消任务");
  });

  it("returns null for finished jobs", () => {
    assert.equal(cancelConfirmFor("done"), null);
    assert.equal(cancelConfirmFor("failed"), null);
  });
});
