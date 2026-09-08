import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  FolderOpen,
  History as HistoryIcon,
  Play,
  RotateCcw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
  DeleteConfirmDialog,
  type DeleteChoice,
} from "../components/DeleteConfirmDialog";
import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import { Segmented } from "../components/Segmented";
import { ThumbTile } from "../components/ThumbTile";
import { formatBytes } from "../lib/format";
import {
  filterHistoryJobs,
  type HistoryStatusFilter,
} from "../lib/historyFilter";
import { groupHistoryByDate } from "../lib/historyGroup";
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
  onGoHome?: () => void;
}

export function HistoryPage({ onJobsChanged, onGoHome }: HistoryPageProps) {
  // History now shows ALL statuses; active jobs form their own pinned group.
  const [allJobs, setAllJobs] = useState<DownloadJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<HistoryStatusFilter>("all");
  const [pendingDelete, setPendingDelete] = useState<DownloadJob | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [bulkBusy, setBulkBusy] = useState(false);
  const bulkBusyRef = useRef(false);

  const loadJobs = useCallback(async () => {
    const list = await api.listJobs();
    // Keep backend order (created_at DESC); no done/failed filtering here.
    setAllJobs(list);
    setLoading(false);
  }, []);

  useEffect(() => {
    void loadJobs();
  }, [loadJobs]);

  const doneCount = allJobs.filter((j) => j.status === "done").length;
  const failedCount = allJobs.filter((j) => j.status === "failed").length;
  const finishedTotal = doneCount + failedCount;

  const activeJobs = allJobs.filter(
    (j) => j.status === "pending" || j.status === "running",
  );
  const finished = useMemo(
    () =>
      filterHistoryJobs(
        allJobs.filter((j) => j.status === "done" || j.status === "failed"),
        { query, status: statusFilter },
      ),
    [allJobs, query, statusFilter],
  );
  const groups = useMemo(() => groupHistoryByDate(finished), [finished]);
  const showActive = activeJobs.length > 0 && statusFilter === "all";

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

  async function handleCancel(id: string) {
    try {
      const updated = await api.cancelJob(id);
      setAllJobs((prev) =>
        prev.map((j) => (j.id === updated.id ? updated : j)),
      );
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleRetry(job: DownloadJob) {
    try {
      const updated = await api.retryJob(job.id);
      // Map-replace only: the retried job turns pending and moves to the
      // pinned "进行中" group instead of being filtered out.
      setAllJobs((prev) =>
        prev.map((j) => (j.id === updated.id ? updated : j)),
      );
      toast.success(`已重新入队：${jobLabel(job)}`);
      onJobsChanged?.();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleClearFinished() {
    if (bulkBusyRef.current) {
      return;
    }
    bulkBusyRef.current = true;
    setBulkBusy(true);
    try {
      await api.clearFinishedJobs();
      setConfirmClear(false);
      // The backend only clears done/failed; keep any active jobs in state.
      setAllJobs((prev) =>
        prev.filter((j) => j.status === "pending" || j.status === "running"),
      );
      toast.success("已清空历史记录");
      onJobsChanged?.();
    } catch (err) {
      setConfirmClear(false);
      toast.error(err instanceof Error ? err.message : String(err));
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

    try {
      await api.deleteJob(job.id, choice === "record_and_file");
      setAllJobs((prev) => prev.filter((j) => j.id !== job.id));
      onJobsChanged?.();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
      await loadJobs();
    }
  }

  function renderRow(job: DownloadJob, active: boolean) {
    return (
      <div className="history-row" key={job.id}>
        <ThumbTile job={job} />
        <div className="history-row-main">
          <p className="history-row-title">{jobLabel(job)}</p>
          <p className="history-row-sub">
            {job.audio_format && (
              <span className="tag fmt">{job.audio_format.toUpperCase()}</span>
            )}
            {job.audio_format === "flac" && <span className="tag hires">Hi-Res</span>}
            {job.file_size && <span>{formatBytes(job.file_size)}</span>}
            {job.error && <span className="history-row-err">{job.error}</span>}
          </p>
          {active && (
            <div className="progress-bar">
              <div
                className="progress-fill"
                style={{ width: `${Math.round(job.progress * 100)}%` }}
              />
            </div>
          )}
        </div>
        <span className={`queue-badge ${job.status}`}>{STATUS_LABEL[job.status]}</span>
        <div className="history-row-actions">
          {active && (
            <IconButton
              icon={X}
              label="取消"
              action="cancel-job"
              onClick={() => void handleCancel(job.id)}
            />
          )}
          {job.status === "failed" && (
            <IconButton
              icon={RotateCcw}
              label="重试"
              action="retry-job"
              onClick={() => void handleRetry(job)}
            />
          )}
          {job.status === "done" && job.output_path && (
            <>
              <IconButton icon={Play} label="打开文件" action="open-file" onClick={() => void handleOpenFile(job)} />
              <IconButton icon={FolderOpen} label="打开文件夹" action="open-folder" onClick={() => void handleOpenFolder(job)} />
            </>
          )}
          <IconButton
            icon={Trash2}
            label="删除"
            danger
            action="delete-job"
            onClick={() => setPendingDelete(job)}
          />
        </div>
      </div>
    );
  }

  return (
    <div className="history-page">
      <div className="history-head">
        <h2 className="history-title">下载历史</h2>
        <span className="history-count">
          {allJobs.length > 0 ? `${allJobs.length} 条记录` : ""}
        </span>
        {finishedTotal > 0 && (
          <button
            type="button"
            className="btn btn-sm btn-danger history-clear"
            data-action="clear-finished"
            disabled={bulkBusy}
            onClick={() => setConfirmClear(true)}
          >
            清空
          </button>
        )}
      </div>

      <div className="history-toolbar">
        <div className="history-search">
          <Search size={14} strokeWidth={2} />
          <input
            type="search"
            aria-label="搜索历史"
            placeholder="搜索标题或文件名…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <Segmented
          options={[
            { value: "all", label: `全部 ${allJobs.length}` },
            { value: "done", label: `完成 ${doneCount}` },
            { value: "failed", label: `失败 ${failedCount}` },
          ]}
          value={statusFilter}
          ariaLabel="状态筛选"
          onChange={(v) => setStatusFilter(v)}
        />
      </div>

      {loading && <p className="queue-empty">加载中…</p>}

      {!loading && allJobs.length === 0 && (
        <EmptyState
          icon={HistoryIcon}
          title="还没有下载记录"
          desc="去主页粘贴一个 B 站链接试试"
          actionLabel="去主页"
          onAction={onGoHome}
        />
      )}

      {!loading && allJobs.length > 0 && !showActive && groups.length === 0 && (
        <EmptyState icon={Search} title="没有匹配的记录" desc="换个关键词或清除筛选" />
      )}

      {showActive && (
        <section className="history-group">
          <p className="history-group-label">进行中</p>
          <div className="history-card">
            {activeJobs.map((job) => renderRow(job, true))}
          </div>
        </section>
      )}

      {groups.map((group) => (
        <section className="history-group" key={group.label}>
          <p className="history-group-label">{group.label}</p>
          <div className="history-card">
            {group.jobs.map((job) => renderRow(job, false))}
          </div>
        </section>
      ))}

      <ConfirmDialog
        open={confirmClear}
        title="清空下载历史"
        message={`将清除 ${finishedTotal} 条历史记录。本地已下载的文件不会被删除。`}
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
