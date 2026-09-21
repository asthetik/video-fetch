import { invoke } from "@tauri-apps/api/core";
// .ts extension required: node --test loads this module directly (ESM).
import { logUi } from "./activityLog.ts";

export type ThemeMode = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const THEME_STORAGE_KEY = "videofetch.theme";
export const THEME_CHANGE_EVENT = "videofetch:theme-changed";

export function resolveTheme(
  mode: ThemeMode,
  systemPrefersDark: boolean,
): ResolvedTheme {
  if (mode === "system") {
    return systemPrefersDark ? "dark" : "light";
  }
  return mode;
}

export function parseThemeMode(raw: string | null): ThemeMode {
  return raw === "light" || raw === "dark" || raw === "system" ? raw : "system";
}

/** Theme handed to the native window: fixed modes pin the titlebar to the
 * choice immediately, system mode passes null so the window keeps tracking
 * the OS instead of freezing on whatever the webview last resolved. */
export function nativeThemeFor(mode: ThemeMode): "light" | "dark" | null {
  return mode === "system" ? null : mode;
}

/** Last appearance handed to the native windows; undefined when nothing has
 * been sent yet or the last send failed, so the next apply retries. */
let lastNativeTheme: "light" | "dark" | null | undefined;
/** The native sync kicked off by the most recent applyTheme, if any. */
let lastNativeSync: Promise<void> | undefined;

function syncNativeTheme(mode: ThemeMode): void {
  if (!("__TAURI_INTERNALS__" in window)) {
    return; // Browser dev/preview and node tests: no native titlebar to sync.
  }
  const native = nativeThemeFor(mode);
  if (native === lastNativeTheme) {
    return; // Already applied; re-sending would only re-run the native nudge.
  }
  lastNativeTheme = native;
  lastNativeSync = invoke("set_window_theme", { theme: native })
    .then(() => {
      // The resolution above still read the appearance pinned until this
      // call landed (a pinned window drives prefers-color-scheme), so a
      // switch to system may have resolved against the stale pin. Re-resolve
      // now so the UI flips without waiting for the media-query event.
      if (getThemeMode() !== "system") {
        return;
      }
      if (
        resolveTheme("system", systemPrefersDark()) ===
        document.documentElement.dataset.theme
      ) {
        return;
      }
      applyTheme("system");
    })
    .catch((error) => {
      // IPC rejected (rare: command missing or window gone): the webview theme
      // still applies, but the titlebar would silently stop following — record
      // it, and allow a later apply to retry.
      lastNativeTheme = undefined;
      logUi(
        "theme",
        `同步原生窗口主题失败: ${error instanceof Error ? error.message : String(error)}`,
        "warn",
      );
    });
}

/** Settles when the most recent native theme sync finished (already resolved
 * when nothing had to be sent). main.tsx awaits this before revealing the
 * window so its first visible frame carries the right titlebar. */
export function nativeThemeSynced(): Promise<void> {
  return lastNativeSync ?? Promise.resolve();
}

function systemPrefersDark(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function announce(resolved: ResolvedTheme): void {
  window.dispatchEvent(
    new CustomEvent<ResolvedTheme>(THEME_CHANGE_EVENT, { detail: resolved }),
  );
}

export function applyTheme(mode: ThemeMode): ResolvedTheme {
  const resolved = resolveTheme(mode, systemPrefersDark());
  document.documentElement.dataset.theme = resolved;
  document.documentElement.style.colorScheme = resolved;
  syncNativeTheme(mode);
  announce(resolved);
  return resolved;
}

export function getThemeMode(): ThemeMode {
  try {
    return parseThemeMode(localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    return "system";
  }
}

export function setThemeMode(mode: ThemeMode): ResolvedTheme {
  try {
    localStorage.setItem(THEME_STORAGE_KEY, mode);
  } catch {
    // Storage unavailable (private mode): keep this session's override only.
  }
  return applyTheme(mode);
}

/** Apply stored/system theme at startup; returns cleanup for the media listener. */
export function initThemeSystem(): () => void {
  applyTheme(getThemeMode());
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = () => {
    if (getThemeMode() === "system") {
      applyTheme("system");
    }
  };
  mq.addEventListener("change", onChange);
  return () => mq.removeEventListener("change", onChange);
}
