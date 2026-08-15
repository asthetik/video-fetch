import test from "node:test";
import assert from "node:assert/strict";
import {
  createActivityLogger,
  describeClickTarget,
  levelClass,
  type UiLogEvent,
} from "./activityLog.ts";

test("describeClickTarget prefers aria-label then text then tag#id", () => {
  assert.equal(describeClickTarget({ tagName: "BUTTON", ariaLabel: "下载" }), "下载");
  assert.equal(describeClickTarget({ tagName: "BUTTON", text: "  立即\n下载  " }), "立即 下载");
  assert.equal(describeClickTarget({ tagName: "BUTTON", id: "go", text: "" }), "button#go");
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

test("levelClass maps warn/error lines", () => {
  assert.equal(levelClass("2026-08-15T12:00:00+08:00 ERROR ui x"), "log-line-error");
  assert.equal(levelClass("2026-08-15T12:00:00+08:00 WARN ui x"), "log-line-warn");
  assert.equal(levelClass("no level here"), "log-line-debug");
});
