import test from "node:test";
import assert from "node:assert/strict";
import {
  createActivityLogger,
  type UiLogEvent,
} from "./activityLog.ts";

test("logUi defaults to error level (whitelist-safe default)", async () => {
  const seen: UiLogEvent[] = [];
  const logger = createActivityLogger({
    flushFn: async (events) => {
      seen.push(...events);
    },
  });
  logger.logUi("cat", "no level passed");
  await logger.flushNow();
  assert.equal(seen.length, 1);
  assert.equal(seen[0].level, "error");
});

test("flushes when batch size is reached", async () => {
  const seen: UiLogEvent[] = [];
  const logger = createActivityLogger({
    flushFn: async (events) => {
      seen.push(...events);
    },
  });
  for (let i = 0; i < 20; i++) {
    logger.logUi("click", `m${i}`, "debug");
  }
  await logger.flushNow();
  assert.equal(seen.length, 20);
});

test("drops events beyond the queue cap while a flush is pending", async () => {
  const seen: UiLogEvent[] = [];
  const resolvers: Array<() => void> = [];
  const logger = createActivityLogger({
    flushFn: (events) =>
      new Promise<void>((resolve) => {
        seen.push(...events);
        resolvers.push(resolve);
      }),
  });
  for (let i = 0; i < 500; i++) {
    logger.logUi("click", `m${i}`, "debug");
  }
  while (resolvers.length) {
    resolvers.shift()!();
  }
  // Let the in-flight flush finish (it clears `flushing` in its finally).
  await Promise.resolve();
  const pending = logger.flushNow();
  while (resolvers.length) {
    resolvers.shift()!();
  }
  await pending;
  assert.equal(seen.length, 220);
  assert.equal(seen[seen.length - 1].message, "m219");
});

test("error throttling logs once then a summary after the window", async () => {
  let t = 0;
  const seen: UiLogEvent[] = [];
  const logger = createActivityLogger({
    flushFn: async (events) => {
      seen.push(...events);
    },
    nowFn: () => t,
  });
  logger.reportJsError("error", "boom");
  t = 30_000;
  logger.reportJsError("error", "boom");
  t = 61_000;
  logger.reportJsError("error", "boom");
  await logger.flushNow();
  assert.equal(seen.filter((e) => e.message.includes("期间重复 1 次")).length, 1);
  assert.equal(seen.filter((e) => e.message.startsWith("error: boom")).length, 3);
});
