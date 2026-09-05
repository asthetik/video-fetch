import { useCallback, useEffect, useRef, useState } from "react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
  DeleteConfirmDialog,
  type DeleteChoice,
} from "../components/DeleteConfirmDialog";
import { api } from "../lib/tauri";
import type { DownloadJob, JobStatus } from "../types";

const STATUS_LABEL: Record<JobStatus, string> = {
  pending: "等待中",
  running: "下载中",
  done: "完成",
  failed: "失败",
};

function parentDir(filePath: string): string {
  const normalized = filePath.replace(/\\/g, "/");
  const idx = normalized.lastIndexOf("/");
  return idx > 0 ? filePath.slice(0, idx) : filePath;
}

function jobLabel(job: DownloadJob): string {
  return job.page_index > 1 ? `${job.title} · P${job.page_index}` : job.title;
}

interface HistoryPageProps {
  onJobsChanged?: () => void;
}

export function HistoryPage({ onJobsChanged }: HistoryPageProps) {
  const [jobs, setJobs] = useState<DownloadJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [actionError, setActionError] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<DownloadJob | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [bulkBusy, setBulkBusy] = useState(false);
  const bulkBusyRef = useRef(false);
  const historyCount = jobs.length;

  const loadJobs = useCallback(async () => {
    const list = await api.listJobs();
    setJobs(list.filter((j) => j.status === "done" || j.status === "failed"));
    setLoading(false);
  }, []);

  useEffect(() => {
    void loadJobs();
  }, [loadJobs]);

  async function handleOpenFile(job: DownloadJob) {
    if (!job.output_path) {
      return;
    }
    await api.openPath(job.output_path);
  }

  async function handleOpenFolder(job: DownloadJob) {
    if (!job.output_path) {
      return;
    }
    await api.openPath(parentDir(job.output_path));
  }

  async function handleClearFinished() {
    if (bulkBusyRef.current) {
      return;
    }
    bulkBusyRef.current = true;
    setBulkBusy(true);
    setActionError(null);
    try {
      await api.clearFinishedJobs();
      setConfirmClear(false);
      setJobs([]);
      onJobsChanged?.();
    } catch (err) {
      setConfirmClear(false);
      setActionError(err instanceof Error ? err.message : String(err));
    } finally {
      bulkBusyRef.current = false;
      setBulkBusy(false);
    }
  }

  async function applyDelete(job: DownloadJob, choice: DeleteChoice) {
    setPendingDelete(null);
    if (choice === "cancel") {
      return;
    }

    setActionError(null);
    try {
      await api.deleteJob(job.id, choice === "record_and_file");
      setJobs((prev) => prev.filter((j) => j.id !== job.id));
      onJobsChanged?.();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
      await loadJobs();
    }
  }

  return (
    <div className="history-page">
      <div className="history-page-header">
        <div>
          <h2 className="page-title">下载历史</h2>
          <p className="page-desc">已完成或失败的下载任务</p>
        </div>
        {historyCount > 0 && (
          <button
            type="button"
            className="btn btn-sm"
            data-action="clear-finished"
            disabled={bulkBusy}
            onClick={() => {
              setActionError(null);
              setConfirmClear(true);
            }}
          >
            清空
          </button>
        )}
      </div>

      {loading && <p className="queue-empty">加载中…</p>}
      {actionError && <p className="url-hint error">{actionError}</p>}
      {!loading && jobs.length === 0 && (
        <p className="queue-empty">暂无历史记录</p>
      )}

      {!loading && jobs.length > 0 && (
        <ul className="queue-list">
          {jobs.map((job) => (
            <li key={job.id} className="queue-item">
              <div className="queue-item-header">
                <p className="queue-title">
                  {job.title}
                  {job.page_index > 1 ? ` · P${job.page_index}` : ""}
                </p>
                <span className={`queue-status ${job.status}`}>
                  {STATUS_LABEL[job.status]}
                </span>
              </div>

              {job.output_path && (
                <p className="history-path">{job.output_path}</p>
              )}
              {job.error && <p className="queue-error">{job.error}</p>}

              <div className="queue-actions">
                {job.status === "done" && job.output_path && (
                  <>
                    <button
                      type="button"
                      className="btn btn-sm"
                      data-action="open-file"
                      onClick={() => void handleOpenFile(job)}
                    >
                      打开文件
                    </button>
                    <button
                      type="button"
                      className="btn btn-sm"
                      data-action="open-folder"
                      onClick={() => void handleOpenFolder(job)}
                    >
                      打开文件夹
                    </button>
                  </>
                )}
                <button
                  type="button"
                  className="btn btn-sm"
                  data-action="delete-job"
                  onClick={() => {
                    setActionError(null);
                    setPendingDelete(job);
                  }}
                >
                  删除
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <ConfirmDialog
        open={confirmClear}
        title="清空下载历史"
        message={`将清除 ${historyCount} 条历史记录。本地已下载的文件不会被删除。`}
        confirmLabel="清空"
        cancelLabel="关闭"
        danger
        busy={bulkBusy}
        onCancel={() => {
          if (!bulkBusy) setConfirmClear(false);
        }}
        onConfirm={() => void handleClearFinished()}
      />

      <DeleteConfirmDialog
        open={pendingDelete !== null}
        jobTitle={pendingDelete ? jobLabel(pendingDelete) : ""}
        filePath={pendingDelete?.output_path}
        onChoose={(choice) => {
          if (pendingDelete) {
            void applyDelete(pendingDelete, choice);
          } else {
            setPendingDelete(null);
          }
        }}
      />
    </div>
  );
}
