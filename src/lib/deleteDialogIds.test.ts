import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The delete dialog can be mounted twice at once (queue row + cancel flow),
// so a static DOM id collides. React's useId generates a unique id per
// instance; node tests cannot mount React, so the migration is pinned in
// source.
const SOURCE = join(
  import.meta.dirname,
  "..",
  "components",
  "DeleteConfirmDialog.tsx",
);
const src = readFileSync(SOURCE, "utf8");

test("the delete dialog titles itself with a per-instance useId", () => {
  assert.match(
    src,
    /const titleId = useId\(\)/,
    "the dialog must derive its id via useId",
  );
  assert.match(
    src,
    /aria-labelledby=\{titleId\}/,
    "the dialog element must reference the generated id",
  );
  assert.doesNotMatch(
    src,
    /id="delete-dialog-title"/,
    "a static id collides when two delete dialogs mount together",
  );
});
