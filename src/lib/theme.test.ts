import test from "node:test";
import assert from "node:assert/strict";
import {
  applyTheme,
  nativeThemeFor,
  parseThemeMode,
  resolveTheme,
  setThemeMode,
} from "./theme.ts";

test("resolveTheme follows system in system mode", () => {
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("system", false), "light");
});

test("resolveTheme ignores system when overridden", () => {
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("dark", false), "dark");
});

test("nativeThemeFor pins the native titlebar for fixed modes", () => {
  assert.equal(nativeThemeFor("light"), "light");
  assert.equal(nativeThemeFor("dark"), "dark");
});

test("nativeThemeFor hands the titlebar back to the OS in system mode", () => {
  assert.equal(nativeThemeFor("system"), null);
});

test("parseThemeMode validates stored values", () => {
  assert.equal(parseThemeMode("dark"), "dark");
  assert.equal(parseThemeMode("light"), "light");
  assert.equal(parseThemeMode("system"), "system");
  assert.equal(parseThemeMode(null), "system");
  assert.equal(parseThemeMode("nonsense"), "system");
});

// --- IPC wiring: the command name/payload here is the JS half of the
// set_window_theme contract (the Rust name is registered in lib.rs). ---

interface InvokeCall {
  cmd: string;
  args: Record<string, unknown>;
}

const g = globalThis as Record<string, any>;

g.CustomEvent ??= class {
  type: string;
  init?: unknown;
  constructor(type: string, init?: unknown) {
    this.type = type;
    this.init = init;
  }
};
g.document = {
  documentElement: { dataset: {} as Record<string, string>, style: {} as Record<string, string> },
};
const storage = new Map<string, string>();
g.localStorage = {
  getItem: (key: string) => storage.get(key) ?? null,
  setItem: (key: string, value: string) => void storage.set(key, value),
};

let calls: InvokeCall[] = [];
let mediaMatchesDark = false;
let invokeImpl: (cmd: string, args: Record<string, unknown>) => Promise<unknown> = () =>
  Promise.resolve();

function installWindow(options: { withInternals: boolean }): void {
  const base = {
    matchMedia: () => ({
      get matches() {
        return mediaMatchesDark;
      },
      addEventListener() {},
      removeEventListener() {},
    }),
    dispatchEvent: () => true,
  };
  g.window = options.withInternals
    ? {
        ...base,
        __TAURI_INTERNALS__: {
          invoke: (cmd: string, args: Record<string, unknown>) => {
            calls.push({ cmd, args });
            return invokeImpl(cmd, args);
          },
        },
      }
    : base;
}

const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const themeCalls = () => calls.filter((call) => call.cmd === "set_window_theme");

test("applyTheme without a Tauri host themes the page and sends no IPC", () => {
  installWindow({ withInternals: false });
  calls = [];
  assert.equal(applyTheme("dark"), "dark");
  assert.equal(g.document.documentElement.dataset.theme, "dark");
  assert.equal(calls.length, 0);
});

test("theme switches invoke set_window_theme with the pinned payload", async () => {
  installWindow({ withInternals: true });
  calls = [];
  mediaMatchesDark = false;
  invokeImpl = () => Promise.resolve();

  setThemeMode("dark");
  await settle();
  setThemeMode("system");
  await settle();

  assert.deepEqual(
    themeCalls().map((call) => call.args),
    [{ theme: "dark" }, { theme: null }],
  );
});

test("re-applying the same mode does not repeat the IPC", async () => {
  installWindow({ withInternals: true });
  calls = [];

  setThemeMode("light");
  await settle();
  const sent = themeCalls().length;
  setThemeMode("light");
  setThemeMode("light");
  await settle();

  assert.equal(themeCalls().length, sent);
});

test("switching to system re-resolves once the native pin is cleared", async () => {
  installWindow({ withInternals: true });
  calls = [];
  mediaMatchesDark = true; // the pinned dark appearance drives matchMedia
  invokeImpl = (cmd, args) => {
    if (cmd === "set_window_theme" && args.theme === null) {
      mediaMatchesDark = false; // AppKit applied the clear by the time the IPC lands
    }
    return Promise.resolve();
  };

  setThemeMode("dark");
  await settle();
  setThemeMode("system"); // first resolution still reads the stale pin
  await settle();

  assert.equal(g.document.documentElement.dataset.theme, "light");
  assert.deepEqual(
    themeCalls().map((call) => call.args),
    [{ theme: "dark" }, { theme: null }],
  );
});

test("a rejected IPC is logged and the next apply retries", async () => {
  installWindow({ withInternals: true });
  calls = [];
  mediaMatchesDark = false;
  let failing = true;
  invokeImpl = () => (failing ? Promise.reject(new Error("boom")) : Promise.resolve());

  setThemeMode("light"); // rejected
  await settle();
  failing = false;
  setThemeMode("light"); // memo was cleared, so this one retries
  await settle();

  assert.equal(themeCalls().length, 2);
  assert.equal(g.document.documentElement.dataset.theme, "light");

  // logUi buffers for 500ms before flushing through the same IPC stub.
  await new Promise((resolve) => setTimeout(resolve, 600));
  const logged = calls.filter((call) => call.cmd === "log_ui_events");
  assert.ok(
    logged.some((call) => JSON.stringify(call.args).includes("同步原生窗口主题失败")),
    "the rejected theme IPC should reach the activity log",
  );
});
