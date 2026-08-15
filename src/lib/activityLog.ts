import { invoke } from "@tauri-apps/api/core";

export type UiLogLevel = "debug" | "info" | "warn" | "error";

export interface UiLogEvent {
  level: UiLogLevel;
  category: string;
  message: string;
}

export interface ClickDescriptor {
  tagName: string;
  id?: string;
  ariaLabel?: string | null;
  title?: string | null;
  text?: string | null;
}

export function describeClickTarget(d: ClickDescriptor): string {
  const aria = d.ariaLabel || d.title;
  if (aria && aria.trim()) {
    return aria.trim();
  }
  const text = d.text?.replace(/\s+/g, " ").trim() ?? "";
  if (text) {
    return text.slice(0, 40);
  }
  return `${d.tagName.toLowerCase()}${d.id ? `#${d.id}` : ""}`;
}

export interface LogLineParts {
  timestamp: string;
  level: "DEBUG" | "INFO" | "WARN" | "ERROR";
  message: string;
}

/** Parse an RFC3339 `LEVEL message` log line into display parts. */
export function parseLogLine(line: string): LogLineParts | null {
  const match = /^(\S+)\s+(DEBUG|INFO|WARN|ERROR)\s+(.*)$/.exec(line);
  if (!match) {
    return null;
  }
  return {
    timestamp: match[1],
    level: match[2] as LogLineParts["level"],
    message: match[3],
  };
}

const FLUSH_INTERVAL_MS = 500;
const FLUSH_BATCH_SIZE = 20;
const ERROR_WINDOW_MS = 60_000;
const ERROR_SIGNATURE_LEN = 120;
const MAX_QUEUE = 200;
const MAX_ERROR_BUCKETS = 200;

interface ActivityLoggerOptions {
  flushFn?: (events: UiLogEvent[]) => Promise<void>;
  nowFn?: () => number;
}

async function defaultFlush(events: UiLogEvent[]) {
  await invoke("log_ui_events", { events });
}

export function createActivityLogger(options: ActivityLoggerOptions = {}) {
  const flushFn = options.flushFn ?? defaultFlush;
  const now = options.nowFn ?? Date.now;
  let queue: UiLogEvent[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;
  let flushing = false;
  const errors = new Map<string, { count: number; firstAt: number }>();

  function scheduleFlush() {
    if (timer === null) {
      timer = setTimeout(() => {
        timer = null;
        void flush();
      }, FLUSH_INTERVAL_MS);
    }
  }

  async function flush() {
    if (queue.length === 0 || flushing) return;
    flushing = true;
    const batch = queue.splice(0, queue.length);
    try {
      await flushFn(batch);
    } catch {
      if (queue.length + batch.length <= MAX_QUEUE) {
        queue.unshift(...batch);
      }
    } finally {
      flushing = false;
    }
  }

  function logUi(category: string, message: string, level: UiLogLevel = "info") {
    queue.push({ level, category, message });
    if (queue.length >= FLUSH_BATCH_SIZE) {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      void flush();
    } else {
      scheduleFlush();
    }
  }

  function reportJsError(kind: "error" | "unhandledrejection", message: string) {
    const t = now();
    const key = `${kind}:${message.slice(0, ERROR_SIGNATURE_LEN)}`;
    const bucket = errors.get(key);
    if (!bucket) {
      logUi("js", `${kind}: ${message}`, "error");
      errors.set(key, { count: 0, firstAt: t });
    } else if (t - bucket.firstAt < ERROR_WINDOW_MS) {
      bucket.count += 1;
    } else {
      if (bucket.count > 0) {
        logUi("js", `${kind}: ${message}（期间重复 ${bucket.count} 次）`, "error");
      }
      errors.set(key, { count: 0, firstAt: t });
      logUi("js", `${kind}: ${message}`, "error");
    }
    if (errors.size > MAX_ERROR_BUCKETS) {
      const oldest = errors.keys().next();
      if (oldest && oldest.value) {
        errors.delete(oldest.value);
      }
    }
  }

  function flushNow() {
    if (timer !== null) {
      clearTimeout(timer);
      timer = null;
    }
    return flush();
  }

  return { logUi, reportJsError, flushNow };
}

const logger = createActivityLogger();

export const logUi = logger.logUi;
export const reportJsError = logger.reportJsError;

export function initActivityLog() {
  document.addEventListener(
    "click",
    (event) => {
      const el = event.target instanceof Element ? event.target : null;
      if (!el) return;
      if (el.closest("[data-action]")) {
        // Explicit logUi() in the handler owns these; avoid double-logging.
        return;
      }
      logUi(
        "click",
        describeClickTarget({
          tagName: el.tagName,
          id: el.id || undefined,
          ariaLabel: el.getAttribute("aria-label"),
          title: el.getAttribute("title"),
          text: el.textContent,
        }),
        "debug",
      );
    },
    true,
  );
  window.addEventListener("error", (e) => {
    reportJsError("error", e.message || "未知错误");
  });
  window.addEventListener("unhandledrejection", (e) => {
    const reason = e.reason instanceof Error ? e.reason.message : String(e.reason);
    reportJsError("unhandledrejection", reason);
  });
  window.addEventListener("beforeunload", () => {
    void logger.flushNow();
  });
  return logger;
}
