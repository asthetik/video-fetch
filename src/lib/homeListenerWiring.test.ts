import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// HomePage registers five global Tauri listeners whose unlisten resolves
// asynchronously. Node tests cannot mount React, so the wiring is pinned in
// source: every registration must go through the bag, and the cleanup must
// dispose it (see listenerBag.test.ts for the behavior).
const HOME = join(import.meta.dirname, "..", "pages", "HomePage.tsx");
const src = readFileSync(HOME, "utf8");

test("every resolve/auth listener registers through the bag", () => {
  const tracked = src.match(/\.then\(listeners\.track\)/g)?.length ?? 0;
  assert.equal(tracked, 5, "all five listeners must register through the bag");
  assert.doesNotMatch(
    src,
    /unlisteners\.push/,
    "a listener bypassing the bag leaks when it resolves after cleanup",
  );
  assert.match(
    src,
    /const listeners = createListenerBag\(\);/,
    "the bag must be created inside the effect",
  );
});

test("the effect cleanup disposes the bag", () => {
  assert.match(
    src,
    /return \(\) => \{\s*listeners\.dispose\(\);\s*\};/,
    "cleanup must dispose the bag so late unlistens are called",
  );
});
