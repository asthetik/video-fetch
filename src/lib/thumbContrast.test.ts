import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// DESIGN.md: non-text contrast (icons) must stay >= 3:1. The white play
// glyph sits directly on the fallback gradient, so every stop of every
// fallback must keep white above the floor once the scrim is composited —
// in BOTH themes, since the palette vars flip.
const SOURCE = join(import.meta.dirname, "..", "components", "ThumbTile.tsx");
const src = readFileSync(SOURCE, "utf8");
const tokens = readFileSync(join(import.meta.dirname, "..", "styles", "tokens.css"), "utf8");

function themeBlock(theme: "light" | "dark"): string {
  if (theme === "light") return tokens.split("html[data-theme")[0];
  const rest = tokens.split('html[data-theme="dark"]')[1] ?? "";
  return rest;
}

function resolveVar(block: string, name: string): [number, number, number] {
  const m = block.match(new RegExp(`--${name}:\\s*#([0-9a-fA-F]{6})`));
  assert.ok(m, `--${name} missing in theme`);
  const n = m[1];
  return [
    parseInt(n.slice(0, 2), 16) / 255,
    parseInt(n.slice(2, 4), 16) / 255,
    parseInt(n.slice(4, 6), 16) / 255,
  ];
}

function luminance([r, g, b]: [number, number, number], scrim: number): number {
  const lin = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const f = (c: number) => lin(c * (1 - scrim));
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

test("every fallback stop keeps the white glyph at >= 3:1 in both themes", () => {
  const scrimMatch = src.match(/rgba\(0,\s*0,\s*0,\s*(0\.\d+)\)/);
  assert.ok(scrimMatch, "the fallback background must composite a dark scrim");
  const scrim = parseFloat(scrimMatch[1]);

  const gradients = src.match(/const FALLBACK_GRADIENTS = \[([\s\S]*?)\];/)?.[1] ?? "";
  const pairs = [...gradients.matchAll(/var\(--([a-z-]+)\), var\(--([a-z-]+)\)/g)];
  assert.ok(pairs.length >= 4, "four palette fallbacks must exist");

  for (const theme of ["light", "dark"] as const) {
    const block = themeBlock(theme);
    for (const [, a, b] of pairs) {
      for (const stop of [a, b]) {
        const l = luminance(resolveVar(block, stop), scrim);
        const contrast = 1.05 / (l + 0.05);
        assert.ok(
          contrast >= 3,
          `--${stop} (${theme}) gives white ${contrast.toFixed(2)}:1, below the 3:1 icon floor`,
        );
      }
    }
  }
});
