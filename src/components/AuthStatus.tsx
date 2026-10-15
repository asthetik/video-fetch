import { useAuthActions } from "../hooks/useAuthActions";
import { authLabelTone, authStatusLabel } from "../lib/authUi";
import { LogoutConfirmDialog } from "./LogoutConfirmDialog";
import type { AuthStatus as AuthStatusType } from "../types";

interface AuthStatusProps {
  onStatusChange?: (status: AuthStatusType) => void;
}

/** Settings auth row: state label, login/logout buttons, inline hint line. */
export function AuthStatus({ onStatusChange }: AuthStatusProps) {
  const {
    authState,
    loggingIn,
    loggingOut,
    confirmLogout,
    hint,
    login,
    logout,
    retryStatus,
    requestLogout,
    cancelLogout,
  } = useAuthActions(onStatusChange);

  return (
    <div className="auth-status-wrap">
      <div className="auth-status">
        <span className={`auth-label ${authLabelTone(authState)}`}>
          {authStatusLabel(authState)}
        </span>
        {authState.status === "error" ? (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void retryStatus()}
          >
            重试
          </button>
        ) : authState.status === "ready" && authState.data === "logged_out" ? (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void login()}
            disabled={loggingIn}
          >
            {loggingIn ? "登录中…" : "登录"}
          </button>
        ) : authState.status === "ready" ? (
          <button
            type="button"
            className="btn btn-sm"
            onClick={requestLogout}
            disabled={loggingOut}
          >
            {loggingOut ? "登出中…" : "登出"}
          </button>
        ) : null}
      </div>
      {hint && <p className="auth-hint">{hint}</p>}

      <LogoutConfirmDialog
        open={confirmLogout}
        busy={loggingOut}
        onCancel={() => {
          if (!loggingOut) cancelLogout();
        }}
        onConfirm={() => void logout()}
      />
    </div>
  );
}
