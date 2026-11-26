import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// A modal is a focus boundary while it is open: Tab must cycle through the
// panel's controls instead of escaping behind the backdrop onto the invisible
// page. The trap must also survive the busy window — disabling the focused
// button drops focus onto body, and a body-targeted Tab never reaches a
// listener attached to the panel, so the trap has to ride on document.
// Node tests cannot mount React, so the trap is pinned in source.
const SOURCE = join(import.meta.dirname, "..", "lib", "useModalFocus.ts");
const src = readFileSync(SOURCE, "utf8");

test("the modal hook traps Tab while the panel is open", () => {
  assert.match(
    src,
    /e\.key !== "Tab"/,
    "the keydown handler must branch on Tab; other keys pass through",
  );
  assert.match(
    src,
    /document\.addEventListener\("keydown", onKeyDown\)/,
    "the trap must listen on document: a busy dialog drops focus to body and a body-targeted Tab never reaches a panel listener",
  );
  assert.match(
    src,
    /document\.removeEventListener\("keydown", onKeyDown\)/,
    "the Tab handler must be removed when the modal closes",
  );
});

test("the trap cycles through focusable controls with wrap-around", () => {
  assert.match(
    src,
    /querySelectorAll<HTMLElement>\(FOCUSABLE_SELECTOR\),/,
    "focusables must be collected from the panel via a selector",
  );
  assert.match(
    src,
    /button:not\(\[disabled\]\)/,
    "the focusable selector must include disabled-filtered buttons",
  );
  assert.match(
    src,
    /\(index \+ 1\) % focusables\.length/,
    "forward Tab must wrap from the last control back to the first",
  );
  assert.match(
    src,
    /\(index <= 0 \? focusables\.length : index\) - 1/,
    "Shift+Tab must resolve the predecessor with the explicit -1-index fallback; a bare modulo walk yields undefined at the first control",
  );
});

test("every focus move is paired with preventDefault", () => {
  assert.match(
    src,
    /next\.focus\(\);\s*e\.preventDefault\(\)/,
    "preventDefault must follow next.focus() directly; one elsewhere leaves the move half-applied and focus free to double-jump",
  );
  assert.match(
    src,
    /focusables\.length === 0[\s\S]{0,200}?panel\.focus\(\);/,
    "a fully busy modal (every control disabled) must pin focus on the panel itself",
  );
});

test("focus outside the open panel re-enters the cycle", () => {
  assert.match(
    src,
    /panel\.contains\(current\)/,
    "focus sitting on body (dropped by a disabling busy button) must count as index -1 and be pulled back in",
  );
});
