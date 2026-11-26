import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Closing the Bilibili login window is the user saying "not now": no hint,
// no toast. The backend reports a voluntary close as the stable
// auth:login-cancelled sentinel (distinct from the timeout, which still
// hints), and the login catch stays silent exactly for that marker.
const HOOK = join(import.meta.dirname, "..", "hooks", "useAuthActions.ts");
const BACKEND = join(import.meta.dirname, "..", "..", "src-tauri", "src", "commands.rs");
const hook = readFileSync(HOOK, "utf8");
const backend = readFileSync(BACKEND, "utf8");

test("the login catch stays silent exactly for the cancelled sentinel", () => {
  assert.match(
    hook,
    /const LOGIN_CANCELLED = "auth:login-cancelled";/,
    "the marker must be a named constant matching the backend sentinel",
  );
  assert.match(
    hook,
    /const message = toErrorMessage\(err\);\n\s+\/\/ A voluntary close is the user saying "not now"[^]*?if \(message !== LOGIN_CANCELLED\) \{\n        setHint\(message\);/,
    "real failures must still hint; only the cancelled marker is silenced",
  );
});

test("the backend sentinel matches the frontend marker verbatim", () => {
  assert.match(
    backend,
    /const LOGIN_CANCELLED: &str = "auth:login-cancelled";/,
    "the backend sentinel is the contract",
  );
  const backendMarker = backend.match(/LOGIN_CANCELLED: &str = "([^"]+)"/)?.[1];
  const frontendMarker = hook.match(/LOGIN_CANCELLED = "([^"]+)"/)?.[1];
  assert.equal(
    backendMarker,
    frontendMarker,
    "marker drift between the two sides would un-silence the close or silence real failures",
  );
});
