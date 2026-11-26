import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// A record-only delete used to be irreversible with the sonner toast
// action sitting unused. The delete toast now offers a timed undo wired to
// the backend's session graveyard (restore_deleted_job) — and only for a
// terminal row: active rows are not stashed (their work dir is gone), so
// promising them an undo would be a lie. Node tests cannot mount React, so
// the wiring is pinned in source.
const QUEUE = join(import.meta.dirname, "..", "components", "DownloadQueue.tsx");
const HISTORY = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const API = join(import.meta.dirname, "..", "lib", "tauri.ts");
const queue = readFileSync(QUEUE, "utf8");
const history = readFileSync(HISTORY, "utf8");
const api = readFileSync(API, "utf8");

function assertUndoWiring(name: string, src: string) {
  assert.match(
    src,
    /action: \{ label: "撤销", onClick: \(\) => void undoDelete\(job\.id\) \}/,
    `${name}: the toast action must call undoDelete with the deleted row's id`,
  );
  assert.match(
    src,
    /duration: 8000/,
    `${name}: the undo window must be explicit, not sonner's 4s default`,
  );
  assert.match(
    src,
    /api\.restoreDeletedJob\(id\)/,
    `${name}: undo must go through the backend graveyard, not a client-side re-insert`,
  );
  assert.match(
    src,
    /"download:\/\/restored"/,
    `${name}: an undo on one surface must refresh the other via the restored event`,
  );
}

test("the queue promises undo only for a terminal record-only delete", () => {
  assert.match(
    queue,
    /if \(choice === "record" && !isActiveStatus\(job\.status\)\) \{[\s\S]{0,400}?已删除任务记录/,
    "active rows are not stashed; promising them an undo would be a false promise",
  );
  assertUndoWiring("queue", queue);
});

test("history promises undo only for a terminal record-only delete", () => {
  assert.match(
    history,
    /choice === "record" &&\n        \(job\.status === "done" \|\| job\.status === "failed"\)\n      \) \{[\s\S]{0,400}?已删除任务记录/,
    "active rows are not stashed; promising them an undo would be a false promise",
  );
  assertUndoWiring("history", history);
});

test("the api surface exposes restoreDeletedJob against the graveyard command", () => {
  assert.match(
    api,
    /invoke<DownloadJob>\("restore_deleted_job", \{ id \}\)/,
    "the undo must reach the backend command that owns the stashed rows",
  );
});
