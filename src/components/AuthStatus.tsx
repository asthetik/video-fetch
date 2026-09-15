import { useAuthActions } from "../hooks/useAuthActions";
import { LogoutConfirmDialog } from "./LogoutConfirmDialog";
import type { AuthStatus as AuthStatusType } from "../types";

const STATUS_LABEL: Record<AuthStatusType, string> = {
  logged_out: "未登录",
  logged_in: "已登录",
  possibly_expired: "登录可能过期",
};

interface AuthStatusProps {
  onStatusChange?: (status: AuthStatusType) => void;
}

/** Settings auth row: state label, login/logout buttons, inline hint line. */
export function AuthStatus({ onStatusChange }: AuthStatusProps) {
  const {
    status,
    loggingIn,
    loggingOut,
    confirmLogout,
    hint,
    login,
    logout,
    requestLogout,
    cancelLogout,
  } = useAuthActions(onStatusChange);

  const labelClass =
    status === "logged_in"
      ? "ok"
      : status === "possibly_expired"
        ? "warn"
        : "muted";

  return (
    <div className="auth-status-wrap">
      <div className="auth-status">
        <span className={`auth-label ${labelClass}`}>{STATUS_LABEL[status]}</span>
        {status === "logged_out" ? (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => void login()}
            disabled={loggingIn}
          >
            {loggingIn ? "登录中…" : "登录"}
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-sm"
            onClick={requestLogout}
            disabled={loggingOut}
          >
            {loggingOut ? "登出中…" : "登出"}
          </button>
        )}
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
