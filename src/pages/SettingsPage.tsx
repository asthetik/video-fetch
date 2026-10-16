import { useCallback, useEffect, useRef, useState } from "react";
import { Download, FileCode2, FolderOpen, Palette, UserRound } from "lucide-react";
import { toast } from "sonner";
import { api } from "../lib/tauri";
import {
  toErrorMessage,
  toLoadState,
  withToast,
  type LoadState,
} from "../lib/asyncAction";
import { saveDirPlaceholder } from "../lib/saveDirDisplay";
import { saveDirRowHint } from "../lib/saveDirNotice";
import { AuthStatus } from "../components/AuthStatus";
import { NamingPreview } from "../components/NamingPreview";
import { Segmented } from "../components/Segmented";
import { useTheme } from "../hooks/useTheme";
import type { AppSettings } from "../types";

const NAMING_PRESETS: { label: string; template: string }[] = [
  { label: "标题 + ID", template: "%(title)s [%(id)s].%(ext)s" },
  { label: "UP主 / 标题", template: "%(uploader)s/%(title)s [%(id)s].%(ext)s" },
  {
    label: "日期_标题",
    template: "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s.%(ext)s",
  },
  {
    label: "日期_标题+ID",
    template: "%(timestamp>%Y-%m-%dT%H-%M-%S)s_%(title)s [%(id)s].%(ext)s",
  },
];

const CONCURRENCY_MIN = 1;
const CONCURRENCY_MAX = 16;

const AUTOSAVE_DEBOUNCE_MS = 400;

export function SettingsPage() {
  const [settingsState, setSettingsState] = useState<LoadState<AppSettings>>({
    status: "loading",
  });
  const settings = settingsState.status === "ready" ? settingsState.data : null;
  const [importing, setImporting] = useState(false);
  const [customOpen, setCustomOpen] = useState(false);
  // Session notice from the backend: set when the configured save directory
  // was unusable at startup and downloads fell back to app data. Mirrors the
  // startup toast in App.tsx, which is easy to miss; the row hint below stays
  // up for the whole session so the condition stays discoverable.
  const [saveDirNotice, setSaveDirNotice] = useState<string | null>(null);
  const { mode, setMode } = useTheme();

  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingSettingsRef = useRef<AppSettings | null>(null);

  const persist = useCallback(async (next: AppSettings) => {
    try {
      await api.saveSettings(next);
    } catch (err) {
      toast.error(`无法保存设置：${toErrorMessage(err)}`);
    }
  }, []);

  const loadSettings = useCallback(async () => {
    setSettingsState({ status: "loading" });
    setSettingsState(await toLoadState(() => api.getSettings()));
  }, []);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  useEffect(() => {
    // Re-derived on every save-dir change: the backend gates the notice on
    // the session fallback still being active, so picking a working
    // directory clears the hint instead of leaving it claiming app-data for
    // the rest of the session.
    let cancelled = false;
    api
      .getSaveDirNotice()
      .then((notice) => {
        if (!cancelled) {
          setSaveDirNotice(notice);
        }
      })
      .catch(() => {
        // A notice read failure must never break the settings page; the row
        // just shows no hint.
      });
    return () => {
      cancelled = true;
    };
  }, [settings?.save_dir]);

  useEffect(() => {
    return () => {
      if (debounceTimerRef.current !== null) {
        clearTimeout(debounceTimerRef.current);
        debounceTimerRef.current = null;
      }
      const pending = pendingSettingsRef.current;
      if (pending) {
        pendingSettingsRef.current = null;
        void api.saveSettings(pending).catch(() => {
          // Unmount: best-effort flush; cannot setState.
        });
      }
    };
  }, []);

  function patch(partial: Partial<AppSettings>, options?: { debounce?: boolean }) {
    setSettingsState((prev) => {
      if (prev.status !== "ready") {
        return prev;
      }
      const next = { ...prev.data, ...partial };
      pendingSettingsRef.current = next;
      if (options?.debounce) {
        if (debounceTimerRef.current !== null) {
          clearTimeout(debounceTimerRef.current);
        }
        debounceTimerRef.current = setTimeout(() => {
          debounceTimerRef.current = null;
          const latest = pendingSettingsRef.current;
          if (latest) {
            pendingSettingsRef.current = null;
            void persist(latest);
          }
        }, AUTOSAVE_DEBOUNCE_MS);
      } else {
        if (debounceTimerRef.current !== null) {
          clearTimeout(debounceTimerRef.current);
          debounceTimerRef.current = null;
        }
        pendingSettingsRef.current = null;
        void persist(next);
      }
      return { status: "ready", data: next };
    });
  }

  async function handlePickDir() {
    try {
      const dir = await api.pickSaveDir();
      patch({ save_dir: dir });
    } catch (err) {
      // Cancel stays silent; anything else (e.g. a path we cannot persist)
      // must surface so the user can pick another folder.
      const msg = toErrorMessage(err);
      if (!msg.includes("取消")) {
        toast.error(msg);
      }
    }
  }

  async function handleOpenSaveDir() {
    const dir = settings?.save_dir;
    if (!dir) {
      return;
    }
    await withToast(
      () => api.openPath(dir),
      (message) => toast.error(message),
    );
  }

  async function handleImportCookies() {
    setImporting(true);
    try {
      const path = await api.pickCookiesFile();
      await api.importCookiesPath(path);
      toast.success("Cookies 已导入");
    } catch (err) {
      const msg = toErrorMessage(err);
      if (!msg.includes("取消")) {
        toast.error(msg);
      }
    } finally {
      setImporting(false);
    }
  }

  if (settingsState.status === "error") {
    return (
      <div className="settings-page">
        <h2 className="page-title">设置</h2>
        <p className="url-hint error">设置加载失败：{settingsState.message}</p>
        <div>
          <button
            type="button"
            className="btn"
            onClick={() => void loadSettings()}
          >
            重试
          </button>
        </div>
      </div>
    );
  }

  if (!settings) {
    return <p className="queue-empty">加载设置中…</p>;
  }

  const activePreset = NAMING_PRESETS.find((p) => p.template === settings.filename_template);
  const saveDirHint = saveDirRowHint(saveDirNotice);

  return (
    <div className="settings-page">
      <h2 className="page-title">设置</h2>

      <section className="settings-section">
        <h3 className="settings-section-title">
          <Palette size={14} strokeWidth={2} />
          外观
        </h3>
        <div className="settings-row-block">
          <div>
            <p className="settings-row-label">主题</p>
            <p className="settings-hint">深浅色跟随系统或手动固定</p>
          </div>
          <Segmented
            options={[
              { value: "system", label: "跟随系统" },
              { value: "light", label: "浅色" },
              { value: "dark", label: "深色" },
            ]}
            value={mode}
            ariaLabel="主题"
            onChange={setMode}
          />
        </div>
      </section>

      <section className="settings-section">
        <h3 className="settings-section-title">
          <Download size={14} strokeWidth={2} />
          下载
        </h3>

        <label className="settings-field">
          <span className="field-label">保存目录</span>
          <div className="settings-row">
            <input
              type="text"
              className="settings-input"
              value={settings.save_dir}
              // A mounted settings page always runs under a backend session
              // that has an effective save dir; an empty setting means that
              // dir cannot cross IPC as a String, not "not configured". With
              // the unavailability hint shown the session fell back to the
              // app-data dir, so the placeholder stands down and lets the
              // hint below speak (see saveDirDisplay.ts).
              placeholder={saveDirPlaceholder(settings.save_dir, saveDirHint !== null)}
              readOnly
            />
            <button type="button" className="btn" onClick={() => void handlePickDir()}>
              更改…
            </button>
            <button
              type="button"
              className="btn"
              // Nothing to open while an unprintable path stands in: the
              // action would be an enabled no-op.
              disabled={!settings.save_dir}
              onClick={() => void handleOpenSaveDir()}
            >
              <FolderOpen size={14} strokeWidth={2} />
              打开
            </button>
          </div>
          {saveDirHint ? <p className="settings-hint warn">{saveDirHint}</p> : null}
        </label>

        <div className="settings-row-block concurrency-row">
          <div>
            <p className="settings-row-label">并发下载数</p>
            <p className="settings-hint">同时进行的下载任务数；过高可能触发 B 站风控</p>
          </div>
          <div className="concurrency-control">
            <input
              type="range"
              className="concurrency-slider"
              min={CONCURRENCY_MIN}
              max={CONCURRENCY_MAX}
              step={1}
              value={settings.concurrency}
              aria-label="并发下载数"
              onChange={(e) =>
                patch({ concurrency: Number(e.target.value) }, { debounce: true })
              }
            />
            <span className="concurrency-value">{settings.concurrency}</span>
          </div>
        </div>

        <div className="settings-row-block">
          <div>
            <p className="settings-row-label">跳过本地已有文件</p>
            <p className="settings-hint">
              开启后，主页点下载若检测到本地文件则直接跳过；关闭后，检测到本地文件会自动另存一份（不覆盖原文件）。
            </p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={settings.skip_existing}
            aria-label="跳过本地已有文件"
            className={`switch${settings.skip_existing ? " on" : ""}`}
            onClick={() => patch({ skip_existing: !settings.skip_existing })}
          >
            <span className="switch-knob" />
          </button>
        </div>
      </section>

      <section className="settings-section">
        <h3 className="settings-section-title">
          <FileCode2 size={14} strokeWidth={2} />
          文件命名
        </h3>
        <p className="settings-hint">
          选择预设或自定义模板；变量：title、id、uploader、timestamp / upload_date（本地时区）、ext
        </p>

        <div className="preset-buttons">
          {NAMING_PRESETS.map((preset) => (
            <button
              key={preset.template}
              type="button"
              className={`chip${activePreset?.template === preset.template ? " on" : ""}`}
              onClick={() => patch({ filename_template: preset.template })}
            >
              {preset.label}
            </button>
          ))}
        </div>

        <details
          className="custom-template"
          open={customOpen}
          onToggle={(e) => setCustomOpen((e.target as HTMLDetailsElement).open)}
        >
          <summary>自定义模板</summary>
          <textarea
            className="settings-textarea"
            rows={2}
            value={settings.filename_template}
            onChange={(e) =>
              patch({ filename_template: e.target.value }, { debounce: true })
            }
            spellCheck={false}
          />
        </details>

        <NamingPreview template={settings.filename_template} />
      </section>

      <section className="settings-section">
        <h3 className="settings-section-title">
          <UserRound size={14} strokeWidth={2} />
          B 站登录
        </h3>
        <div className="auth-settings">
          <AuthStatus />
        </div>

        <details className="settings-advanced">
          <summary>高级</summary>
          <div className="settings-advanced-body">
            <button
              type="button"
              className="btn btn-sm"
              onClick={() => void handleImportCookies()}
              disabled={importing}
            >
              {importing ? "导入中…" : "导入 cookies.txt"}
            </button>
            <p className="settings-hint">
              支持 Netscape 格式 cookies.txt；若网页登录失败可手动导入。
            </p>
          </div>
        </details>
      </section>
    </div>
  );
}
