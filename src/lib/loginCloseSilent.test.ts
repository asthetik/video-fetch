import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";

// Closing the Bilibili login window is the user saying "not now": no hint,
// no toast. The backend reports a voluntary close as the structured
// cancelled kind (B20's error envelope; distinct from the timeout, which
// still hints), and the login catch stays silent exactly for that kind.
const HOOK = join(import.meta.dirname, "..", "hooks", "useAuthActions.ts");
const BACKEND = join(
  import.meta.dirname,
  "..",
  "..",
  "src-tauri",
  "src",
  "commands.rs",
);
const hook = readFileSync(HOOK, "utf8");
const backend = readFileSync(BACKEND, "utf8");

test("the login catch stays silent exactly for the cancelled kind", () => {
  assert.match(
    hook,
    /const \{ message, kind \} = toApiError\(err\);\n\s+\/\/ A voluntary close is the user saying "not now"[^]*?if \(kind !== "cancelled"\) \{\n        setHint\(message\);/,
    "real failures must still hint; only the cancelled kind is silenced",
  );
});

test("the backend marks the voluntary close with the cancelled kind", () => {
  assert.match(
    backend,
    /PollOutcome::Cancelled => Err\(crate::error::AppError::with_kind\(\n\s+crate::error::ErrorKind::Cancelled,\n\s+"登录已取消",\n\s+\)\),/,
    "the cancelled kind is the contract; the login window close must carry it",
  );
});
