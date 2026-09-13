import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { api } from "../lib/tauri";
import type { AuthStatus as AuthStatusType } from "../types";

const LOGIN_HINT =
  "未获取到登录凭证。请完成网页登录，或到设置 → 高级导入 cookies.txt。";

/**
 * Shared auth state + actions for auth controls: initial fetch,
 * auth://status event stream, busy flags, logout confirmation, and a transient
 * hint for failures. Consumers without room for a hint line surface it as a
 * toast instead.
 */
export function useAuthActions(
  onStatusChange?: (status: AuthStatusType) => void,
) {
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

  const dismissHint = useCallback(() => setHint(null), []);
  const requestLogout = useCallback(() => setConfirmLogout(true), []);
  const cancelLogout = useCallback(() => setConfirmLogout(false), []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    void api.getAuthStatus().then((next) => {
      if (active) applyStatus(next);
    });
    void listen<AuthStatusType>("auth://status", (event) => {
      applyStatus(event.payload);
      if (event.payload === "logged_in") {
        setHint(null);
      }
    }).then((fn) => {
      if (active) unlisten = fn;
      else fn();
    });
    return () => {
      active = false;
      unlisten?.();
    };
  }, [applyStatus]);

  const login = useCallback(async () => {
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
  }, [applyStatus]);

  const logout = useCallback(async () => {
    setConfirmLogout(false);
    setLoggingOut(true);
    setHint(null);
    try {
      await api.clearAuth();
      applyStatus(await api.getAuthStatus());
      toast.success("已退出登录");
    } catch (err) {
      setHint(err instanceof Error ? err.message : String(err));
    } finally {
      setLoggingOut(false);
    }
  }, [applyStatus]);

  return {
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
  };
}
