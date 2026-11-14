import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// DESIGN.md bans purple gradients outright and every fallback colour must
// come from the palette. Pin the fallback array to palette variables only
// (CSS vars also carry the dark theme for free).
const SOURCE = join(import.meta.dirname, "..", "components", "ThumbTile.tsx");
const src = readFileSync(SOURCE, "utf8");
const block = src.match(/const FALLBACK_GRADIENTS = \[([\s\S]*?)\];/)?.[1] ?? "";

test("thumbnail fallback gradients use only palette variables", () => {
  const entries =
    [...block.matchAll(/linear-gradient\(135deg, var\(--[a-z-]+\), var\(--[a-z-]+\)\)/g)]
      .length;
  assert.equal(entries, 4, "four palette-only fallbacks must exist");
  const anyGradient = block.match(/linear-gradient\(/g)?.length ?? 0;
  assert.equal(anyGradient, 4, "no gradient may fall outside the palette shape");
  assert.doesNotMatch(
    block,
    /#[0-9a-fA-F]{3,8}/,
    "hard-coded hex is off-palette by definition",
  );
});
