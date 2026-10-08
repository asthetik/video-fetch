import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { initActivityLog, logUi } from "./lib/activityLog";
import { initThemeSystem, nativeThemeSynced } from "./lib/theme";

initActivityLog();
initThemeSystem();
void revealMainWindow();

/** The main window starts hidden (tauri.conf.json) and is revealed once the
 * stored theme has been synced to the native side, so its first visible
 * frame already carries the right titlebar instead of flashing the system
 * appearance. Never settles without showing the window. */
async function revealMainWindow(): Promise<void> {
  // Same guard as `syncNativeTheme` in theme.ts. In a browser there is no
  // native window to reveal and no fallback thread behind it, so returning
  // here keeps every recorded warn meaning "a real failure".
  if (!("__TAURI_INTERNALS__" in window)) {
    return;
  }
  try {
    await nativeThemeSynced();
    await getCurrentWindow().show();
  } catch (e) {
    // Never swallow this. A failed reveal is invisible: the three-second
    // fallback thread in `lib.rs` still shows the window, so the only symptom
    // is a slower start. That is how `plugin:window|show` went ungranted in
    // capabilities/default.json from 2026-09-21 and every launch since waited
    // out the fallback.
    logUi("startup", `显示主窗口失败，将由兜底线程显示：${String(e)}`, "warn");
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
