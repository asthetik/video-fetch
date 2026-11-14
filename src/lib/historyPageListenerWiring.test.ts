import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Same race as DownloadQueue: a late unlisten stored in a dead closure leaks
// the global progress listener (StrictMode doubles every mount). Pinned in
// source because Node tests cannot mount React; the behavior lives in
// listenerBag.test.ts and the HomePage precedent.
const SOURCE = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const src = readFileSync(SOURCE, "utf8");

test("the progress listener registers through the bag", () => {
  assert.match(
    src,
    /\.then\(listeners\.track\)/,
    "registration must go through the bag",
  );
  assert.match(
    src,
    /const listeners = createListenerBag\(\);/,
    "the bag must be created inside the effect",
  );
  assert.doesNotMatch(
    src,
    /unlisten = fn/,
    "a stored late unlisten leaks when cleanup already ran",
  );
});

test("the effect cleanup disposes the bag", () => {
  assert.match(
    src,
    /return \(\) => \{\s*listeners\.dispose\(\);\s*\};/,
    "cleanup must dispose the bag so late unlistens are called immediately",
  );
});
