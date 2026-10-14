import { useEffect } from "react";
import { toast } from "sonner";
import { useAuthActions } from "../hooks/useAuthActions";
import { authChipView } from "../lib/authUi";
import { LogoutConfirmDialog } from "./LogoutConfirmDialog";

/** Single header button: colored dot shows auth state, label shows the action. */
export function AuthChip() {
  const {
    authState,
    loggingIn,
    loggingOut,
    confirmLogout,
    hint,
    dismissHint,
    login,
    logout,
    retryStatus,
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

  const view = authChipView(authState);

  function handleClick() {
    if (view.action === "retry") {
      void retryStatus();
    } else if (view.action === "logout") {
      requestLogout();
    } else {
      void login();
    }
  }

  return (
    <>
      <button
        type="button"
        className={`auth-chip ${view.stateClass}`}
        data-action={`auth-${view.action}`}
        onClick={handleClick}
        disabled={view.disabled || loggingIn || loggingOut}
        title={view.title}
      >
        <span className="auth-chip-dot" />
        {loggingIn
          ? "登录中…"
          : loggingOut
            ? "登出中…"
            : view.label}
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
