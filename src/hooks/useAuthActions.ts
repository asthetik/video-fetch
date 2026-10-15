import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { api } from "../lib/tauri";
import {
  toErrorMessage,
  toLoadState,
  type LoadState,
} from "../lib/asyncAction";
import type { AuthStatus as AuthStatusType } from "../types";

const LOGIN_HINT =
  "未获取到登录凭证。请完成网页登录，或到设置 → 高级导入 cookies.txt。";

/**
 * Shared auth state + actions for auth controls: initial fetch,
 * auth://status event stream, busy flags, logout confirmation, and a transient
 * hint for failures. Consumers without room for a hint line surface it as a
 * toast instead. authState is a LoadState so a failed status read is its own
 * error state with retry — never a silent stand-in for logged_out.
 */
export function useAuthActions(
  onStatusChange?: (status: AuthStatusType) => void,
) {
  const [authState, setAuthState] = useState<LoadState<AuthStatusType>>({
    status: "loading",
  });
  const [loggingIn, setLoggingIn] = useState(false);
  const [loggingOut, setLoggingOut] = useState(false);
  const [confirmLogout, setConfirmLogout] = useState(false);
  const [hint, setHint] = useState<string | null>(null);

  const applyStatus = useCallback(
    (next: AuthStatusType) => {
      setAuthState({ status: "ready", data: next });
      onStatusChange?.(next);
    },
    [onStatusChange],
  );

  const retryStatus = useCallback(async () => {
    setAuthState({ status: "loading" });
    const next = await toLoadState(() => api.getAuthStatus());
    if (next.status === "ready") applyStatus(next.data);
    else setAuthState(next);
  }, [applyStatus]);

  const dismissHint = useCallback(() => setHint(null), []);
  const requestLogout = useCallback(() => setConfirmLogout(true), []);
  const cancelLogout = useCallback(() => setConfirmLogout(false), []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let active = true;
    void toLoadState(() => api.getAuthStatus()).then((next) => {
      if (!active) return;
      if (next.status === "ready") applyStatus(next.data);
      else setAuthState(next);
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
      setHint(toErrorMessage(err));
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
      setHint(toErrorMessage(err));
    } finally {
      setLoggingOut(false);
    }
  }, [applyStatus]);

  return {
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
  };
}
