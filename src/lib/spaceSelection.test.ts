import test from "node:test";
import assert from "node:assert/strict";
import { selectionAfterFilterChange } from "./spaceSelection.ts";

test("keyword/order change clears residual selection", () => {
  // 10 rows checked under "最新发布", then a keyword narrows the list to 2:
  // the batch bar must not keep counting the 8 invisible rows.
  const prev = new Map(
    Array.from({ length: 10 }, (_, i) => [`BV${i}`, { bvid: `BV${i}` }]),
  );
  const next = selectionAfterFilterChange(prev);
  assert.equal(next.size, 0);
});

test("filter change drops every id, including rows still visible", () => {
  // Chosen semantics: clear the whole selection on filter change rather than
  // pruning it down to the rows that survive the new keyword/order.
  const prev = new Map([
    ["BV1", { bvid: "BV1" }],
    ["BV2", { bvid: "BV2" }],
    ["BV3", { bvid: "BV3" }],
  ]);
  const next = selectionAfterFilterChange(prev);
  assert.deepEqual([...next.keys()], []);
});

test("selectionAfterFilterChange does not mutate the previous selection", () => {
  const prev = new Map([["BV1", { bvid: "BV1" }]]);
  selectionAfterFilterChange(prev);
  assert.equal(prev.size, 1);
});

test("selectionAfterFilterChange returns a fresh empty map", () => {
  const prev = new Map([["BV1", { bvid: "BV1" }]]);
  const next = selectionAfterFilterChange(prev);
  assert.notEqual(next, prev);
  assert.equal(next.size, 0);
});
