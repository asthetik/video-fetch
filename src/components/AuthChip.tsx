import { useEffect } from "react";
import { toast } from "sonner";
import { useAuthActions } from "../hooks/useAuthActions";
import { LogoutConfirmDialog } from "./LogoutConfirmDialog";
import type { AuthStatus as AuthStatusType } from "../types";

const TITLE: Record<AuthStatusType, string> = {
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

      <LogoutConfirmDialog
        open={confirmLogout}
        busy={loggingOut}
        onCancel={() => {
          if (!loggingOut) cancelLogout();
        }}
        onConfirm={() => void logout()}
      />
    </>
  );
}
