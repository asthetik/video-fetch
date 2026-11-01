// Anti-FOUC: resolve theme before React mounts (see src/lib/theme.ts).
// Loaded as a classic external script so it runs before first paint; the
// production CSP has no 'unsafe-inline', which an inline copy of this
// would require (or a byte-exact hash that vite's html re-indentation
// would break — source and dist bytes differ on whitespace alone).
try {
  var m = localStorage.getItem("videofetch.theme");
  var dark =
    m === "dark" ||
    (m !== "light" &&
      window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
} catch (e) {}
