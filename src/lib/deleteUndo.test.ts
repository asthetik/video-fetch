import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// A record-only delete used to be irreversible with the sonner toast
// action sitting unused. The delete toast now offers a timed undo wired to
// the backend's session graveyard (restore_deleted_job), and only the
// record-only choice gets it. Node tests cannot mount React, so the wiring
// is pinned in source.
const QUEUE = join(import.meta.dirname, "..", "components", "DownloadQueue.tsx");
const HISTORY = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const API = join(import.meta.dirname, "..", "lib", "tauri.ts");
const queue = readFileSync(QUEUE, "utf8");
const history = readFileSync(HISTORY, "utf8");
const api = readFileSync(API, "utf8");

for (const [name, src] of [
  ["queue", queue],
  ["history", history],
] as const) {
  test(`${name}: the record-only delete toast offers a timed undo`, () => {
    assert.match(
      src,
      /if \(choice === "record"\) \{[\s\S]{0,200}?已删除任务记录/,
      "the undo toast must fire only for the record-only choice",
    );
    assert.match(
      src,
      /action: \{ label: "撤销", onClick: \(\) => void undoDelete\(job\.id\) \}/,
      "the toast action must call undoDelete with the deleted row's id",
    );
    assert.match(
      src,
      /duration: 8000/,
      "the undo window must be explicit, not sonner's 4s default",
    );
    assert.match(
      src,
      /api\.restoreDeletedJob\(id\)/,
      "undo must go through the backend graveyard, not a client-side re-insert",
    );
  });
}

test("the api surface exposes restoreDeletedJob against the graveyard command", () => {
  assert.match(
    api,
    /invoke<DownloadJob>\("restore_deleted_job", \{ id \}\)/,
    "the undo must reach the backend command that owns the stashed rows",
  );
});
