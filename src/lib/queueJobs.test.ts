import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { DownloadJob, JobStatus } from "../types";
import { partitionQueueJobs, sortJobs, upsertJob } from "./queueJobs";

function job(
  id: string,
  status: JobStatus,
  page_index = 1,
): DownloadJob {
  return {
    id,
    url: "https://example.com",
    video_id: "BV1",
    page_index,
    format_id: "80",
    title: id,
    output_template: "%(title)s.%(ext)s",
    status,
    progress: status === "done" ? 1 : 0,
    error: status === "failed" ? "用户取消下载" : null,
    output_path: null,
  };
}

describe("sortJobs", () => {
  it("orders running before pending before failed before done", () => {
    const input = [
      job("d", "done"),
      job("f", "failed"),
      job("p", "pending"),
      job("r", "running"),
    ];
    assert.deepEqual(
      sortJobs(input).map((j) => j.id),
      ["r", "p", "f", "d"],
    );
  });
});

describe("upsertJob", () => {
  it("moves newly failed job to front of failed group", () => {
    const base = [
      job("r", "running"),
      job("p", "pending"),
      job("f-old", "failed"),
    ];
    const next = upsertJob(base, { ...job("r", "failed"), error: "用户取消下载" });
    assert.deepEqual(
      next.map((j) => `${j.id}:${j.status}`),
      ["p:pending", "r:failed", "f-old:failed"],
    );
  });

  it("retry pending leaves failed group", () => {
    const base = [job("f", "failed"), job("r", "running")];
    const next = upsertJob(base, job("f", "pending"));
    const part = partitionQueueJobs(next);
    assert.deepEqual(
      part.active.map((j) => j.id),
      ["r", "f"],
    );
    assert.equal(part.recentFailed.length, 0);
  });
});

describe("partitionQueueJobs", () => {
  it("caps recentFailed at 3 preserving order", () => {
    const jobs = [
      job("r", "running"),
      job("f1", "failed"),
      job("f2", "failed"),
      job("f3", "failed"),
      job("f4", "failed"),
    ];
    const part = partitionQueueJobs(jobs);
    assert.deepEqual(
      part.recentFailed.map((j) => j.id),
      ["f1", "f2", "f3"],
    );
    assert.equal(part.doneFallback, null);
  });

  it("uses doneFallback only when no active and no failed", () => {
    const jobs = [job("d1", "done"), job("d2", "done")];
    const part = partitionQueueJobs(jobs);
    assert.equal(part.active.length, 0);
    assert.equal(part.recentFailed.length, 0);
    assert.deepEqual(
      part.doneFallback?.map((j) => j.id),
      ["d1", "d2"],
    );
  });
});
