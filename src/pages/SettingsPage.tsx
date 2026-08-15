import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../lib/tauri";
import { logUi } from "../lib/activityLog";
import { AuthStatus } from "../components/AuthStatus";
import { NamingPreview } from "../components/NamingPreview";
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

const AUTOSAVE_DEBOUNCE_MS = 400;

export function SettingsPage() {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [importing, setImporting] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [customOpen, setCustomOpen] = useState(false);

  const debounceTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingSettingsRef = useRef<AppSettings | null>(null);

  const persist = useCallback(async (next: AppSettings) => {
    try {
      await api.saveSettings(next);
      logUi("settings", "保存设置", "info");
      setMessage((prev) =>
        prev && prev.startsWith("无法保存设置") ? null : prev,
      );
    } catch (err) {
      const detail = err instanceof Error ? err.message : String(err);
      setMessage(`无法保存设置：${detail}`);
    }
  }, []);

  useEffect(() => {
    void api.getSettings().then(setSettings);
  }, []);

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
    setSettings((prev) => {
      if (!prev) {
        return prev;
      }
      const next = { ...prev, ...partial };
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
      return next;
    });
  }

  async function handlePickDir() {
    try {
      const dir = await api.pickSaveDir();
      patch({ save_dir: dir });
    } catch {
      // User cancelled the dialog.
    }
  }

  async function handleImportCookies() {
    setImporting(true);
    setMessage(null);
    try {
      const path = await api.pickCookiesFile();
      await api.importCookiesPath(path);
      setMessage("Cookies 已导入");
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (!msg.includes("取消")) {
        setMessage(msg);
      }
    } finally {
      setImporting(false);
    }
  }

  if (!settings) {
    return <p className="queue-empty">加载设置中…</p>;
  }

  const activePreset = NAMING_PRESETS.find((p) => p.template === settings.filename_template);

  return (
    <div className="settings-page">
      <h2 className="page-title">设置</h2>

      <section className="settings-section">
        <h3>下载</h3>

        <label className="settings-field">
          <span className="field-label">保存目录</span>
          <div className="settings-row">
            <input
              type="text"
              className="settings-input"
              value={settings.save_dir}
              readOnly
            />
            <button type="button" className="btn" onClick={() => void handlePickDir()}>
              选择…
            </button>
          </div>
        </label>

        <label className="settings-field">
          <span className="field-label">并发下载数</span>
          <input
            type="number"
            className="settings-input settings-input-narrow"
            min={1}
            max={8}
            value={settings.concurrency}
            onChange={(e) =>
              patch({ concurrency: Math.max(1, Math.min(8, Number(e.target.value))) })
            }
          />
        </label>

        <label className="settings-check">
          <input
            type="checkbox"
            checked={settings.skip_existing}
            onChange={(e) => patch({ skip_existing: e.target.checked })}
          />
          <span>
            本地已有文件时默认跳过
            <span className="settings-check-hint">
              开启后，主页点下载若检测到本地文件则直接跳过；关闭后，检测到本地文件会自动另存一份（不覆盖原文件）。
            </span>
          </span>
        </label>

      </section>

      <section className="settings-section">
        <h3>文件命名</h3>
        <p className="settings-hint">
          选择预设或自定义模板；变量：title、id、uploader、timestamp / upload_date（本地时区）、ext
        </p>

        <div className="preset-buttons">
          {NAMING_PRESETS.map((preset) => (
            <button
              key={preset.template}
              type="button"
              className={`btn btn-sm${activePreset?.template === preset.template ? " preset-active" : ""}`}
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
        <h3>B 站登录</h3>
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

      {message && (
        <div className="settings-footer">
          <p
            className={`settings-message${
              message.includes("失败") || message.includes("无法") ? " error" : ""
            }`}
          >
            {message}
          </p>
        </div>
      )}
    </div>
  );
}
