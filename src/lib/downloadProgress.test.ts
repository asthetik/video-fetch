import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { mergeJob } from "./downloadProgress.ts";
import type { DownloadJob } from "../types.ts";

function baseJob(overrides: Partial<DownloadJob> = {}): DownloadJob {
  return {
    id: "job-1",
    url: "https://b23.tv/x",
    video_id: "BV1xx",
    page_index: 1,
    format_id: "80",
    audio_format: null,
    title: "sample",
    output_template: "%(title)s.%(ext)s",
    status: "running",
    progress: 0.4,
    error: null,
    output_path: null,
    ...overrides,
  };
}

describe("mergeJob", () => {
  it("overlays progress, status and optional fields onto the existing job", () => {
    const merged = mergeJob(baseJob(), {
      id: "job-1",
      progress: 0.75,
      status: "running",
      speed: 1024,
      eta: 30,
      downloaded_bytes: 100,
      total_bytes: 400,
    });

    assert.equal(merged.progress, 0.75);
    assert.equal(merged.status, "running");
    assert.equal(merged.speed, 1024);
    assert.equal(merged.eta, 30);
    // Untouched identity fields survive the overlay.
    assert.equal(merged.title, "sample");
    assert.equal(merged.video_id, "BV1xx");
  });

  it("keeps previous values when the patch omits optional fields", () => {
    const merged = mergeJob(
      baseJob({ speed: 2048, output_path: "/tmp/a.mkv" }),
      { id: "job-1", progress: 0.5, status: "running" },
    );

    assert.equal(merged.speed, 2048);
    assert.equal(merged.output_path, "/tmp/a.mkv");
  });

  it("clears live speed/eta/bytes when the patch is terminal", () => {
    const merged = mergeJob(
      baseJob({ speed: 2048, eta: 12, downloaded_bytes: 5, total_bytes: 10 }),
      { id: "job-1", progress: 1, status: "done", output_path: "/tmp/a.mkv" },
    );

    assert.equal(merged.status, "done");
    assert.equal(merged.output_path, "/tmp/a.mkv");
    assert.equal(merged.speed, null);
    assert.equal(merged.eta, null);
    assert.equal(merged.downloaded_bytes, null);
    assert.equal(merged.total_bytes, null);
  });

  it("does not resurrect a cleared error with an explicit null patch", () => {
    const merged = mergeJob(baseJob({ error: "boom" }), {
      id: "job-1",
      progress: 0.1,
      status: "running",
      error: null,
    });

    assert.equal(merged.error, "boom");
  });

  it("ignores a late running patch on a paused job", () => {
    const paused = baseJob({ status: "paused", progress: 0.5 });
    const merged = mergeJob(paused, {
      id: "job-1",
      progress: 0.9,
      status: "running",
      speed: 999,
    });

    assert.equal(merged, paused, "the paused row is returned untouched");
    assert.equal(merged.status, "paused");
    assert.equal(merged.progress, 0.5);
    assert.equal(merged.speed, undefined, "the late patch's speed is discarded");
  });

  it("paused patch clears live fields but keeps progress", () => {
    const merged = mergeJob(
      baseJob({ speed: 2048, eta: 12, downloaded_bytes: 5, total_bytes: 10 }),
      { id: "job-1", progress: 0.62, status: "paused" },
    );

    assert.equal(merged.status, "paused");
    assert.equal(merged.progress, 0.62);
    assert.equal(merged.speed, null);
    assert.equal(merged.eta, null);
    assert.equal(merged.downloaded_bytes, null);
    assert.equal(merged.total_bytes, null);
  });

  it("ignores a stale paused patch on a finished job", () => {
    const done = baseJob({ status: "done", progress: 1 });
    const merged = mergeJob(done, {
      id: "job-1",
      progress: 0.5,
      status: "paused",
    });

    assert.equal(merged, done, "a finished row never regresses to paused");
    assert.equal(merged.status, "done");
    assert.equal(merged.progress, 1);

    const failed = baseJob({ status: "failed", error: "boom" });
    const mergedFailed = mergeJob(failed, {
      id: "job-1",
      progress: 0.5,
      status: "paused",
    });
    assert.equal(mergedFailed, failed);
    assert.equal(mergedFailed.status, "failed");
  });

  it("accepts a pending patch on a paused job (resume path)", () => {
    const merged = mergeJob(baseJob({ status: "paused", progress: 0.5 }), {
      id: "job-1",
      progress: 0.5,
      status: "pending",
    });

    assert.equal(merged.status, "pending");
  });
});
