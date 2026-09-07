import test from "node:test";
import assert from "node:assert/strict";
import { groupHistoryByDate } from "./historyGroup.ts";
import type { DownloadJob } from "../types.ts";

const NOW = new Date("2026-09-08T12:00:00");

function job(id: string, createdAt: string | null): DownloadJob {
  return {
    id,
    url: "u",
    video_id: "v",
    page_index: 1,
    format_id: "f",
    title: id,
    output_template: "t",
    status: "done",
    progress: 1,
    created_at: createdAt,
  } as DownloadJob;
}

test("groups by local date: today / yesterday / recent week", () => {
  // Local timezone is CST (UTC+8) on this machine; created_at strings are UTC.
  const jobs = [
    job("a", "2026-09-08 03:00:00"), // UTC 03:00 -> 11:00 CST, same day = 今天
    job("b", "2026-09-07 18:00:00"), // UTC 18:00 -> 09-08 02:00 CST = 今天
    job("c", "2026-09-06 20:00:00"), // UTC 20:00 -> 09-07 04:00 CST = 昨天
    job("d", "2026-09-04 00:00:00"), // -> 09-04 08:00 CST = 近 7 天
  ];
  const groups = groupHistoryByDate(jobs, NOW);
  assert.deepEqual(
    groups.map((g) => g.label),
    ["今天", "昨天", "近 7 天"],
  );
  assert.deepEqual(groups[0].jobs.map((j) => j.id), ["a", "b"]);
  assert.deepEqual(groups[1].jobs.map((j) => j.id), ["c"]);
  assert.deepEqual(groups[2].jobs.map((j) => j.id), ["d"]);
});

test("older jobs group by month, newest month first", () => {
  const jobs = [
    job("x", "2026-08-15 00:00:00"),
    job("y", "2026-07-01 00:00:00"),
    job("z", "2026-08-02 00:00:00"),
  ];
  const groups = groupHistoryByDate(jobs, NOW);
  assert.deepEqual(
    groups.map((g) => g.label),
    ["2026-08", "2026-07"],
  );
  assert.deepEqual(groups[0].jobs.map((j) => j.id), ["x", "z"]);
});

test("missing created_at falls into a trailing earlier bucket; input order kept", () => {
  const jobs = [job("a", "2026-09-08 00:00:00"), job("old", null)];
  const groups = groupHistoryByDate(jobs, NOW);
  assert.deepEqual(
    groups.map((g) => g.label),
    ["今天", "更早"],
  );
});

test("empty input yields no groups", () => {
  assert.deepEqual(groupHistoryByDate([], NOW), []);
});
