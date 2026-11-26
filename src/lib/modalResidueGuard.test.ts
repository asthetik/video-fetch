import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The queue's dialogs portal to document.body, so they escape the home
// subtree's hidden+inert wrapper when a tab switch deactivates the page —
// the dialog would keep floating over other pages. Like SpaceListView, the
// queue must gate every dialog on `active` and clear pending dialog state
// while inactive. Node tests cannot mount React, so the guard is pinned in
// source.
const QUEUE = join(
  import.meta.dirname,
  "..",
  "components",
  "DownloadQueue.tsx",
);
const HOME = join(import.meta.dirname, "..", "pages", "HomePage.tsx");
const queue = readFileSync(QUEUE, "utf8");
const home = readFileSync(HOME, "utf8");

test("every queue dialog is gated on the page's active flag", () => {
  assert.match(
    queue,
    /active: boolean/,
    "DownloadQueue must receive the home page's active flag",
  );
  assert.match(
    queue,
    /open=\{confirmCancelAll && active\}/,
    "cancel-all dialog must hide on a tab switch",
  );
  assert.match(
    queue,
    /open=\{pendingCancel !== null && cancelCopy !== null && active\}/,
    "cancel dialog must hide on a tab switch",
  );
  assert.match(
    queue,
    /open=\{pendingDelete !== null && active\}/,
    "delete dialog must hide on a tab switch",
  );
});

test("deactivating the queue clears pending dialog state", () => {
  assert.match(
    queue,
    /if \(!active\) \{[\s\S]*?setPendingDelete\(null\);[\s\S]*?setPendingCancel\(null\);[\s\S]*?setConfirmCancelAll\(false\);/,
    "returning to the page must not silently re-open a dialog left behind",
  );
});

test("the home page passes its active flag down to the queue", () => {
  assert.match(
    home,
    /<DownloadQueue[^>]*active=\{active\}/,
    "the home page owns `active`; the queue cannot see a tab switch without it",
  );
});
