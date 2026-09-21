import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Guards the paper-ink palette against the defect class that reached review in
// PR #80: a semantic colour used as small text, where the decorative-grade
// mid-tone is not dark enough to clear WCAG AA. Values are read from
// tokens.css, so this fails loudly when a token drifts.
//
// White labels on solid fills are deliberately not covered: button and chip
// fills keep the brand mid-tones by product decision (see the comment above
// --accent-ink in tokens.css), so those pairs are an accepted deviation.

const TOKENS_CSS = join(import.meta.dirname, "..", "styles", "tokens.css");

function cssBlock(css: string, selector: string): string {
  const start = css.indexOf(selector);
  assert.notEqual(start, -1, `tokens.css is missing the ${selector} block`);
  const open = css.indexOf("{", start);
  let depth = 0;
  for (let i = open; i < css.length; i++) {
    if (css[i] === "{") depth++;
    else if (css[i] === "}") {
      depth--;
      if (depth === 0) return css.slice(open + 1, i);
    }
  }
  throw new Error(`unterminated ${selector} block`);
}

function readTokens(block: string): Map<string, string> {
  const tokens = new Map<string, string>();
  for (const match of block.matchAll(/--([a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    tokens.set(`--${match[1]}`, match[2].trim());
  }
  return tokens;
}

const css = readFileSync(TOKENS_CSS, "utf8");
const light = readTokens(cssBlock(css, ":root"));
const dark = new Map([...light, ...readTokens(cssBlock(css, 'html[data-theme="dark"]'))]);

function rgba(value: string): [number, number, number, number] {
  const hex = /^#([0-9a-f]{6})$/i.exec(value);
  if (hex) {
    const n = parseInt(hex[1], 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 1];
  }
  const fn = /rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+))?\)/.exec(value);
  assert.ok(fn, `unsupported colour value: ${value}`);
  return [Number(fn[1]), Number(fn[2]), Number(fn[3]), fn[4] === undefined ? 1 : Number(fn[4])];
}

function token(name: string, theme: Map<string, string>): string {
  const value = theme.get(name);
  assert.ok(value, `token ${name} is not defined in both themes`);
  return value;
}

/** Token reference, or a literal colour passed straight through. */
function resolve(spec: string, theme: Map<string, string>): string {
  return spec.startsWith("--") ? token(spec, theme) : spec;
}

function over(top: string, bottom: string, alphaOverride?: number): string {
  const [tr, tg, tb, ownAlpha] = rgba(top);
  const [br, bg, bb] = rgba(bottom);
  const ta = alphaOverride ?? ownAlpha;
  const mix = (t: number, b: number) => Math.round(t * ta + b * (1 - ta));
  return `#${[mix(tr, br), mix(tg, bg), mix(tb, bb)]
    .map((c) => c.toString(16).padStart(2, "0"))
    .join("")}`;
}

function luminance(colour: string): number {
  const channel = (c: number) => {
    const s = c / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  const [r, g, b] = rgba(colour);
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function ratio(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

// [label, foreground, background, minimum ratio]. A background of the form
// "tint|surface" is that tint composited over the surface, which is the worst
// case a chip label can land on.
const TEXT_MIN = 4.5;
const INDICATOR_MIN = 3;

const CHECKS: Array<[string, string, string, number]> = [
  // Status ink as chip labels, on each backdrop a chip can sit on.
  ["accent-ink on surface", "--accent-ink", "--surface", TEXT_MIN],
  ["accent-ink on tint/paper", "--accent-ink", "--accent-tint|--bg", TEXT_MIN],
  ["warn-ink on surface", "--warn-ink", "--surface", TEXT_MIN],
  ["warn-ink on tint/white", "--warn-ink", "--warn-tint|--surface", TEXT_MIN],
  ["danger-ink on surface", "--danger-ink", "--surface", TEXT_MIN],
  ["danger-ink on tint/paper", "--danger-ink", "--danger-tint|--bg", TEXT_MIN],
  ["success-ink on surface", "--success-ink", "--surface", TEXT_MIN],
  ["success-ink on tint/paper", "--success-ink", "--success-tint|--bg", TEXT_MIN],
  ["muted-ink on surface", "--muted-ink", "--surface", TEXT_MIN],
  ["muted-ink on neutral tint", "--muted-ink", "--muted-tint|--surface", TEXT_MIN],
  ["muted-ink on soft neutral tint", "--muted-ink", "--muted-tint-soft|--surface", TEXT_MIN],
  ["celebrate-ink on Hi-Res tag", "--celebrate-ink", "--celebrate-tint|--surface", TEXT_MIN],
  // Log badges: INFO is neutral (body text on the grey chip), WARN/ERROR are
  // louder than status chips -- a strong tint paired with a darker ink. The
  // backdrop is --ctl-bg: .log-view, not the card surface, is what sits under
  // a badge.
  ["text on neutral tint (log-badge-info)", "--text", "--muted-tint|--ctl-bg", TEXT_MIN],
  ["warn-ink-strong on ctl-bg", "--warn-ink-strong", "--ctl-bg", TEXT_MIN],
  ["warn-ink-strong on strong tint/ctl-bg", "--warn-ink-strong", "--warn-tint-strong|--ctl-bg", TEXT_MIN],
  ["danger-ink-strong on ctl-bg", "--danger-ink-strong", "--ctl-bg", TEXT_MIN],
  ["danger-ink-strong on strong tint/ctl-bg", "--danger-ink-strong", "--danger-tint-strong|--ctl-bg", TEXT_MIN],
  // Focus rings are omitted on purpose: they keep the brand pink #FB7299
  // (2.64:1 on white, 2.47:1 on paper), a product decision recorded in
  // tokens.css and CHANGELOG.md rather than a target to assert.
  // Icon buttons keep the mid-tone: as a 14px icon it only owes the 3:1
  // indicator bar, not the 4.5:1 text bar.
  ["danger icon on surface", "--danger", "--surface", INDICATOR_MIN],
];

/**
 * "surface" -> that token's colour; "tint|surface" -> the tint composited over
 * the surface, i.e. the worst backdrop a chip label can land on.
 */
function background(spec: string, theme: Map<string, string>): string {
  const [baseSpec, surfaceSpec] = spec.split("|");
  const top = resolve(baseSpec, theme);
  if (!surfaceSpec) return top;
  return over(top, token(surfaceSpec, theme));
}

for (const [themeName, theme] of [
  ["light", light],
  ["dark", dark],
] as const) {
  for (const [label, fgSpec, bgSpec, min] of CHECKS) {
    test(`${themeName}: ${label} clears ${min}:1`, () => {
      const fg = resolve(fgSpec, theme);
      const bg = background(bgSpec, theme);
      const value = ratio(fg, bg);
      assert.ok(
        value >= min,
        `${themeName}: ${label} is ${value.toFixed(2)}:1 (needs ${min}:1) — ${fg} on ${bg}`,
      );
    });
  }
}

test("every ink token is defined for both themes", () => {
  for (const name of ["--accent-ink", "--warn-ink", "--danger-ink", "--success-ink", "--muted-ink", "--celebrate-ink", "--warn-ink-strong", "--danger-ink-strong"]) {
    assert.ok(dark.has(name), `${name} has no dark-theme value`);
  }
});

// The ratio checks above guard the palette in isolation; these guard the
// migration itself. If a consumer rule gets reverted to a mid-tone (the exact
// defect class this file exists for), the declarations below stop matching
// and the suite fails loudly.
const PAGES_CSS = join(import.meta.dirname, "..", "styles", "pages.css");
const COMPONENTS_CSS = join(import.meta.dirname, "..", "styles", "components.css");
const pagesCss = readFileSync(PAGES_CSS, "utf8");
const componentsCss = readFileSync(COMPONENTS_CSS, "utf8");

/** [file css, selector, required declaration snippets]. */
const CONSUMER_RULES: Array<[string, string, string[]]> = [
  [pagesCss, ".queue-badge.pending", ["background: var(--muted-tint)", "color: var(--muted-ink)"]],
  [pagesCss, ".queue-badge.running", ["background: var(--accent-tint)", "color: var(--accent-ink)"]],
  [pagesCss, ".queue-badge.done", ["background: var(--success-tint)", "color: var(--success-ink)"]],
  [pagesCss, ".queue-badge.failed", ["background: var(--danger-tint)", "color: var(--danger-ink)"]],
  [pagesCss, ".queue-count", ["background: var(--accent-tint)", "color: var(--accent-ink)"]],
  [pagesCss, ".log-badge-info", ["background: var(--muted-tint)", "color: var(--text)"]],
  [pagesCss, ".log-badge-warn", ["background: var(--warn-tint-strong)", "color: var(--warn-ink-strong)"]],
  [pagesCss, ".log-badge-error", ["background: var(--danger-tint-strong)", "color: var(--danger-ink-strong)"]],
  [pagesCss, ".log-badge-debug", ["background: var(--muted-tint)", "color: var(--muted-ink)"]],
  [pagesCss, ".tag.hires", ["background: var(--celebrate-tint)", "color: var(--celebrate-ink)"]],
  [pagesCss, ".tag.fmt", ["background: var(--muted-tint-soft)", "color: var(--muted-ink)"]],
  [componentsCss, ".url-hint.error", ["color: var(--danger-ink)"]],
  [componentsCss, ".auth-hint", ["color: var(--danger-ink)"]],
  [componentsCss, ".auth-label.ok", ["color: var(--success-ink)"]],
  [componentsCss, ".auth-label.warn", ["color: var(--warn-ink)"]],
  [pagesCss, ".history-row-err", ["color: var(--danger-ink)"]],
];

for (const [css, selector, declarations] of CONSUMER_RULES) {
  test(`consumer rule ${selector} keeps its ink/tint tokens`, () => {
    const block = cssBlock(css, selector);
    for (const declaration of declarations) {
      assert.ok(
        block.includes(declaration),
        `${selector} no longer declares "${declaration}" — the ink migration regressed`,
      );
    }
  });
}

test("Hi-Res tag needs no dark override (tint is a themed token)", () => {
  assert.equal(
    pagesCss.indexOf('html[data-theme="dark"] .tag.hires'),
    -1,
    "the dark .tag.hires override should not exist; --celebrate-tint carries the dark value",
  );
});
