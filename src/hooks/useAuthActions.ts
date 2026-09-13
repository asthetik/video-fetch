import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { api } from "../lib/tauri";
import type { AuthStatus as AuthStatusType } from "../types";

const LOGIN_HINT =
  "未获取到登录凭证。请完成网页登录，或到设置 → 高级导入 cookies.txt。";

/**
 * Shared auth state + actions for header-style controls: initial fetch,
 * auth://status event stream, busy flags, logout confirmation, and a transient
 * hint for failures. Consumers without room for a hint line surface it as a
 * toast instead.
 */
export function useAuthActions() {
  const [status, setStatus] = useState<AuthStatusType>("logged_out");
  const [loggingIn, setLoggingIn] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const [confirmLogout, setConfirmLogout] = useState(false);
  const [hint, setHint] = useState<string | null>(null);

  useEffect(() => {
    void api.getAuthStatus().then(setStatus);
    let unlisten: (() => void) | undefined;
    void listen<AuthStatusType>("auth://status", (event) => {
      setStatus(event.payload);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, []);

  const login = useCallback(async () => {
    setLoggingIn(true);
    setHint(null);
    try {
      const next = await api.startBilibiliLogin();
      if (next !== "logged_in") {
        setHint(LOGIN_HINT);
      }
    } catch (err) {
      setHint(err instanceof Error ? err.message : String(err));
    } finally {
      setLoggingIn(false);
    }
  }, []);

  const logout = useCallback(async () => {
    setConfirmLogout(false);
    setLoggingOut(true);
    setHint(null);
    try {
      await api.clearAuth();
      await api.getAuthStatus();
      toast.success("已退出登录");
    } catch (err) {
      setHint(err instanceof Error ? err.message : String(err));
    } finally {
      setLoggingOut(false);
    }
  }, []);

  return {
    status,
    loggingIn,
    loggingOut,
    confirmLogout,
    hint,
    dismissHint: useCallback(() => setHint(null), []),
    login,
    logout,
    requestLogout: useCallback(() => setConfirmLogout(true), []),
    cancelLogout: useCallback(() => setConfirmLogout(false), []),
  };
}
