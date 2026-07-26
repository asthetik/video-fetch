import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  DeleteConfirmDialog,
  type DeleteChoice,
} from "./DeleteConfirmDialog";
import { api } from "../lib/tauri";
import type { DownloadJob, JobStatus } from "../types";

interface DownloadProgressPayload {
  id: string;
  progress: number;
  status: JobStatus;
  error?: string | null;
  output_path?: string | null;
}

const STATUS_LABEL: Record<JobStatus, string> = {
  pending: "等待中",
  running: "下载中",
  done: "完成",
  failed: "失败",
};

function mergeJob(existing: DownloadJob, patch: DownloadProgressPayload): DownloadJob {
  return {
    ...existing,
    progress: patch.progress,
    status: patch.status,
    error: patch.error ?? existing.error,
    output_path: patch.output_path ?? existing.output_path,
  };
}

function parentDir(filePath: string): string {
  const normalized = filePath.replace(/\\/g, "/");
  const idx = normalized.lastIndexOf("/");
  return idx > 0 ? filePath.slice(0, idx) : filePath;
}

function jobLabel(job: DownloadJob): string {
  return job.page_index > 1 ? `${job.title} · P${job.page_index}` : job.title;
}

interface DownloadQueueProps {
  refreshToken: number;
}

export function DownloadQueue({ refreshToken }: DownloadQueueProps) {
  const [jobs, setJobs] = useState<DownloadJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [actionError, setActionError] = useState<string | null>(null);
  const [pendingDelete, setPendingDelete] = useState<DownloadJob | null>(null);

  const loadJobs = useCallback(async () => {
    const list = await api.listJobs();
    // Show active and recent jobs on the home page (newest first).
    const sorted = [...list].sort((a, b) => {
      const rank = (s: JobStatus) =>
        s === "running" ? 0 : s === "pending" ? 1 : s === "failed" ? 2 : 3;
      const diff = rank(a.status) - rank(b.status);
      return diff !== 0 ? diff : b.page_index - a.page_index;
    });
    setJobs(sorted);
    setLoading(false);
  }, []);

  useEffect(() => {
    void loadJobs();
  }, [loadJobs, refreshToken]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    void listen<DownloadProgressPayload>("download://progress", (event) => {
      const patch = event.payload;
      setJobs((prev) => {
        const idx = prev.findIndex((j) => j.id === patch.id);
        if (idx >= 0) {
          const next = [...prev];
          next[idx] = mergeJob(next[idx], patch);
          return next;
        }
        // Job may not be in local state yet; reload the full list.
        void loadJobs();
        return prev;
      });
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, [loadJobs]);

  async function handleCancel(id: string) {
    setActionError(null);
    try {
      const updated = await api.cancelJob(id);
      setJobs((prev) => prev.map((j) => (j.id === id ? updated : j)));
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleRetry(id: string) {
    setActionError(null);
    try {
      const updated = await api.retryJob(id);
      setJobs((prev) => prev.map((j) => (j.id === id ? updated : j)));
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
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
      await loadJobs();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : String(err));
      await loadJobs();
    }
  }

  async function handleOpenFolder(job: DownloadJob) {
    if (!job.output_path) {
      return;
    }
    await api.openPath(parentDir(job.output_path));
  }

  const activeJobs = jobs.filter(
    (j) => j.status === "pending" || j.status === "running" || j.status === "failed",
  );
  const displayJobs = activeJobs.length > 0 ? activeJobs : jobs.slice(0, 8);

  return (
    <section className="download-queue">
      <h3>下载队列</h3>
      {loading && <p className="queue-empty">加载中…</p>}
      {actionError && <p className="url-hint error">{actionError}</p>}
      {!loading && displayJobs.length === 0 && (
        <p className="queue-empty">暂无下载任务</p>
      )}
      {!loading && displayJobs.length > 0 && (
        <ul className="queue-list">
          {displayJobs.map((job) => (
            <li key={job.id} className="queue-item">
              <div className="queue-item-header">
                <p className="queue-title">
                  {job.title}
                  {job.page_index > 1 ? ` · P${job.page_index}` : ""}
                </p>
                <span className={`queue-status ${job.status}`}>
                  {STATUS_LABEL[job.status]}
                  {(job.status === "running" || job.status === "pending") &&
                    job.status === "running" &&
                    ` ${Math.round(job.progress * 100)}%`}
                </span>
              </div>

              {(job.status === "running" || job.status === "pending") && (
                <div className="progress-bar">
                  <div
                    className="progress-fill"
                    style={{ width: `${Math.round(job.progress * 100)}%` }}
                  />
                </div>
              )}

              {job.error && <p className="queue-error">{job.error}</p>}

              <div className="queue-actions">
                {(job.status === "pending" || job.status === "running") && (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => void handleCancel(job.id)}
                  >
                    取消
                  </button>
                )}
                {job.status === "failed" && (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => void handleRetry(job.id)}
                  >
                    重试
                  </button>
                )}
                {job.status === "done" && job.output_path && (
                  <button
                    type="button"
                    className="btn btn-sm"
                    onClick={() => void handleOpenFolder(job)}
                  >
                    打开文件夹
                  </button>
                )}
                <button
                  type="button"
                  className="btn btn-sm btn-danger"
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
    </section>
  );
}
