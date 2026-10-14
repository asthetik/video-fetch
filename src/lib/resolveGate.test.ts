import test from "node:test";
import assert from "node:assert/strict";
import { createResolveGate } from "./resolveGate.ts";

test("a failed URL stays retryable instead of becoming a dead end", () => {
  const gate = createResolveGate();
  const id = gate.begin("https://b23.tv/x");
  assert.notEqual(id, null);
  gate.settle(id!, false);
  // Re-pasting the same link must dispatch again: the failure must not be
  // remembered as a settled resolution.
  assert.notEqual(gate.begin("https://b23.tv/x"), null);
});

test("an in-flight URL is not dispatched twice", () => {
  const gate = createResolveGate();
  const id = gate.begin("https://b23.tv/x");
  assert.notEqual(id, null);
  assert.equal(gate.begin("https://b23.tv/x"), null);
  gate.settle(id!, true);
});

test("a successfully resolved URL is not re-dispatched until cleared", () => {
  const gate = createResolveGate();
  const id = gate.begin("https://b23.tv/x");
  assert.notEqual(id, null);
  gate.settle(id!, true);
  assert.equal(gate.begin("https://b23.tv/x"), null);
  gate.clear();
  assert.notEqual(gate.begin("https://b23.tv/x"), null);
});

test("clear() also drops an in-flight URL", () => {
  const gate = createResolveGate();
  assert.notEqual(gate.begin("https://b23.tv/x"), null);
  gate.clear();
  assert.notEqual(gate.begin("https://b23.tv/x"), null);
});

test("a stale settle never clobbers a newer dispatch", () => {
  const gate = createResolveGate();
  const first = gate.begin("url-a");
  const b = gate.begin("url-b"); // supersedes a
  gate.settle(first!, true); // late result of the superseded dispatch: ignored
  assert.equal(gate.begin("url-b"), null); // b is still in flight
  gate.settle(b!, true);
  assert.equal(gate.begin("url-b"), null); // b's result is on screen
  // a's result was discarded when b superseded it, so a is dispatchable again.
  assert.notEqual(gate.begin("url-a"), null);
});

test("a stale settle must not clear a newer dispatch of the same URL", () => {
  const gate = createResolveGate();
  const first = gate.begin("url-a");
  assert.notEqual(first, null);
  const b = gate.begin("url-b"); // supersedes a
  gate.settle(b!, false); // b fails fast
  const retry = gate.begin("url-a");
  assert.notEqual(retry, null); // a retried
  gate.settle(first!, false); // the FIRST a's late settle — must not touch the retry
  assert.equal(
    gate.begin("url-a"),
    null,
    "the retry is still in flight; a late settle for a dead dispatch must not clear it",
  );
  gate.settle(retry!, true);
});
