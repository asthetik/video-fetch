import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// DESIGN.md conformance pinned at the token/rule level: the panel radius
// tier (24px) exists, the CJK base line-height is 1.6, empty-state copy
// uses the UI font, and .modal lands on the panel tier.
const read = (rel: string) => readFileSync(join(import.meta.dirname, "..", rel), "utf8");
const tokens = read("styles/tokens.css");
const pages = read("styles/pages.css");
const components = read("styles/components.css");

test("token sheet defines the panel radius tier and CJK line height", () => {
  assert.match(tokens, /--radius-panel:\s*24px;/);
  assert.match(tokens, /line-height:\s*1\.6;/);
});

test("empty-state copy uses the UI font, not the display font", () => {
  const uses = pages.match(/var\(--font-display\)/g)?.length ?? 0;
  assert.equal(uses, 0, "the display font is gone; nothing may reference --font-display");
});

test("the modal dialog lands on the panel radius tier", () => {
  const modal = components.match(/^\.modal\s*\{[^}]*\}/m)?.[0] ?? "";
  assert.match(modal, /border-radius:\s*var\(--radius-panel\);/);
});

test("the display webfont is gone repo-wide", () => {
  assert.doesNotMatch(tokens, /Smiley Sans/, "DESIGN.md records the webfont as removed");
  assert.doesNotMatch(tokens, /--font-display:/);
  for (const sheet of [pages, components, read("styles/base.css"), read("styles/shell.css")]) {
    assert.doesNotMatch(sheet, /var\(--font-display\)/);
  }
});
