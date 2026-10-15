import test from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { toErrorMessage, toLoadState, withToast } from "./asyncAction.ts";

// Finding A12: rejected invoke() promises were swallowed or left as
// unhandledrejection events. These tests pin the unified handling:
// a rejected load settles in a retryable error state (never the eternal
// loading copy), and a rejected action surfaces a toast instead of escaping.

test("toErrorMessage normalizes Error, string, and message-carrying values", () => {
  assert.equal(toErrorMessage(new Error("boom")), "boom");
  assert.equal(toErrorMessage("plain string"), "plain string");
  assert.equal(toErrorMessage({ message: "from-ipc" }), "from-ipc");
  assert.equal(toErrorMessage(42), "42");
});

test("toLoadState maps a successful fetch to a ready state", async () => {
  const state = await toLoadState(() => Promise.resolve({ concurrency: 3 }));
  assert.deepEqual(state, { status: "ready", data: { concurrency: 3 } });
});

test("settings load failure settles in a persistent error state, never eternal loading", async () => {
  // SettingsPage shows "加载设置中…" for the loading state and nothing else;
  // if a failure left the state at "loading" the page would spin forever.
  const state = await toLoadState(() => Promise.reject(new Error("db locked")));
  assert.notEqual(state.status, "loading");
  assert.deepEqual(state, { status: "error", message: "db locked" });
});

test("retrying a failed load can leave the error state again", async () => {
  let calls = 0;
  const fetcher = () => {
    calls += 1;
    return calls === 1
      ? Promise.reject(new Error("db locked"))
      : Promise.resolve("ok");
  };
  const first = await toLoadState(fetcher);
  assert.equal(first.status, "error");
  const second = await toLoadState(fetcher);
  assert.deepEqual(second, { status: "ready", data: "ok" });
});

test("withToast returns the value on success without notifying", async () => {
  const notified: string[] = [];
  const value = await withToast(
    () => Promise.resolve(7),
    (message) => notified.push(message),
  );
  assert.equal(value, 7);
  assert.deepEqual(notified, []);
});

test("withToast surfaces a rejected action instead of letting it become an unhandledrejection", async () => {
  const notified: string[] = [];
  const value = await withToast(
    () => Promise.reject(new Error("file moved away")),
    (message) => notified.push(message),
  );
  assert.equal(value, undefined);
  assert.deepEqual(notified, ["file moved away"]);
});

test("every api.openPath call site routes through withToast", () => {
  // Before the fix, clicking "打开文件" on a moved-away file did nothing:
  // the rejected invoke() promise had no .catch and only landed in the
  // devtools console. The wrappers at the call sites are the only thing
  // that turns those failures into a toast, so read the source and require
  // the wrapper to be present (same approach as capabilityPermissions.test.ts).
  const SRC = join(import.meta.dirname, "..");
  const files: string[] = [];
  const walk = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) {
        files.push(path);
      }
    }
  };
  walk(SRC);
  for (const file of files) {
    const source = readFileSync(file, "utf8");
    const calls = source.match(/api\.openPath\(/g)?.length ?? 0;
    const wrapped =
      source.match(/withToast\s*\(\s*\(\)\s*=>\s*api\.openPath\(/g)?.length ?? 0;
    assert.equal(
      wrapped,
      calls,
      `${file}: every api.openPath(...) must run inside withToast(...)`,
    );
  }
});

test("initial-read and retry auth-status successes fire onStatusChange via applyStatus", () => {
  // The hook's public onStatusChange callback must fire for the initial status
  // read and for a successful retryStatus, not only for auth://status events
  // and login/logout. applyStatus is the sole path that both settles ready
  // state and notifies the consumer, so both getAuthStatus read paths must
  // deliver ready data through it; loading/error results stay plain state and
  // must never masquerade as a status.
  const hook = readFileSync(
    join(import.meta.dirname, "..", "hooks", "useAuthActions.ts"),
    "utf8",
  );
  const readyRoutes = hook.match(/applyStatus\(next\.data\)/g)?.length ?? 0;
  assert.equal(
    readyRoutes,
    2,
    "initial read and retryStatus must each deliver ready data via applyStatus(next.data)",
  );
  assert.doesNotMatch(
    hook,
    /setAuthState\(await toLoadState/,
    "retryStatus must not push a raw load state past applyStatus",
  );
  assert.doesNotMatch(
    hook,
    /if \(active\) setAuthState\(next\)/,
    "the initial read must not push a raw load state past applyStatus",
  );
});

test("settings page renders a retryable error state and auth UIs never masquerade as logged out", () => {
  // Render-level pins for the same finding: the settings page must not keep
  // "加载设置中…" as its only answer to a failed load, and a failed auth-status
  // read must surface an explicit error state rather than "未登录".
  const settings = readFileSync(
    join(import.meta.dirname, "..", "pages", "SettingsPage.tsx"),
    "utf8",
  );
  assert.match(settings, /toLoadState\(/);
  assert.match(settings, /设置加载失败/);
  assert.match(settings, /重试/);
  assert.doesNotMatch(settings, /getSettings\(\)\s*\.then\(/);

  const hook = readFileSync(
    join(import.meta.dirname, "..", "hooks", "useAuthActions.ts"),
    "utf8",
  );
  assert.match(hook, /toLoadState\(/);
  assert.doesNotMatch(hook, /getAuthStatus\(\)\s*\.then\(/);

  const authStatus = readFileSync(
    join(import.meta.dirname, "..", "components", "AuthStatus.tsx"),
    "utf8",
  );
  assert.match(authStatus, /authStatusLabel\(/);

  const authChip = readFileSync(
    join(import.meta.dirname, "..", "components", "AuthChip.tsx"),
    "utf8",
  );
  assert.match(authChip, /authChipView\(/);
});
