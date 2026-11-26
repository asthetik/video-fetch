import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The naming preview invoked the backend once per keystroke (settings state
// updates immediately; only the persist is debounced), so a template edit
// fired an IPC round trip per character. The preview now waits out a 300ms
// quiet window and keeps the last preview visible while waiting. Node tests
// cannot mount React, so the debounce is pinned in source.
const SOURCE = join(import.meta.dirname, "..", "components", "NamingPreview.tsx");
const src = readFileSync(SOURCE, "utf8");

test("the naming preview waits out a 300ms quiet window", () => {
  assert.match(
    src,
    /const timer = setTimeout\(\(\) => \{/,
    "the invoke must sit behind a timer, not run per keystroke",
  );
  assert.match(
    src,
    /\}, 300\);/,
    "the quiet window is 300ms (same as the URL bar)",
  );
});

test("a template change cancels the pending window and the stale result", () => {
  assert.match(
    src,
    /cancelled = true;\n      clearTimeout\(timer\);/,
    "the cleanup must both flag cancellation and clear the pending timer",
  );
  assert.match(
    src,
    /if \(cancelled\) \{\n        return;\n      \}/,
    "a timer that already fired into an unmounted effect must not invoke",
  );
  assert.match(
    src,
    /if \(!cancelled\) \{\n\s+setPreview/,
    "the stale result must still be dropped if the effect unmounted mid-flight",
  );
});
