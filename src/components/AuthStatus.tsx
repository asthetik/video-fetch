import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { ConfirmDialog } from "./ConfirmDialog";
import { api } from "../lib/tauri";
import type { AuthStatus as AuthStatusType } from "../types";

const STATUS_LABEL: Record<AuthStatusType, string> = {
  logged_out: "未登录",
  logged_in: "已登录",
  possibly_expired: "登录可能过期",
};

const LOGOUT_CONFIRM = "确定退出登录？退出后将按未登录状态解析与下载。";

const LOGIN_HINT =
  "未获取到登录凭证。请完成网页登录，或到设置 → 高级导入 cookies.txt。";

interface AuthStatusProps {
  onStatusChange?: (status: AuthStatusType) => void;
}

export function AuthStatus({ onStatusChange }: AuthStatusProps) {
  const [status, setStatus] = useState<AuthStatusType>("logged_out");
  const [loggingIn, setLoggingIn] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const [confirmLogout, setConfirmLogout] = useState(false);
  const [hint, setHint] = useState<string | null>(null);

  const applyStatus = useCallback(
    (next: AuthStatusType) => {
      setStatus(next);
      onStatusChange?.(next);
    },
    [onStatusChange],
  );

  const refresh = useCallback(async () => {
    applyStatus(await api.getAuthStatus());
  }, [applyStatus]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void listen<AuthStatusType>("auth://status", (event) => {
      applyStatus(event.payload);
      if (event.payload === "logged_in") {
        setHint(null);
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [applyStatus]);

  async function handleLogin() {
    setLoggingIn(true);
    setHint(null);
    try {
      const next = await api.startBilibiliLogin();
      applyStatus(next);
      if (next !== "logged_in") {
        setHint(LOGIN_HINT);
      }
    } catch (err) {
      setHint(err instanceof Error ? err.message : String(err));
    } finally {
      setLoggingIn(false);
    }
  }

  async function handleLogout() {
    setConfirmLogout(false);
    setLoggingOut(true);
    setHint(null);
    try {
      await api.clearAuth();
      applyStatus(await api.getAuthStatus());
    } catch (err) {
      setHint(err instanceof Error ? err.message : String(err));
    } finally {
      setLoggingOut(false);
    }
  }

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
            onClick={() => void handleLogin()}
            disabled={loggingIn}
          >
            {loggingIn ? "登录中…" : "登录"}
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => setConfirmLogout(true)}
            disabled={loggingOut}
          >
            {loggingOut ? "登出中…" : "登出"}
          </button>
        )}
      </div>
      {hint && <p className="auth-hint">{hint}</p>}

      <ConfirmDialog
        open={confirmLogout}
        title="退出登录"
        message={LOGOUT_CONFIRM}
        confirmLabel="退出登录"
        cancelLabel="关闭"
        danger
        busy={loggingOut}
        onCancel={() => {
          if (!loggingOut) setConfirmLogout(false);
        }}
        onConfirm={() => void handleLogout()}
      />
    </div>
  );
}
