import test from "node:test";
import assert from "node:assert/strict";
import { parseLogLine } from "./activityLog.ts";

test("parseLogLine splits timestamp, level and message", () => {
  const parts = parseLogLine("2026-08-15T12:00:01.123+08:00 INFO core:resolve 成功 BV1xx");
  assert.ok(parts);
  assert.equal(parts.timestamp, "2026-08-15T12:00:01.123+08:00");
  assert.equal(parts.level, "INFO");
  assert.equal(parts.message, "core:resolve 成功 BV1xx");
});

test("parseLogLine handles padded levels", () => {
  const parts = parseLogLine("2026-08-15T12:00:01.123+08:00 WARN   ui:click x");
  assert.ok(parts);
  assert.equal(parts.level, "WARN");
  assert.equal(parts.message, "ui:click x");
});

test("parseLogLine returns null for non-log lines", () => {
  assert.equal(parseLogLine(""), null);
  assert.equal(parseLogLine("some random text"), null);
});
