import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The confirm dialog portals to document.body, so it escapes the parent's
// hidden+inert subtree when a tab switch closes the parent (`open && active`).
// Node tests cannot mount React, so the guard is pinned in source: the
// confirm must die with the parent's `open`.
const SOURCE = join(import.meta.dirname, "..", "components", "BatchFormatDialog.tsx");
const src = readFileSync(SOURCE, "utf8");

test("the batch confirm dialog is gated by the parent's open", () => {
  assert.match(
    src,
    /open=\{open && confirmOpen\}/,
    "confirmOpen alone leaves an orphan dialog that can still submit a 50+ batch",
  );
});
