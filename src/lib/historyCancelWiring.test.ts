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
// status check into a shared helper would need this file updated. If pause
// ever returns to this page, revisit both the frozen-snapshot assumption
// here and isActiveStatus (it counts paused as still-cancelable).

const HISTORY_PAGE = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");

const src = readFileSync(HISTORY_PAGE, "utf8");

test("the history page never branches on the paused status", () => {
  // "paused" may appear in prose; a comparison against it or a confirm-copy
  // dispatch on it would branch for a state this page's filters strip.
  assert.doesNotMatch(src, /[!=]==?\s*["']paused["']/);
  assert.doesNotMatch(src, /["']paused["']\s*[!=]==?/);
  assert.doesNotMatch(src, /cancelConfirmFor\(\s*["']paused["']/);
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
  // Anchor on the actions container rather than a magic-length window: the
  // gate sits between it and the button, while the failed/done-only buttons
  // live after the marker and stay out of the slice.
  const start = src.lastIndexOf('<div className="history-row-actions">', at);
  assert.notEqual(start, -1, "actions container not found above the cancel button");
  const window = src.slice(start, at);
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
    /cancelConfirmFor\(\s*[^)]*status/,
    "HistoryPage must not dispatch confirm copy on job status (pending/running share one copy)",
  );
  assert.match(
    src,
    /cancelConfirmFor\(\s*"(pending|running)"\s*\)/,
    "HistoryPage keeps the shared cancelConfirmFor copy at a fixed pending/running status",
  );
});
