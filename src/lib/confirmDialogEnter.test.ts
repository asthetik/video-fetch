import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Enter must confirm the dialog even when focus sits on the panel itself
// (where useModalFocus lands on open) — until now it did nothing until you
// tabbed to a button. And the dismiss button speaks one word across every
// confirm dialog: 关闭 (the #46 bulk-dialog precedent); mixed 取消/关闭 for
// the same role reads as randomness. Node tests cannot mount React, so both
// are pinned in source.
const CONFIRM = join(import.meta.dirname, "..", "components", "ConfirmDialog.tsx");
const DELETE = join(import.meta.dirname, "..", "components", "DeleteConfirmDialog.tsx");
const BATCH = join(import.meta.dirname, "..", "components", "BatchFormatDialog.tsx");
const confirm = readFileSync(CONFIRM, "utf8");
const del = readFileSync(DELETE, "utf8");
const batch = readFileSync(BATCH, "utf8");

test("Enter on the focused panel confirms the dialog", () => {
  assert.match(
    confirm,
    /e\.key === "Enter" && e\.target === e\.currentTarget && !busy/,
    "the panel must confirm on Enter only when the panel itself is the event target and the dialog is not busy",
  );
  assert.match(
    confirm,
    /e\.preventDefault\(\);\s*onConfirm\(\)/,
    "the synthetic confirm must preventDefault and call onConfirm",
  );
});

test("the Enter branch lives on the modal panel, not the backdrop", () => {
  assert.match(
    confirm,
    /tabIndex=\{-1\}[\s\S]{0,600}e\.key === "Enter"/,
    "the Enter branch must sit on the panel (reachable after its tabIndex={-1}); on the backdrop it would never match target===currentTarget and the feature would die silently",
  );
});

test("every confirm dialog dismisses with 关闭", () => {
  assert.match(
    confirm,
    /cancelLabel = "关闭"/,
    "the shared ConfirmDialog default must be 关闭",
  );
  assert.ok(
    (del.match(/\n\s+关闭\n/g) ?? []).length >= 2,
    "both delete-dialog dismiss buttons must read 关闭",
  );
  assert.doesNotMatch(
    del,
    /\n\s+取消\n/,
    "delete-dialog dismiss buttons must not revert to 取消",
  );
  assert.doesNotMatch(
    batch,
    /cancelLabel="取消"/,
    "the batch dialog inherits the unified default instead of overriding it",
  );
  assert.doesNotMatch(
    batch,
    /\n\s+取消\n/,
    "the batch dialog's own first-step dismiss button must read 关闭 too",
  );
});
