import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/tauri";
import { parseLogLine } from "../lib/activityLog";
import { ConfirmDialog } from "../components/ConfirmDialog";
import type { LogFileInfo } from "../types";

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function LogsPage() {
  const [dir, setDir] = useState("");
  const [files, setFiles] = useState<LogFileInfo[]>([]);
  const [selected, setSelected] = useState("");
  const [lines, setLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [refreshTick, setRefreshTick] = useState(0);

  const refresh = useCallback(async () => {
    try {
      const [logDir, list] = await api.listLogFiles();
      setDir(logDir);
      setFiles(list);
      setSelected((prev) =>
        list.some((f) => f.name === prev) ? prev : (list[0]?.name ?? ""),
      );
      setRefreshTick((t) => t + 1);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!selected) {
      setLines([]);
      return;
    }
    let cancelled = false;
    api
      .readLogTail(selected)
      .then((next) => {
        if (!cancelled) setLines(next);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [selected, refreshTick]);

  async function handleClear() {
    setClearing(true);
    try {
      await api.clearLogs();
      setConfirmOpen(false);
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setClearing(false);
    }
  }

  return (
    <div className="logs-page">
      {error && <p className="url-hint error">无法读取日志：{error}</p>}
      <section className="logs-viewer">
        <div className="logs-viewer-head">
          <label className="logs-file-label" htmlFor="log-file-select">
            日志文件
          </label>
          <select
            id="log-file-select"
            className="log-file-select"
            value={selected}
            disabled={files.length === 0}
            onChange={(e) => setSelected(e.target.value)}
          >
            {files.length === 0 ? (
              <option value="">暂无日志文件</option>
            ) : (
              files.map((f) => (
                <option key={f.name} value={f.name}>
                  {f.name}（{formatBytes(f.size)}）
                </option>
              ))
            )}
          </select>
          <div className="logs-toolbar">
            <button type="button" className="btn btn-sm" onClick={() => void refresh()}>
              刷新
            </button>
            <button
              type="button"
              className="btn btn-sm"
              disabled={!dir}
              onClick={() => void api.openPath(dir)}
            >
              打开日志目录
            </button>
            <button
              type="button"
              className="btn btn-sm btn-danger"
              disabled={files.length === 0}
              onClick={() => setConfirmOpen(true)}
            >
              清空日志
            </button>
          </div>
        </div>
        <div className="log-view">
          {lines.length === 0 ? (
            <p className="logs-viewer-empty">
              {selected ? "该文件暂无内容" : "暂无日志"}
            </p>
          ) : (
            lines.map((line, i) => {
              const parts = parseLogLine(line);
              if (!parts) {
                return (
                  <div key={i} className="log-line log-line-raw">
                    {line}
                  </div>
                );
              }
              const level = parts.level.toLowerCase();
              return (
                <div key={i} className="log-line">
                  <span className="log-time">{parts.timestamp}</span>
                  <span className={`log-badge log-badge-${level}`}>
                    {parts.level}
                  </span>
                  <span className="log-msg">{parts.message}</span>
                </div>
              );
            })
          )}
        </div>
      </section>
      <ConfirmDialog
        open={confirmOpen}
        title="清空全部日志？"
        message="将清空所有本地日志文件（包括今天），此操作不可撤销。"
        confirmLabel="清空"
        danger
        busy={clearing}
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => void handleClear()}
      />
    </div>
  );
}
