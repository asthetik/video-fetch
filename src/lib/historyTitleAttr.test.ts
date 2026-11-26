import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// The history row title is clamped to a single ellipsized line, so long
// titles are unreadable without a native tooltip. Node tests cannot mount
// React, so the title attribute is pinned in source.
const SOURCE = join(import.meta.dirname, "..", "pages", "HistoryPage.tsx");
const src = readFileSync(SOURCE, "utf8");

test("the ellipsized history row title exposes itself via a title tooltip", () => {
  assert.match(
    src,
    /className="history-row-title" title=\{jobLabel\(job\)\}/,
    "the row title must carry title={jobLabel(job)}; ellipsis alone hides the full text",
  );
});
