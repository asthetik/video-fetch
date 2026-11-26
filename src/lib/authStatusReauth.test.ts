import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// A stale (possibly_expired) session used to offer only 登出 on the
// settings row: re-login required logging out first, which throws away the
// cookies before the user has proved the fresh login works. The row now
// carries both — 重新登录 and 登出. Node tests cannot mount React, so the
// wiring is pinned in source.
const SOURCE = join(import.meta.dirname, "..", "components", "AuthStatus.tsx");
const src = readFileSync(SOURCE, "utf8");

test("the settings row offers re-login alongside logout when stale", () => {
  assert.match(
    src,
    /authState\.data === "possibly_expired" \? \(/,
    "the stale state needs its own branch between logged_out and the logout fallback",
  );
  const branch = src.split('authState.data === "possibly_expired" ? (')[1] ?? "";
  assert.match(
    branch,
    /重新登录/,
    "the stale branch must offer a re-login button",
  );
  assert.match(
    branch,
    /requestLogout/,
    "the stale branch must keep logout reachable without going through the chip",
  );
});
