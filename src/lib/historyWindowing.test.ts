import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The history page rendered every finished row at once — thousands of rows
// meant thousands of DOM nodes on one filter keystroke. The list now renders
// in bounded batches with a 显示更多 expander, the window resets when the
// filter scope changes, and the toolbar sticks to the viewport so the filters
// stay reachable deep into the list. Node tests cannot mount React, so the
// wiring is pinned in source.
const PAGE = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const CSS = join(import.meta.dirname, "..", "styles", "pages.css");
const page = readFileSync(PAGE, "utf8");
const css = readFileSync(CSS, "utf8");

test("the finished list renders in bounded batches", () => {
  assert.match(
    page,
    /const HISTORY_RENDER_BATCH = 100;/,
    "the first batch is 100 rows",
  );
  assert.match(
    page,
    /const shownFinished = useMemo\(\s*\(\) =>\s*finished\.slice\(0, renderLimit\)/,
    "the window is a slice of the full filtered list",
  );
  assert.match(
    page,
    /groupHistoryByDate\(shownFinished\)/,
    "grouping must consume the windowed slice, not the full filtered list",
  );
  assert.match(
    page,
    /data-action="history-load-more"/,
    "a 显示更多 control must expose the rest of the list",
  );
  assert.match(
    page,
    /setRenderLimit\(\(limit\) => limit \+ 200\)/,
    "the expander widens the window by 200 rows per click",
  );
});

test("changing the filter scope resets the window during render", () => {
  assert.match(
    page,
    /const prevScopeRef = useRef\(filterScope\);\n  if \(prevScopeRef\.current !== filterScope\) \{\n    prevScopeRef\.current = filterScope;\n    setRenderLimit\(HISTORY_RENDER_BATCH\);\n  \}/,
    "the reset must happen during render (prev-value compare): a late effect would paint the full list for a frame after the window was widened",
  );
});

test("the history toolbar sticks to the viewport", () => {
  assert.match(
    css,
    /\.history-toolbar \{[\s\S]{0,400}?top: var\(--header-h\);/,
    "the toolbar must pin below the app header, not under it",
  );
  assert.match(
    css,
    /\.history-toolbar \{[\s\S]{0,400}?background: var\(--surface\);/,
    "an opaque surface keeps rows from showing through the pinned bar",
  );
});
