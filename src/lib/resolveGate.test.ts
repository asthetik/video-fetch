import test from "node:test";
import assert from "node:assert/strict";
import type { ResolveGate } from "./resolveGate.ts";
import {
  EMPTY_RESOLVE_GATE,
  gateInputChange,
  gateResolveOutcome,
} from "./resolveGate.ts";

const URL_A = "https://www.bilibili.com/video/BV1xx411c7mD";
const URL_B = "https://b23.tv/other";

test("empty input never schedules a resolve", () => {
  const { schedule, gate } = gateInputChange(EMPTY_RESOLVE_GATE, "   ");
  assert.equal(schedule, false);
  assert.deepEqual(gate, EMPTY_RESOLVE_GATE);
});

test("a fresh URL schedules a resolve", () => {
  const { schedule } = gateInputChange(EMPTY_RESOLVE_GATE, URL_A);
  assert.equal(schedule, true);
});

test("a repeat of the last successful resolve is not scheduled again", () => {
  // The dedup gate exists for blur/refocus churn (e.g. focus leaving the
  // input for the quality select): a resolved URL must not re-resolve.
  let gate = EMPTY_RESOLVE_GATE;
  const first = gateInputChange(gate, URL_A);
  assert.equal(first.schedule, true);
  gate = gateResolveOutcome(first.gate, URL_A, true);
  const repeat = gateInputChange(gate, URL_A);
  assert.equal(repeat.schedule, false);
});

test("re-pasting the same URL after a failed resolve schedules a new attempt", () => {
  // A6 dead-end: paste while offline -> error -> network recovers -> re-paste
  // the same link must resolve again instead of doing absolutely nothing.
  let gate = EMPTY_RESOLVE_GATE;
  const first = gateInputChange(gate, URL_A);
  assert.equal(first.schedule, true);
  gate = gateResolveOutcome(first.gate, URL_A, false);
  const rePaste = gateInputChange(gate, URL_A);
  assert.equal(rePaste.schedule, true);
});

test("clearing the input resets the gate so the same URL can resolve again", () => {
  let gate = EMPTY_RESOLVE_GATE;
  const first = gateInputChange(gate, URL_A);
  gate = gateResolveOutcome(first.gate, URL_A, true);
  const cleared = gateInputChange(gate, "  ");
  assert.equal(cleared.schedule, false);
  assert.deepEqual(cleared.gate, EMPTY_RESOLVE_GATE);
  const again = gateInputChange(cleared.gate, URL_A);
  assert.equal(again.schedule, true);
});

test("a failed outcome never plants the failed URL in the gate", () => {
  // Only a successful resolve may be remembered. A stale memory of another
  // URL survives a failure untouched.
  const previous: ResolveGate = { lastResolved: URL_A };
  const afterFailure = gateResolveOutcome(previous, URL_B, false);
  assert.deepEqual(afterFailure, previous);
});

test("pre-fix schedule-time marking drops a re-paste after failure", () => {
  // Characterization of the pre-fix UrlBar: lastResolvedRef was written
  // inside the debounce timeout, before success/failure was known, so a
  // failure could not roll the write back and the URL dead-ended.
  const markedAtSchedule: ResolveGate = { lastResolved: URL_A };
  const afterFailure = gateResolveOutcome(markedAtSchedule, URL_A, false);
  assert.equal(
    gateInputChange(afterFailure, URL_A).schedule,
    false,
    "schedule-time marking is the root cause of the dead-end input",
  );
});
