// src/components/AuthChip.tsx
import { useEffect } from "react";
import { toast } from "sonner";
import { ConfirmDialog } from "./ConfirmDialog";
import { useAuthActions } from "../hooks/useAuthActions";

const TITLE: Record<string, string> = {
  logged_in: "已登录 · 点击退出登录",
  logged_out: "未登录 · 点击登录 B 站",
  possibly_expired: "登录可能过期 · 点击退出登录",
};

/** Single header button: colored dot shows auth state, label shows the action. */
export function AuthChip() {
  const {
    status,
    loggingIn,
    loggingOut,
    confirmLogout,
    hint,
    dismissHint,
    login,
    logout,
    requestLogout,
    cancelLogout,
  } = useAuthActions();

  // The button has no room for a hint line; surface failures as toasts.
  useEffect(() => {
    if (hint) {
      toast.error(hint);
      dismissHint();
    }
  }, [hint, dismissHint]);

  const loggedIn = status !== "logged_out";

  return (
    <>
      <button
        type="button"
        className={`auth-chip ${status}`}
        data-action={loggedIn ? "auth-logout" : "auth-login"}
        onClick={loggedIn ? requestLogout : () => void login()}
        disabled={loggingIn || loggingOut}
        title={TITLE[status]}
      >
        <span className="auth-chip-dot" />
        {loggedIn
          ? loggingOut
            ? "登出中…"
            : "登出"
          : loggingIn
            ? "登录中…"
            : "登录"}
      </button>

      <ConfirmDialog
        open={confirmLogout}
        title="退出登录"
        message="确定退出登录？退出后将按未登录状态解析与下载。"
        confirmLabel="退出登录"
        cancelLabel="关闭"
        danger
        busy={loggingOut}
        onCancel={() => {
          if (!loggingOut) cancelLogout();
        }}
        onConfirm={() => void logout()}
      />
    </>
  );
}
