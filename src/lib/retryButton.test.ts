import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { isRetryVisible } from "./retryButton.ts";

// The URL-bar retry button is only meaningful when there is something to
// retry. After the input is cleared the previous error may stay on screen as
// context, but offering 重试 there is a dead affordance: the click ran
// nothing (handleRetry early-returns on empty) and gave no feedback — the
// A6 dead-end in miniature. Visibility is therefore: an error is showing
// AND the input holds a non-empty URL.

test("retry is visible while an error shows and the input has a URL", () => {
  assert.equal(
    isRetryVisible("解析失败", "https://www.bilibili.com/video/BV1xx411c7mD"),
    true,
  );
});

test("retry is visible for a URL with surrounding whitespace", () => {
  assert.equal(isRetryVisible("解析失败", "  https://b23.tv/x  "), true);
});

test("retry is hidden once the input is cleared, error kept as context", () => {
  assert.equal(isRetryVisible("解析失败", ""), false);
});

test("retry is hidden for a whitespace-only input", () => {
  assert.equal(isRetryVisible("解析失败", "   "), false);
});

test("retry is hidden when no error is showing", () => {
  assert.equal(
    isRetryVisible(null, "https://www.bilibili.com/video/BV1xx411c7mD"),
    false,
  );
});

test("retry is hidden when no error and the input is empty", () => {
  assert.equal(isRetryVisible(null, ""), false);
});

// Source-level wiring guard: the pure decision above only helps if UrlBar
// actually renders the button through it. Node tests cannot click React, so
// pin the call site — reverting the JSX gate (button always inside the error
// hint) must go red here.
const URL_BAR = join(import.meta.dirname, "..", "components", "UrlBar.tsx");

test("UrlBar renders the retry button only through isRetryVisible", () => {
  const src = readFileSync(URL_BAR, "utf8");
  // The button must sit directly inside the gated span — a gate that opens
  // and closes around nothing while the button renders unconditionally
  // elsewhere must not satisfy this pin.
  assert.match(
    src,
    /\{\s*isRetryVisible\(\s*error\s*,\s*url\s*\)\s*&&\s*\(\s*<button/,
    "the 重试 button must render inside the isRetryVisible(error, url) gate",
  );
  assert.ok(
    src.includes("重试"),
    "UrlBar keeps its retry button (hidden by the gate, not removed)",
  );
});
