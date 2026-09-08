import test from "node:test";
import assert from "node:assert/strict";
import { filterHistoryJobs } from "./historyFilter.ts";
import type { DownloadJob } from "../types.ts";

function job(partial: Partial<DownloadJob>): DownloadJob {
  return {
    id: "j1",
    url: "u",
    video_id: "v1",
    page_index: 1,
    format_id: "f",
    title: "晴天 MV",
    output_template: "t",
    status: "done",
    progress: 1,
    ...partial,
  };
}

test("status filter keeps only matching status", () => {
  const jobs = [
    job({ id: "a", status: "done" }),
    job({ id: "b", status: "failed" }),
  ];
  const done = filterHistoryJobs(jobs, { query: "", status: "done" });
  assert.deepEqual(done.map((j) => j.id), ["a"]);
  const all = filterHistoryJobs(jobs, { query: "", status: "all" });
  assert.equal(all.length, 2);
});

test("query matches title or output path, case-insensitive, trimmed", () => {
  const jobs = [
    job({ id: "a", title: "晴天 MV" }),
    job({ id: "b", title: "other", output_path: "/tmp/晴Music/ok.mp4" }),
    job({ id: "c", title: "无关" }),
  ];
  const hits = filterHistoryJobs(jobs, { query: "  晴 ", status: "all" });
  assert.deepEqual(hits.map((j) => j.id), ["a", "b"]);
});

test("empty query returns everything in input order", () => {
  const jobs = [job({ id: "a" }), job({ id: "b", status: "failed" })];
  assert.equal(filterHistoryJobs(jobs, { query: "", status: "all" }).length, 2);
});
