import type { AuthStatus } from "../types";
import type { LoadState } from "./asyncAction";

/**
 * View models for the auth controls. A failed status read is its own explicit
 * error state with a retry: "未登录" may only appear after a successful read
 * that actually returned logged_out, never as a stand-in for a failure.
 */

export type AuthLabelTone = "ok" | "warn" | "muted" | "error" | "loading";

/** Settings-row label for the current auth read. */
export function authStatusLabel(state: LoadState<AuthStatus>): string {
  if (state.status === "error") return "读取登录状态失败";
  if (state.status === "loading") return "读取登录状态…";
  switch (state.data) {
    case "logged_in":
      return "已登录";
    case "possibly_expired":
      return "登录可能过期";
    case "logged_out":
      return "未登录";
  }
}

/** Tone class (auth-label <tone>) for the settings-row label. */
export function authLabelTone(state: LoadState<AuthStatus>): AuthLabelTone {
  if (state.status === "error") return "error";
  // "loading", not "muted": the in-flight read is its own state, and reusing
  // the logged-out tone makes the row momentarily read as "未登录".
  if (state.status === "loading") return "loading";
  switch (state.data) {
    case "logged_in":
      return "ok";
    case "possibly_expired":
      return "warn";
    case "logged_out":
      return "muted";
  }
}

export interface AuthChipView {
  /** What a click does: login, log out, or retry a failed status read. */
  action: "login" | "logout" | "retry";
  /** Button text while no login/logout call is in flight. */
  label: string;
  /** Tooltip text. */
  title: string;
  /** State suffix for the `auth-chip <state>` class chain. */
  stateClass: string;
  disabled: boolean;
}

/** Header-chip presentation for the current auth read. */
export function authChipView(state: LoadState<AuthStatus>): AuthChipView {
  if (state.status === "loading") {
    return {
      action: "retry",
      label: "读取中…",
      title: "正在读取登录状态…",
      // Own class so .auth-chip.loading can style the in-flight read as its
      // own state instead of the muted "未登录" dot.
      stateClass: "loading",
      disabled: true,
    };
  }
  if (state.status === "error") {
    return {
      action: "retry",
      label: "重试",
      title: "读取登录状态失败 · 点击重试",
      stateClass: "error",
      disabled: false,
    };
  }
  switch (state.data) {
    case "logged_in":
      return {
        action: "logout",
        label: "登出",
        title: "已登录 · 点击退出登录",
        stateClass: "logged_in",
        disabled: false,
      };
    case "possibly_expired":
      return {
        action: "logout",
        label: "登出",
        title: "登录可能过期 · 点击退出登录",
        stateClass: "possibly_expired",
        disabled: false,
      };
    case "logged_out":
      return {
        action: "login",
        label: "登录",
        title: "未登录 · 点击登录 B 站",
        stateClass: "logged_out",
        disabled: false,
      };
  }
}
