import test from "node:test";
import assert from "node:assert/strict";
import { createListenerBag } from "./listenerBag.ts";

test("dispose calls every unlisten tracked before it", () => {
  const bag = createListenerBag();
  const calls: string[] = [];
  bag.track(() => calls.push("a"));
  bag.track(() => calls.push("b"));
  bag.dispose();
  assert.deepEqual(calls, ["a", "b"]);
});

// The leak this bag exists for: `listen()` resolves asynchronously, and its
// unlisten can arrive after the effect cleanup ran. Pushing it into a plain
// array would leave a live global listener behind; the bag must call it.
test("an unlisten that resolves after dispose is called immediately", () => {
  const bag = createListenerBag();
  const calls: string[] = [];
  bag.dispose();
  bag.track(() => calls.push("late"));
  assert.deepEqual(calls, ["late"]);
});

test("dispose is idempotent and never double-calls an unlisten", () => {
  const bag = createListenerBag();
  let calls = 0;
  bag.track(() => calls++);
  bag.dispose();
  bag.dispose();
  assert.equal(calls, 1);
});
