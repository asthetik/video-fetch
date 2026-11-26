import test from "node:test";
import assert from "node:assert/strict";
import { formatBytes } from "./format.ts";

test("formatBytes walks the usual tiers", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1024), "1.00 KB");
  assert.equal(formatBytes(1536), "1.50 KB");
  assert.equal(formatBytes(1024 * 1024), "1.00 MB");
  assert.equal(formatBytes(3.5 * 1024 * 1024), "3.50 MB");
});

test("GB is the top tier — larger sizes accumulate in GB, never TB", () => {
  assert.equal(formatBytes(2 * 1024 ** 3), "2.00 GB");
  assert.equal(
    formatBytes(1024 ** 4),
    "1024.0 GB",
    "a terabyte-scale size must render in GB; the TB tier was dropped by decision",
  );
});
