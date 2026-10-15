import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Source-level guard for the history page's cancel wiring (external review
// #151 Minor 2). History never lists paused rows (see historyVisibleJobs),
// so the active section must wire cancel for exactly pending/running and the
// confirm copy must not dispatch on job status — a paused branch here would
// be unreachable defensive code implying a state the page cannot show. The
// paused-only confirm copy stays in cancelConfirm.ts for the download queue.
//
// These assertions read the source text because the repo has no component
// test harness. They pin the literal gate form in renderRow: extracting the
// status check into a shared helper would need this file updated.

const HISTORY_PAGE = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");

const src = readFileSync(HISTORY_PAGE, "utf8");

test("the history page never references the paused status as a value", () => {
  // "paused" survives only as the unquoted STATUS_LABEL key and in prose
  // comments; any quoted use is a paused branch creeping back into a page
  // that cannot show paused rows.
  assert.doesNotMatch(
    src,
    /["']paused["']/,
    `HistoryPage.tsx must not branch on "paused": paused jobs live only in the download queue`,
  );
});

test("the active section filters exactly pending/running", () => {
  const start = src.indexOf("const activeJobs");
  assert.notEqual(start, -1, "activeJobs filter is missing");
  const end = src.indexOf("const finished", start);
  assert.notEqual(end, -1, "activeJobs filter boundaries not found");
  const statuses = [...src.slice(start, end).matchAll(/status === "([a-z]+)"/g)].map(
    (m) => m[1],
  );
  assert.deepEqual(
    [...statuses].sort(),
    ["pending", "running"],
    "the pinned active group must be exactly pending/running",
  );
});

test("cancel is wired for pending/running only", () => {
  const markers = [...src.matchAll(/action="cancel-job"/g)];
  assert.equal(markers.length, 1, "history rows keep a single cancel entry point");
  const at = markers[0].index;
  // The gate sits directly above the IconButton; a backward window covers the
  // JSX condition wrapping it without reaching the row's other status branches
  // (the retry button below is gated on "failed").
  const window = src.slice(at - 280, at);
  assert.match(window, /status === "pending"/, "cancel must be wired for pending");
  assert.match(window, /status === "running"/, "cancel must be wired for running");
  assert.doesNotMatch(
    window,
    /status === "(paused|done|failed)"/,
    "cancel must not be wired for statuses the active section cannot show",
  );
});

test("confirm copy is shared, not selected by job status", () => {
  assert.doesNotMatch(
    src,
    /cancelConfirmFor\([^)\n]*status/,
    "HistoryPage must not dispatch confirm copy on job status (pending/running share one copy)",
  );
  assert.match(
    src,
    /cancelConfirmFor\(\s*"(pending|running)"\s*\)/,
    "HistoryPage keeps the shared cancelConfirmFor copy at a fixed pending/running status",
  );
});
