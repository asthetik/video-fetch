import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Guards UrlBar's use of the skip classification. Node tests cannot drive
// React, so the decision function's tests in resolveGate.test.ts only help if
// the component actually dispatches on it: reverting the skip path to a bare
// `return` (the pre-fix behavior) must go red here.

const URL_BAR = join(import.meta.dirname, "..", "components", "UrlBar.tsx");

const src = readFileSync(URL_BAR, "utf8")
  .replace(/\/\*[\s\S]*?\*\//g, "")
  .replace(/^\s*\/\/.*$/gm, "");

test("UrlBar classifies every skipped schedule through gateSkipAction", () => {
  assert.match(
    src,
    /switch\s*\(\s*gateSkipAction\s*\(/,
    "a gate skip must branch on gateSkipAction instead of returning silently",
  );
});

test("the rerun branch resolves the remembered URL again", () => {
  assert.match(
    src,
    /case\s+"rerun"\s*:\s*runResolve\s*\(/,
    "rerun must re-resolve the remembered URL so the card matches the input",
  );
});

test("the discard branch tells the parent to drop the in-flight resolve", () => {
  assert.match(
    src,
    /case\s+"discard"\s*:[\s\S]*?onDiscard\s*\(\s*\)/,
    "discard must bump the parent request id through onDiscard",
  );
});

test("UrlBar only remembers an outcome that still matches the input", () => {
  assert.match(
    src,
    /urlRef\.current\.trim\(\)\s*!==\s*trimmed/,
    "a late outcome for a URL the input moved away from must not rewrite the gate",
  );
});

test("a superseded attempt's outcome is inert", () => {
  assert.match(
    src,
    /attempt\s*!==\s*attemptRef\.current/,
    "only the newest attempt may clear the in-flight marker or rewrite the gate",
  );
});
