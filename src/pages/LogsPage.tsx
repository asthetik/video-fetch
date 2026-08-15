import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/tauri";
import { levelClass } from "../lib/activityLog";
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

  const refresh = useCallback(async () => {
    try {
      const [logDir, list] = await api.listLogFiles();
      setDir(logDir);
      setFiles(list);
      setSelected((prev) => prev || list[0]?.name || "");
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
  }, [selected]);

  async function handleClear() {
    setClearing(true);
    try {
      await api.clearLogHistory();
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
      <div className="logs-toolbar">
        <button type="button" className="btn" onClick={() => void refresh()}>
          刷新
        </button>
        <button
          type="button"
          className="btn"
          disabled={!dir}
          onClick={() => void api.openPath(dir)}
        >
          打开日志目录
        </button>
        <button
          type="button"
          className="btn btn-danger"
          disabled={files.length === 0}
          onClick={() => setConfirmOpen(true)}
        >
          清空历史日志
        </button>
      </div>
      {error && <p className="url-hint error">无法读取日志：{error}</p>}
      <div className="logs-layout">
        <ul className="log-files">
          {files.map((f) => (
            <li key={f.name}>
              <button
                type="button"
                className={f.name === selected ? "log-file active" : "log-file"}
                onClick={() => setSelected(f.name)}
              >
                <span>{f.name}</span>
                <span>{formatBytes(f.size)}</span>
              </button>
            </li>
          ))}
        </ul>
        <pre className="log-view">
          {lines.length === 0
            ? "（暂无日志）"
            : lines.map((line, i) => (
                <div key={i} className={`log-line ${levelClass(line)}`}>
                  {line}
                </div>
              ))}
        </pre>
      </div>
      <ConfirmDialog
        open={confirmOpen}
        title="清空历史日志？"
        message="将删除除今天以外的全部日志文件，此操作不可撤销。"
        confirmLabel="清空"
        danger
        busy={clearing}
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => void handleClear()}
      />
    </div>
  );
}
