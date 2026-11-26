import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// A modal is a focus boundary: while it is open, Tab must cycle through the
// panel's controls instead of escaping behind the backdrop onto the invisible
// page. Node tests cannot mount React, so the trap is pinned in source.
const SOURCE = join(import.meta.dirname, "..", "lib", "useModalFocus.ts");
const src = readFileSync(SOURCE, "utf8");

test("the modal hook traps Tab inside the open panel", () => {
  assert.match(
    src,
    /e\.key !== "Tab"/,
    "the keydown handler must branch on Tab; other keys pass through",
  );
  assert.match(
    src,
    /addEventListener\("keydown", onKeyDown\)/,
    "the Tab handler must be attached to the open panel",
  );
  assert.match(
    src,
    /removeEventListener\("keydown", onKeyDown\)/,
    "the Tab handler must be removed when the modal closes",
  );
});

test("the trap cycles through focusable controls with wrap-around", () => {
  assert.match(
    src,
    /querySelectorAll<HTMLElement>\(FOCUSABLE_SELECTOR\)/,
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
    /e\.shiftKey[\s\S]*?focusables\.length/,
    "Shift+Tab must move backward and wrap from the first control to the last",
  );
  assert.match(
    src,
    /e\.preventDefault\(\)/,
    "the trap must preventDefault so focus cannot leave the panel",
  );
});
