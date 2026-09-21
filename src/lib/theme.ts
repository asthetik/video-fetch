import { getCurrentWindow } from "@tauri-apps/api/window";

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

function syncNativeTheme(mode: ThemeMode): void {
  try {
    void getCurrentWindow()
      .setTheme(nativeThemeFor(mode))
      .catch((error) => {
        // Tauri IPC rejected (missing permission, platform gap): the webview
        // theme still applies, but the titlebar silently stops following --
        // leave a trace instead of failing the whole apply.
        console.warn("[theme] native setTheme rejected:", error);
      });
  } catch {
    // Not running under Tauri (vite dev/preview, node tests): no native
    // titlebar to sync.
  }
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
