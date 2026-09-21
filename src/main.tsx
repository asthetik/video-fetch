import React from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import App from "./App";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { initActivityLog } from "./lib/activityLog";
import { initThemeSystem, nativeThemeSynced } from "./lib/theme";

initActivityLog();
initThemeSystem();
void revealMainWindow();

/** The main window starts hidden (tauri.conf.json) and is revealed once the
 * stored theme has been synced to the native side, so its first visible
 * frame already carries the right titlebar instead of flashing the system
 * appearance. Never settles without showing the window. */
async function revealMainWindow(): Promise<void> {
  try {
    await nativeThemeSynced();
    await getCurrentWindow().show();
  } catch {
    // Not running under Tauri (browser preview): nothing to reveal.
  }
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
);
