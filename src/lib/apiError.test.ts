import test from "node:test";
import assert from "node:assert/strict";
import { toApiError, toErrorMessage } from "./asyncAction.ts";

// B20: invoke rejections now carry the structured error envelope
// ({kind, message}) for annotated errors and stay bare strings for generic
// ones. toApiError is the one place that reads the envelope and must
// tolerate every historical shape.

test("a bare string passes through as the message", () => {
  assert.deepEqual(toApiError("保存失败"), { message: "保存失败" });
});

test("an envelope keeps its message and kind", () => {
  assert.deepEqual(toApiError({ kind: "cancelled", message: "登录已取消" }), {
    message: "登录已取消",
    kind: "cancelled",
  });
});

test("an unknown kind is dropped, the message kept", () => {
  assert.deepEqual(toApiError({ kind: "mystery", message: "x" }), {
    message: "x",
    kind: undefined,
  });
});

test("a plain {message} object (historical shape) keeps working", () => {
  assert.deepEqual(toApiError({ message: "x" }), {
    message: "x",
    kind: undefined,
  });
});

test("an Error instance passes its message through", () => {
  assert.deepEqual(toApiError(new Error("boom")), { message: "boom" });
});

test("anything else stringifies", () => {
  assert.deepEqual(toApiError(42), { message: "42" });
});

test("toErrorMessage delegates to the message", () => {
  assert.equal(
    toErrorMessage({ kind: "db", message: "数据库错误: x" }),
    "数据库错误: x",
  );
});
