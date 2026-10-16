import test from "node:test";
import assert from "node:assert/strict";
import {
  authChipView,
  authLabelTone,
  authStatusLabel,
} from "./authUi.ts";

// Finding A12: a failed getAuthStatus() read silently left the UI on the
// initial "logged_out" default, so the header chip and the settings row both
// misreported the user as "未登录". The error state must render as its own
// explicit failure with a retry, and "未登录" may appear only after a
// successful read that actually returned logged_out.

test("authStatusLabel reports an explicit error state, never '未登录'", () => {
  const label = authStatusLabel({ status: "error", message: "db locked" });
  assert.equal(label, "读取登录状态失败");
  assert.notEqual(label, "未登录");
});

test("authStatusLabel shows '未登录' only after a successful read", () => {
  assert.equal(
    authStatusLabel({ status: "ready", data: "logged_out" }),
    "未登录",
  );
  assert.equal(
    authStatusLabel({ status: "ready", data: "logged_in" }),
    "已登录",
  );
  assert.equal(
    authStatusLabel({ status: "ready", data: "possibly_expired" }),
    "登录可能过期",
  );
});

test("authStatusLabel shows a neutral placeholder while the read is in flight", () => {
  assert.notEqual(authStatusLabel({ status: "loading" }), "未登录");
});

test("authLabelTone colors the error state as an error, not muted", () => {
  assert.equal(authLabelTone({ status: "error", message: "x" }), "error");
  assert.equal(authLabelTone({ status: "loading" }), "muted");
  assert.equal(authLabelTone({ status: "ready", data: "logged_in" }), "ok");
  assert.equal(
    authLabelTone({ status: "ready", data: "possibly_expired" }),
    "warn",
  );
  assert.equal(authLabelTone({ status: "ready", data: "logged_out" }), "muted");
});

test("authChipView turns a failed read into a retry affordance", () => {
  const view = authChipView({ status: "error", message: "db locked" });
  assert.equal(view.action, "retry");
  assert.equal(view.label, "重试");
  assert.match(view.title, /重试/);
  assert.equal(view.stateClass, "error");
  assert.equal(view.disabled, false);
});

test("authChipView disables the chip while the read is in flight", () => {
  const view = authChipView({ status: "loading" });
  assert.equal(view.disabled, true);
  assert.notEqual(view.label, "登录");
});

// The in-flight chip gets its own stateClass: the old "unknown" had no CSS
// rule, so the chip rendered pixel-identical to the muted logged-out state
// while the read was still in flight.
test("authChipView marks the in-flight read with a loading class", () => {
  assert.equal(authChipView({ status: "loading" }).stateClass, "loading");
});

test("authChipView maps known statuses to login/logout actions", () => {
  const out = authChipView({ status: "ready", data: "logged_out" });
  assert.equal(out.action, "login");
  assert.equal(out.label, "登录");
  assert.equal(out.stateClass, "logged_out");

  const inn = authChipView({ status: "ready", data: "logged_in" });
  assert.equal(inn.action, "logout");
  assert.equal(inn.label, "登出");
  assert.equal(inn.stateClass, "logged_in");

  const expired = authChipView({ status: "ready", data: "possibly_expired" });
  assert.equal(expired.action, "logout");
  assert.equal(expired.stateClass, "possibly_expired");
});
