import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Guards the sliding nav indicator (motion's layoutId in App.tsx) against the
// two ways it broke while it was written:
//
// 1. The selected button painting its own coral fill again. The pill is a
//    single shared element that travels; a second fill on the button means two
//    coral backgrounds exist mid-travel and the selection reads as torn in two
//    rather than moving.
// 2. The reduced-motion opt-out silently losing its effect. `.nav-btn.active`
//    carries its own `transition` and is more specific than the bare
//    `.nav-btn` it is listed with, so naming only `.nav-btn` there leaves the
//    label colour fading for anyone who asked for reduced motion.
const SHELL_CSS = join(import.meta.dirname, "..", "styles", "shell.css");
const css = readFileSync(SHELL_CSS, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
const TOKENS_CSS = join(import.meta.dirname, "..", "styles", "tokens.css");
const tokens = readFileSync(TOKENS_CSS, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

function cssBlock(source: string, selector: string): string {
  const start = source.indexOf(selector);
  assert.notEqual(start, -1, `shell.css is missing the ${selector} block`);
  const open = source.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    if (source[i] === "{") depth++;
    else if (source[i] === "}") {
      depth--;
      if (depth === 0) return source.slice(open + 1, i);
    }
  }
  throw new Error(`unterminated ${selector} block`);
}

test(".nav-indicator carries the selected fill", () => {
  assert.match(
    cssBlock(css, ".nav-indicator"),
    /background:\s*var\(--accent\)/,
    ".nav-indicator must be the one coral fill the selection is drawn from",
  );
});

test(".nav-btn.active paints no fill of its own", () => {
  // A declaration, not the word: the transition list legitimately names
  // `background-color` as a property to animate.
  assert.doesNotMatch(
    cssBlock(css, ".nav-btn.active"),
    /(?:^|[;{])\s*background(?:-color|-image)?\s*:/m,
    ".nav-btn.active must not set a background — it would paint alongside the travelling indicator",
  );
});

test(".nav-btn is the indicator's containing block", () => {
  assert.match(
    cssBlock(css, ".nav-btn"),
    /position:\s*relative/,
    ".nav-indicator is absolutely positioned and must anchor to the button, not the nav",
  );
});

test("prefers-reduced-motion disables the selected label's own transition too", () => {
  const at = css.indexOf("@media (prefers-reduced-motion: reduce)");
  assert.notEqual(at, -1, "shell.css lost its reduced-motion block");
  const zone = css.slice(at);
  const noneAt = zone.indexOf("transition: none");
  assert.notEqual(noneAt, -1, "the reduced-motion block no longer sets transition: none");
  assert.ok(
    zone.slice(0, noneAt).includes(".nav-btn.active"),
    "the reduced-motion block lists .nav-btn but not .nav-btn.active — the more"
      + " specific rule outranks it, so the label still fades",
  );
});

test("the label timing tokens are defined", () => {
  // `.nav-btn.active` spends both tokens in its transition shorthand, and a
  // missing custom property invalidates that whole declaration: the shorthand
  // falls back to its initial value and the base `.nav-btn` transition does
  // NOT take over. The label then snaps white on click, restoring the
  // white-on-paper window this file exists to prevent — while every other
  // test here stays green.
  const root = cssBlock(tokens, ":root");
  for (const token of ["--nav-label-ms", "--nav-label-in-delay"]) {
    assert.match(
      root,
      new RegExp(`${token}\\s*:`),
      `tokens.css :root is missing ${token}, which the selected label's transition needs`,
    );
  }
});
