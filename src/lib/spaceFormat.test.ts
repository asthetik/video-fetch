import test from "node:test";
import assert from "node:assert/strict";
import { formatDate, formatDuration, formatPlayCount } from "./spaceFormat.ts";

test("formatPlayCount formats wan and yi, raw below 10000", () => {
  assert.equal(formatPlayCount(9999), "9999");
  assert.equal(formatPlayCount(10000), "1万");
  assert.equal(formatPlayCount(32000), "3.2万");
  assert.equal(formatPlayCount(120000000), "1.2亿");
});

test("formatDate renders 0 as dash and formats yyyy-mm-dd", () => {
  assert.equal(formatDate(0), "—");
  assert.match(formatDate(1700000000), /^\d{4}-\d{2}-\d{2}$/);
});

test("formatDuration renders 0 as dash, mm:ss and hh:mm:ss", () => {
  assert.equal(formatDuration(0), "—");
  assert.equal(formatDuration(312), "5:12");
  assert.equal(formatDuration(3912), "1:05:12");
});
