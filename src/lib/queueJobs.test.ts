import assert from "node:assert/strict";
import { describe, it } from "node:test";
import type { DownloadJob, JobStatus } from "../types";
import { partitionQueueJobs, sortJobs, upsertJob } from "./queueJobs.ts";

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

  it("progress on one running job does not reorder concurrent peers", () => {
    const base = [job("r1", "running"), job("r2", "running"), job("p", "pending")];
    const next = upsertJob(base, {
      ...job("r1", "running"),
      progress: 0.4,
    });
    assert.deepEqual(
      next.map((j) => j.id),
      ["r1", "r2", "p"],
    );
    assert.equal(next[0].progress, 0.4);
  });

  it("fail then promote pending puts new running on top and failed first", () => {
    let jobs = [job("r", "running"), job("p", "pending"), job("f-old", "failed")];
    jobs = upsertJob(jobs, { ...job("r", "failed"), error: "网络错误" });
    jobs = upsertJob(jobs, job("p", "running"));
    const part = partitionQueueJobs(jobs);
    assert.deepEqual(
      part.active.map((j) => j.id),
      ["p"],
    );
    assert.deepEqual(
      part.recentFailed.map((j) => j.id),
      ["r", "f-old"],
    );
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
    assert.equal(part.failedTotal, 4);
    assert.equal(part.doneFallback, null);
  });

  it("uses doneFallback when no active even if failures exist", () => {
    const jobs = [
      job("f1", "failed"),
      job("d1", "done"),
      job("d2", "done"),
    ];
    const part = partitionQueueJobs(jobs);
    assert.equal(part.active.length, 0);
    assert.equal(part.failedTotal, 1);
    assert.deepEqual(
      part.doneFallback?.map((j) => j.id),
      ["d1", "d2"],
    );
  });

  it("uses doneFallback when only done", () => {
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
