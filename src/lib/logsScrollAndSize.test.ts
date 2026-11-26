import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The logs page shipped a local formatBytes capped at MB (log files grow
// past that; a 2 GB log rendered as "2048.0 MB") while the shared
// src/lib/format.ts already covers B..TB. And the log viewer had no way
// back to the bottom once you scrolled up in a live log. Node tests cannot
// mount React/DOM, so both are pinned in source + CSS.
const PAGE = join(import.meta.dirname, "..", "pages", "LogsPage.tsx");
const CSS = join(import.meta.dirname, "..", "styles", "pages.css");
const page = readFileSync(PAGE, "utf8");
const css = readFileSync(CSS, "utf8");

test("log file sizes use the shared B..TB formatBytes", () => {
  assert.doesNotMatch(
    page,
    /function formatBytes/,
    "the local MB-capped copy must go; it hides GB-scale files behind MB numbers",
  );
  assert.match(
    page,
    /import \{ formatBytes \} from "\.\.\/lib\/format";/,
    "sizes come from the shared formatter (GB/TB tiers included)",
  );
  assert.match(
    page,
    /formatBytes\(f\.size\)/,
    "the file select still renders each size",
  );
});

test("the log viewer offers a jump-to-bottom control", () => {
  assert.match(
    page,
    /data-action="logs-jump-bottom"/,
    "the viewer must render a jump-to-bottom button",
  );
  assert.match(
    page,
    /回到底部/,
    "the button labels itself 回到底部",
  );
  assert.match(
    page,
    /scrollHeight - el\.scrollTop - el\.clientHeight > SCROLL_BOTTOM_THRESHOLD_PX/,
    "visibility is derived from distance to the bottom",
  );
  assert.match(
    page,
    /\[lines, updateJumpVisibility\]/,
    "line-list changes must recompute visibility, or the button goes stale after a refresh",
  );
});

test("the jump wiring is pinned end to end", () => {
  assert.match(
    page,
    /ref=\{logViewRef\}/,
    "the scroll container must be the measured element",
  );
  assert.match(
    page,
    /onScroll=\{updateJumpVisibility\}/,
    "scrolling is what toggles the button; without the handler it never appears",
  );
  assert.match(
    page,
    /el\.scrollTop = el\.scrollHeight;/,
    "the click must actually scroll to the bottom",
  );
  assert.match(
    page,
    /new ResizeObserver\(\(\) => updateJumpVisibility\(\)\)/,
    "a resize changes clientHeight without a scroll event; the observer keeps the button honest",
  );
});

test("the jump button floats over the viewer", () => {
  assert.match(
    css,
    /\.logs-viewer \{[\s\S]{0,400}?position: relative;/,
    "the viewer is the button's positioning context",
  );
  assert.match(
    css,
    /\.logs-jump-bottom \{[\s\S]{0,200}?position: absolute;/,
    "the button floats inside the viewer instead of shifting the layout",
  );
});
