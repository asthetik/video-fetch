import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The download://progress listener's unlisten resolves asynchronously; a
// plain `unlisten = fn` store inside .then() loses it when the effect
// cleanup already ran (React.StrictMode doubles every mount), leaking a
// global listener that keeps calling loadJobs() on an unmounted component.
// Node tests cannot mount React, so the wiring is pinned in source (see
// homeListenerWiring.test.ts and listenerBag.test.ts for the behavior).
const SOURCE = join(import.meta.dirname, "..", "components", "DownloadQueue.tsx");
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
