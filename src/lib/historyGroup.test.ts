import test from "node:test";
import assert from "node:assert/strict";
import { groupHistoryByDate } from "./historyGroup.ts";
import type { DownloadJob } from "../types.ts";

// No timezone suffix: parsed as local wall time, so NOW is noon on 2026-09-08
// in every timezone and hour offsets around it never cross a day boundary.
const NOW = new Date("2026-09-08T12:00:00");

/** UTC "YYYY-MM-DD HH:MM:SS" stamp at NOW + offsetMs, derived per timezone. */
function utcStamp(offsetMs: number): string {
  return new Date(NOW.getTime() + offsetMs)
    .toISOString()
    .slice(0, 19)
    .replace("T", " ");
}

const HOUR = 3_600_000;
const DAY = 24 * HOUR;

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
  // Stamps are derived from NOW so the local-day buckets hold in any timezone
  // (CI runs in UTC; hardcoded CST-assuming strings broke there).
  const jobs = [
    job("a", utcStamp(-1 * HOUR)), // 11:00 local -> 今天
    job("b", utcStamp(-3 * HOUR)), // 09:00 local -> 今天
    job("c", utcStamp(-20 * HOUR)), // 16:00 previous day -> 昨天
    job("d", utcStamp(-4 * DAY)), // -> 近 7 天
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
    job("x", utcStamp(-20 * DAY)), // 08-19 local -> 2026-08
    job("y", utcStamp(-60 * DAY)), // -> 2026-07
    job("z", utcStamp(-10 * DAY)), // 08-29 local -> 2026-08
  ];
  const groups = groupHistoryByDate(jobs, NOW);
  assert.deepEqual(
    groups.map((g) => g.label),
    ["2026-08", "2026-07"],
  );
  assert.deepEqual(groups[0].jobs.map((j) => j.id), ["x", "z"]);
});

test("month groups self-sort newest first even with oldest-first input", () => {
  const jobs = [
    job("old", utcStamp(-60 * DAY)), // 2026-07
    job("mid", utcStamp(-31 * DAY)), // 2026-08
    job("new", utcStamp(-20 * DAY)), // 2026-08
  ];
  const groups = groupHistoryByDate(jobs, NOW);
  assert.deepEqual(
    groups.map((g) => g.label),
    ["2026-08", "2026-07"],
  );
  assert.deepEqual(groups[0].jobs.map((j) => j.id), ["mid", "new"]);
});

test("missing created_at falls into a trailing earlier bucket; input order kept", () => {
  const jobs = [job("a", utcStamp(-2 * HOUR)), job("old", null)];
  const groups = groupHistoryByDate(jobs, NOW);
  assert.deepEqual(
    groups.map((g) => g.label),
    ["今天", "更早"],
  );
});

test("empty input yields no groups", () => {
  assert.deepEqual(groupHistoryByDate([], NOW), []);
});
