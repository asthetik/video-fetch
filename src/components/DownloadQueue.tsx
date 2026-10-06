import { useCallback, useEffect, useRef, useState } from "react";
import { AnimatePresence, motion } from "motion/react";
import {
  Download,
  FolderOpen,
  History as HistoryIcon,
  Pause,
  Play,
  RotateCcw,
  Trash2,
  X,
} from "lucide-react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import { ConfirmDialog } from "./ConfirmDialog";
import {
  DeleteConfirmDialog,
  type DeleteChoice,
} from "./DeleteConfirmDialog";
import { IconButton } from "./IconButton";
import { api } from "../lib/tauri";
import { formatBytes } from "../lib/format";
import {
  isActiveStatus,
  partitionQueueJobs,
  sortJobs,
  upsertJob,
} from "../lib/queueJobs";
import {
  type DownloadProgressPayload,
  mergeJob,
} from "../lib/downloadProgress";
import { formatDuration } from "../lib/spaceFormat";
import type { DownloadJob, JobStatus } from "../types";

const STATUS_LABEL: Record<JobStatus, string> = {
  pending: "等待中",
  running: "下载中",
  done: "完成",
  failed: "失败",
  paused: "已暂停",
};

function formatSpeed(bps?: number | null): string | null {
  if (bps == null || !Number.isFinite(bps) || bps <= 0) {
    return null;
  }
  return `${formatBytes(bps)}/s`;
}

function formatEta(seconds?: number | null): string | null {
  if (seconds == null || !Number.isFinite(seconds) || seconds <= 0) {
    return null;
  }
  return formatDuration(Math.round(seconds));
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
  onOpenHistory: () => void;
}

export function DownloadQueue({
  refreshToken,
  onOpenHistory,
}: DownloadQueueProps) {
  const [jobs, setJobs] = useState<DownloadJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [pendingDelete, setPendingDelete] = useState<DownloadJob | null>(null);
  const [pendingPauseCancel, setPendingPauseCancel] = useState<DownloadJob | null>(null);
  const [confirmCancelAll, setConfirmCancelAll] = useState(false);
  const [bulkBusy, setBulkBusy] = useState(false);
  const bulkBusyRef = useRef(false);

  const loadJobs = useCallback(async () => {
    try {
      const list = await api.listJobs();
      setJobs(sortJobs(list));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
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
        if (idx < 0) {
          void loadJobs();
          return prev;
        }
        return upsertJob(prev, mergeJob(prev[idx], patch));
      });
    }).then((fn) => {
      unlisten = fn;
    });

    return () => {
      unlisten?.();
    };
  }, [loadJobs]);

  async function handleCancel(id: string) {
    try {
      const updated = await api.cancelJob(id);
      setJobs((prev) => upsertJob(prev, updated));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleRetry(id: string) {
    try {
      const updated = await api.retryJob(id);
      setJobs((prev) => upsertJob(prev, updated));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }

  async function handlePause(id: string) {
    try {
      const updated = await api.pauseJob(id);
      setJobs((prev) => upsertJob(prev, updated));
      if (updated.status !== "paused") {
        toast.info("任务已结束，未执行暂停");
      }
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleResume(id: string) {
    try {
      const updated = await api.resumeJob(id);
      setJobs((prev) => upsertJob(prev, updated));
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }

  // Shared skeleton for the three bulk actions: busy guard, partial-failure
  // warning, error toast, refresh. Callers keep only their api call and text.
  async function runBulk<T extends { errors?: string[] }>(
    run: () => Promise<T>,
    warning: (result: T) => string,
  ) {
    if (bulkBusyRef.current) {
      return;
    }
    bulkBusyRef.current = true;
    setBulkBusy(true);
    try {
      const result = await run();
      if (result.errors && result.errors.length > 0) {
        toast.warning(warning(result));
      }
      await loadJobs();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    } finally {
      bulkBusyRef.current = false;
      setBulkBusy(false);
    }
  }

  async function handlePauseAll() {
    await runBulk(
      () => api.pauseAllJobs(),
      (r) => `已暂停 ${r.paused} 个任务，部分失败：${r.errors?.[0]}`,
    );
  }

  async function handleResumeAll() {
    await runBulk(
      () => api.resumeAllJobs(),
      (r) => `已继续 ${r.resumed} 个任务，部分失败：${r.errors?.[0]}`,
    );
  }

  async function applyDelete(job: DownloadJob, choice: DeleteChoice) {
    setPendingDelete(null);
    if (choice === "cancel") {
      return;
    }

    try {
      await api.deleteJob(job.id, choice === "record_and_file");
      setJobs((prev) => prev.filter((j) => j.id !== job.id));
      await loadJobs();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
      await loadJobs();
    }
  }

  async function handleOpenFolder(job: DownloadJob) {
    if (!job.output_path) {
      return;
    }
    await api.openPath(parentDir(job.output_path));
  }

  async function handleCancelAll() {
    await runBulk(
      () => api.cancelAllJobs(),
      (r) => `已取消 ${r.cancelled} 个任务，部分失败：${r.errors?.[0]}`,
    );
    setConfirmCancelAll(false);
  }

  const { active, recentFailed, failedTotal, doneFallback } =
    partitionQueueJobs(jobs);
  const cancellableCount = active.length;
  const pausableCount = jobs.filter(
    (j) => j.status === "pending" || j.status === "running",
  ).length;
  const resumableCount = jobs.filter((j) => j.status === "paused").length;

  function renderJobItem(job: DownloadJob) {
    const speed = job.status === "running" ? formatSpeed(job.speed) : null;
    const eta = job.status === "running" ? formatEta(job.eta) : null;
    const bytes =
      job.status === "running" &&
      job.downloaded_bytes != null &&
      job.total_bytes != null &&
      job.total_bytes > 0
        ? `${formatBytes(job.downloaded_bytes)} / ${formatBytes(job.total_bytes)}`
        : null;
    const pct = Math.round(job.progress * 100);
    return (
      <motion.li
        key={job.id}
        layout
        initial={{ opacity: 0, y: -6 }}
        animate={{ opacity: 1, y: 0 }}
        exit={{ opacity: 0, y: -6 }}
        transition={{ duration: 0.15 }}
        className="queue-item"
      >
        <div className="queue-main">
          <div className="queue-item-header">
            <p className="queue-title">
              {job.title}
              {job.page_index > 1 ? ` · P${job.page_index}` : ""}
            </p>
            <span className={`queue-badge ${job.status}`}>
              {STATUS_LABEL[job.status]}
              {job.status === "running" && ` ${pct}%`}
              {job.status === "paused" && job.progress > 0 && ` ${pct}%`}
            </span>
          </div>

          {(job.status === "running" ||
            job.status === "pending" ||
            (job.status === "paused" && job.progress > 0)) && (
            <div className="progress-bar">
              <div className="progress-fill" style={{ width: `${pct}%` }} />
            </div>
          )}

          {job.error && <p className="queue-error">{job.error}</p>}

          <div className="queue-actions">
            {(job.status === "pending" || job.status === "running") && (
              <>
                <IconButton
                  icon={Pause}
                  label="暂停"
                  action="pause-job"
                  onClick={() => void handlePause(job.id)}
                />
                <IconButton
                  icon={X}
                  label="取消"
                  action="cancel-job"
                  onClick={() => void handleCancel(job.id)}
                />
              </>
            )}
            {job.status === "paused" && (
              <>
                <IconButton
                  icon={Play}
                  label="继续"
                  action="resume-job"
                  onClick={() => void handleResume(job.id)}
                />
                <IconButton
                  icon={X}
                  label="取消"
                  action="cancel-job"
                  onClick={() => setPendingPauseCancel(job)}
                />
              </>
            )}
            {job.status === "failed" && (
              <IconButton
                icon={RotateCcw}
                label="重试"
                action="retry-job"
                onClick={() => void handleRetry(job.id)}
              />
            )}
            {job.status === "done" && job.output_path && (
              <IconButton
                icon={FolderOpen}
                label="打开文件夹"
                onClick={() => void handleOpenFolder(job)}
              />
            )}
            <IconButton
              icon={Trash2}
              label="删除"
              danger
              onClick={() => {
                setPendingDelete(job);
              }}
            />
          </div>
        </div>
        {(speed || eta || bytes) && (
          <div className="queue-data">
            {speed && <b>{speed}</b>}
            {bytes && <span>{bytes}</span>}
            {eta && <span>剩余 {eta}</span>}
          </div>
        )}
      </motion.li>
    );
  }

  return (
    <section className="download-queue">
      <div className="section-heading">
        <h3 className="queue-heading">
          <Download size={14} strokeWidth={2} />
          下载队列
          {cancellableCount > 0 && (
            <span className="queue-count">{cancellableCount}</span>
          )}
        </h3>
        <div className="queue-heading-actions">
          {pausableCount > 0 && (
            <button
              type="button"
              className="btn btn-sm"
              data-action="pause-all"
              disabled={bulkBusy}
              onClick={() => void handlePauseAll()}
            >
              全部暂停
            </button>
          )}
          {resumableCount > 0 && (
            <button
              type="button"
              className="btn btn-sm"
              data-action="resume-all"
              disabled={bulkBusy}
              onClick={() => void handleResumeAll()}
            >
              全部继续
            </button>
          )}
          {cancellableCount > 0 && (
            <button
              type="button"
              className="btn btn-sm"
              data-action="cancel-all"
              disabled={bulkBusy}
              onClick={() => {
                setConfirmCancelAll(true);
              }}
            >
              全部取消
            </button>
          )}
          <IconButton
            icon={HistoryIcon}
            label="查看历史"
            onClick={onOpenHistory}
          />
        </div>
      </div>
      {loading && <p className="queue-empty">加载中…</p>}
      {!loading &&
        active.length === 0 &&
        recentFailed.length === 0 &&
        !doneFallback?.length && (
          <p className="queue-empty">暂无下载任务</p>
        )}
      {active.length > 0 && (
        <ul className="queue-list">
          <AnimatePresence initial={false}>{active.map(renderJobItem)}</AnimatePresence>
        </ul>
      )}
      {recentFailed.length > 0 && (
        <>
          <div className="queue-section-heading">
            <p className="queue-section-label">最近失败</p>
            <button
              type="button"
              className="btn-text queue-section-link"
              onClick={onOpenHistory}
            >
              共 {failedTotal} 条 · 在历史查看
            </button>
          </div>
          <ul className="queue-list">
            <AnimatePresence initial={false}>{recentFailed.map(renderJobItem)}</AnimatePresence>
          </ul>
        </>
      )}
      {doneFallback && doneFallback.length > 0 && (
        <>
          {recentFailed.length > 0 && <p className="queue-section-label">最近完成</p>}
          <ul className="queue-list">
            <AnimatePresence initial={false}>{doneFallback.map(renderJobItem)}</AnimatePresence>
          </ul>
        </>
      )}

      <ConfirmDialog
        open={confirmCancelAll}
        title="取消全部下载"
        message={`将取消 ${cancellableCount} 个未完成的任务（含暂停中的）；未完成任务的已下载部分会被删除，已保存到本地的成品不受影响。`}
        confirmLabel="全部取消"
        cancelLabel="关闭"
        busy={bulkBusy}
        onCancel={() => {
          if (!bulkBusy) setConfirmCancelAll(false);
        }}
        onConfirm={() => void handleCancelAll()}
      />

      <ConfirmDialog
        open={pendingPauseCancel !== null}
        title="取消暂停的任务"
        message="将取消该暂停任务，已下载的部分会被删除。"
        confirmLabel="取消任务"
        cancelLabel="关闭"
        danger
        onCancel={() => setPendingPauseCancel(null)}
        onConfirm={() => {
          const job = pendingPauseCancel;
          setPendingPauseCancel(null);
          if (job) {
            void handleCancel(job.id);
          }
        }}
      />

      <DeleteConfirmDialog
        open={pendingDelete !== null}
        jobTitle={pendingDelete ? jobLabel(pendingDelete) : ""}
        filePath={pendingDelete?.output_path}
        note={
          pendingDelete && isActiveStatus(pendingDelete.status)
            ? "已下载的部分会被删除。"
            : null
        }
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
