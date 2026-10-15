import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Guards the startup cookie-notice glue against silent drops: the backend
// records `AppState.cookie_notice` when startup cookie materialization
// failed, but the user only learns about it if App.tsx actually fetches and
// toasts it. Removing either hop leaves the login chip reading "logged in"
// while downloads fail cold — the exact bug this notice exists to prevent.
const TAURI_TS = join(import.meta.dirname, "..", "lib", "tauri.ts");
const APP_TSX = join(import.meta.dirname, "..", "App.tsx");
const tauriApi = readFileSync(TAURI_TS, "utf8");
const app = readFileSync(APP_TSX, "utf8");

test("tauri api exposes the cookie notice command", () => {
  assert.match(
    tauriApi,
    /getCookieNotice:\s*\(\)\s*=>\s*invoke<\s*string\s*\|\s*null\s*>\(\s*"get_cookie_notice"\s*\)/,
    "api.getCookieNotice must invoke the get_cookie_notice command",
  );
});

test("App toasts the cookie notice at startup", () => {
  assert.match(
    app,
    /api\s*\.getCookieNotice\(\)/,
    "App must fetch the startup cookie notice",
  );
  assert.match(
    app,
    /getCookieNotice\(\)[\s\S]*?toast\.warning\(notice/,
    "the fetched cookie notice must reach toast.warning",
  );
  assert.match(
    app,
    /toast\.warning\(notice,\s*\{\s*description:\s*"继续、重试会直接报错，不会静默以未登录状态下载"/,
    "the cookie toast must keep the description explaining that resume/retry refuse instead of degrading",
  );
});
