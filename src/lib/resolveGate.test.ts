import test from "node:test";
import assert from "node:assert/strict";
import { createResolveGate } from "./resolveGate.ts";

test("a failed URL stays retryable instead of becoming a dead end", () => {
  const gate = createResolveGate();
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
  gate.settle("https://b23.tv/x", false);
  // Re-pasting the same link must dispatch again: the failure must not be
  // remembered as a settled resolution.
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
});

test("an in-flight URL is not dispatched twice", () => {
  const gate = createResolveGate();
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), false);
  gate.settle("https://b23.tv/x", true);
});

test("a successfully resolved URL is not re-dispatched until cleared", () => {
  const gate = createResolveGate();
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
  gate.settle("https://b23.tv/x", true);
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), false);
  gate.clear();
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
});

test("clear() also drops an in-flight URL", () => {
  const gate = createResolveGate();
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
  gate.clear();
  assert.equal(gate.shouldDispatch("https://b23.tv/x"), true);
});

test("a stale settle never clobbers a newer dispatch", () => {
  const gate = createResolveGate();
  assert.equal(gate.shouldDispatch("url-a"), true);
  assert.equal(gate.shouldDispatch("url-b"), true); // supersedes a
  gate.settle("url-a", true); // late result of the superseded dispatch: ignored
  assert.equal(gate.shouldDispatch("url-b"), false); // b is still in flight
  gate.settle("url-b", true);
  assert.equal(gate.shouldDispatch("url-b"), false); // b's result is on screen
  // a's result was discarded when b superseded it, so a is dispatchable again.
  assert.equal(gate.shouldDispatch("url-a"), true);
});
