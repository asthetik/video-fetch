import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Guards UrlBar's styling against the defect class that reached review on
// PR #154: presentation written as inline `style=` in the component instead
// of a stylesheet class. Inline styles bypass src/styles/ and the DESIGN.md
// token discipline the CSS there follows, so the component source is kept
// free of them, and the hoisted spacing is pinned to the exact value the
// inline style carried so the rendered result stays pixel-identical.

const URL_BAR = join(import.meta.dirname, "..", "components", "UrlBar.tsx");
const COMPONENTS_CSS = join(import.meta.dirname, "..", "styles", "components.css");

// Comments are dropped first so prose about `style=` never trips the guard;
// only real JSX attributes may fail it.
const tsx = readFileSync(URL_BAR, "utf8")
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/^\s*\/\/.*$/gm, "");

test("UrlBar renders no inline style attributes", () => {
  assert.ok(
    !/\bstyle\s*=/.test(tsx),
    "UrlBar.tsx grew a style= attribute — hoist it into a class in src/styles/ instead",
  );
});

const css = readFileSync(COMPONENTS_CSS, "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
const hintAction = /\.hint-action\s*\{([^}]*)\}/.exec(css);

test("hint-action keeps the hoisted hint-row spacing", () => {
  assert.ok(hintAction, ".hint-action rule missing from components.css");
  assert.ok(
    /margin-left:\s*0\.5rem\b/.test(hintAction[1]),
    ".hint-action no longer declares margin-left: 0.5rem — the inline style it " +
      "replaced used that value (DESIGN.md spacing.sm); changing it silently " +
      "shifts the error-row retry button",
  );
});
