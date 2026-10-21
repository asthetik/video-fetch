import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The settings page flushes a pending debounced save on unmount. That flush
// cannot setState, which is how its failure used to disappear: the user's last
// change was silently not persisted. A toast does not depend on React state,
// so the unmount flush must surface there. Node tests cannot mount React, so
// the call site is pinned in source.

const SETTINGS = join(import.meta.dirname, "..", "pages", "SettingsPage.tsx");
const src = readFileSync(SETTINGS, "utf8");

test("the unmount flush reports a failed save instead of swallowing it", () => {
  assert.match(
    src,
    /api\.saveSettings\(pending\)\s*\.catch\(\s*\(err\)\s*=>\s*\{[\s\S]*?toast\.error\(/,
    "the unmount flush's catch must toast the failure (it cannot setState)",
  );
});

test("the unmount flush is still attempted", () => {
  assert.match(
    src,
    /return \(\) => \{[\s\S]*?pendingSettingsRef\.current[\s\S]*?api\.saveSettings\(pending\)/,
    "unmount must still flush the pending settings write",
  );
});
