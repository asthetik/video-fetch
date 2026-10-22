import test from "node:test";
import assert from "node:assert/strict";
import type { ResolveGate } from "./resolveGate.ts";
import {
  EMPTY_RESOLVE_GATE,
  gateInputChange,
  gateResolveOutcome,
  gateSkipAction,
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

// What a skipped schedule must do about whatever is on screen. The gate skips
// only when the input returned to the last successful URL or was cleared; a
// resolve for another URL may still be in flight (its result must not land on
// the card), and a failed attempt may have cleared that URL's card (the skip
// would otherwise leave the input with nothing displayed).

test("a skipped re-paste of the displayed result does nothing", () => {
  // Focus churn: the card still shows URL_A and nothing is in flight.
  assert.equal(gateSkipAction(URL_A, null, true), "none");
});

test("returning to the remembered URL while another resolve is in flight reruns it", () => {
  // A resolved -> B resolving (card cleared) -> paste A back. Re-resolving A
  // also bumps the parent request id, discarding B's late result.
  assert.equal(gateSkipAction(URL_A, URL_B, false), "rerun");
});

test("returning to the remembered URL after another attempt cleared the card reruns it", () => {
  // The failed attempt left nothing on screen; the gate still remembers A.
  assert.equal(gateSkipAction(URL_A, null, false), "rerun");
});

test("the URL already resolving is not resolved twice", () => {
  assert.equal(gateSkipAction(URL_A, URL_A, false), "none");
});

test("clearing the input while a resolve is in flight discards it", () => {
  assert.equal(gateSkipAction("", URL_B, false), "discard");
});

test("clearing the input with nothing in flight does nothing", () => {
  assert.equal(gateSkipAction("   ", null, false), "none");
});
