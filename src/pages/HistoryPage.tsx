import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import {
  FolderOpen,
  History as HistoryIcon,
  Play,
  RotateCcw,
  Search,
  Trash2,
} from "lucide-react";
import { ConfirmDialog } from "../components/ConfirmDialog";
import {
  DeleteConfirmDialog,
  type DeleteChoice,
} from "../components/DeleteConfirmDialog";
import { EmptyState } from "../components/EmptyState";
import { IconButton } from "../components/IconButton";
import {
  filterHistoryJobs,
  type HistoryStatusFilter,
} from "../lib/historyFilter";
import { api } from "../lib/tauri";
import type { DownloadJob } from "../types";

const STATUS_LABEL: Record<"done" | "failed", string> = {
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

/** History rows are pre-filtered to done/failed by loadJobs; fall back to the raw status otherwise. */
function statusLabel(job: DownloadJob): string {
  return job.status === "done" || job.status === "failed"
    ? STATUS_LABEL[job.status]
    : job.status;
}

interface HistoryPageProps {
  onJobsChanged?: () => void;
  onGoHome?: () => void;
}

export function HistoryPage({ onJobsChanged, onGoHome }: HistoryPageProps) {
  const [jobs, setJobs] = useState<DownloadJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<HistoryStatusFilter>("all");
  const [pendingDelete, setPendingDelete] = useState<DownloadJob | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [bulkBusy, setBulkBusy] = useState(false);
  const bulkBusyRef = useRef(false);
  const historyCount = jobs.length;

  const visible = useMemo(
    () => filterHistoryJobs(jobs, { query, status: statusFilter }),
    [jobs, query, statusFilter],
  );
  const doneCount = jobs.filter((j) => j.status === "done").length;
  const failedCount = jobs.length - doneCount;

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

  async function handleRetry(job: DownloadJob) {
    try {
      const updated = await api.retryJob(job.id);
      setJobs((prev) =>
        prev.map((j) => (j.id === updated.id ? updated : j)).filter(
          (j) => j.status === "done" || j.status === "failed",
        ),
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
      setJobs([]);
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
      setJobs((prev) => prev.filter((j) => j.id !== job.id));
      onJobsChanged?.();
    } catch (err) {
      toast.error(err instanceof Error ? err.message : String(err));
      await loadJobs();
    }
  }

  return (
    <div className="history-page">
      <div className="history-toolbar">
        <div className="history-search">
          <Search size={14} strokeWidth={2} />
          <input
            type="search"
            placeholder="搜索标题或文件名…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <div className="chip-row" role="group" aria-label="状态筛选">
          <button
            type="button"
            className={`chip${statusFilter === "all" ? " on" : ""}`}
            onClick={() => setStatusFilter("all")}
          >
            全部 {jobs.length}
          </button>
          <button
            type="button"
            className={`chip${statusFilter === "done" ? " on" : ""}`}
            onClick={() => setStatusFilter("done")}
          >
            完成 {doneCount}
          </button>
          <button
            type="button"
            className={`chip${statusFilter === "failed" ? " on" : ""}`}
            onClick={() => setStatusFilter("failed")}
          >
            失败 {failedCount}
          </button>
        </div>
        {historyCount > 0 && (
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

      {loading && <p className="queue-empty">加载中…</p>}

      {!loading && jobs.length === 0 && (
        <EmptyState
          icon={HistoryIcon}
          title="还没有下载记录"
          desc="去主页粘贴一个 B 站链接试试"
          actionLabel="去主页"
          onAction={onGoHome}
        />
      )}

      {!loading && jobs.length > 0 && visible.length === 0 && (
        <EmptyState icon={Search} title="没有匹配的记录" desc="换个关键词或清除筛选" />
      )}

      {!loading && visible.length > 0 && (
        <ul className="history-list">
          {visible.map((job) => (
            <li key={job.id} className="history-row">
              <div className="history-row-main">
                <p className="history-row-title">{jobLabel(job)}</p>
                {job.output_path && <p className="history-row-path">{job.output_path}</p>}
                {job.error && <p className="queue-error">{job.error}</p>}
              </div>
              <span className={`queue-badge ${job.status}`}>
                {statusLabel(job)}
              </span>
              <div className="history-row-actions">
                {job.status === "done" && job.output_path && (
                  <>
                    <IconButton icon={Play} label="打开文件" action="open-file" onClick={() => void handleOpenFile(job)} />
                    <IconButton icon={FolderOpen} label="打开文件夹" action="open-folder" onClick={() => void handleOpenFolder(job)} />
                  </>
                )}
                {job.status === "failed" && (
                  <IconButton icon={RotateCcw} label="重试" action="retry-job" onClick={() => void handleRetry(job)} />
                )}
                <IconButton
                  icon={Trash2}
                  label="删除"
                  danger
                  action="delete-job"
                  onClick={() => setPendingDelete(job)}
                />
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
