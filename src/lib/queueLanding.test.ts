import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { diffLanding } from "./queueLanding.ts";
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

describe("diffLanding", () => {
  it("flags a live running -> done flip as newly done", () => {
    const prev = new Map([["job-1", "running" as const]]);
    const { next, newlyDone } = diffLanding(prev, [baseJob({ status: "done" })]);
    assert.deepEqual(newlyDone, ["job-1"]);
    assert.equal(next.get("job-1"), "done");
  });

  it("flags pending -> done flips too", () => {
    const prev = new Map([["job-1", "pending" as const]]);
    const { newlyDone } = diffLanding(prev, [baseJob({ status: "done" })]);
    assert.deepEqual(newlyDone, ["job-1"]);
  });

  it("does not flag first-seen done jobs (history reload, not a landing)", () => {
    const { newlyDone } = diffLanding(new Map(), [baseJob({ status: "done" })]);
    assert.deepEqual(newlyDone, []);
  });

  it("does not re-flag jobs that stay done", () => {
    const prev = new Map([["job-1", "done" as const]]);
    const { newlyDone } = diffLanding(prev, [baseJob({ status: "done" })]);
    assert.deepEqual(newlyDone, []);
  });

  it("prunes removed jobs from the tracked map", () => {
    const prev = new Map([
      ["job-1", "done" as const],
      ["job-2", "running" as const],
    ]);
    const { next } = diffLanding(prev, [baseJob({ id: "job-1", status: "done" })]);
    assert.equal(next.size, 1);
    assert.ok(next.has("job-1"));
    assert.ok(!next.has("job-2"));
  });

  it("reports only the flipped job among many", () => {
    const prev = new Map([
      ["job-1", "running" as const],
      ["job-2", "running" as const],
      ["job-3", "done" as const],
    ]);
    const jobs = [
      baseJob({ id: "job-1", status: "running" }),
      baseJob({ id: "job-2", status: "done" }),
      baseJob({ id: "job-3", status: "done" }),
    ];
    const { newlyDone } = diffLanding(prev, jobs);
    assert.deepEqual(newlyDone, ["job-2"]);
  });
});
