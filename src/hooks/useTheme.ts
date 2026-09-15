import { useEffect, useState } from "react";
import {
  getThemeMode,
  setThemeMode,
  THEME_CHANGE_EVENT,
  type ResolvedTheme,
  type ThemeMode,
} from "../lib/theme";

function currentResolved(): ResolvedTheme {
  return document.documentElement.dataset.theme === "dark" ? "dark" : "light";
}

export function useTheme(): {
  mode: ThemeMode;
  resolved: ResolvedTheme;
  setMode: (mode: ThemeMode) => void;
} {
  const [mode, setModeState] = useState<ThemeMode>(getThemeMode);
  const [resolved, setResolved] = useState<ResolvedTheme>(currentResolved);

  useEffect(() => {
    const onTheme = (event: Event) => {
      setResolved((event as CustomEvent<ResolvedTheme>).detail);
    };
    window.addEventListener(THEME_CHANGE_EVENT, onTheme);
    return () => window.removeEventListener(THEME_CHANGE_EVENT, onTheme);
  }, []);

  const setMode = (next: ThemeMode) => {
    setModeState(next);
    setThemeMode(next);
  };

  return { mode, resolved, setMode };
}
