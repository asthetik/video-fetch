import { useCallback, useEffect, useRef, useState } from "react";
import { formatDate, formatDuration, formatPlayCount } from "../lib/spaceFormat";
import { api } from "../lib/tauri";
import type { SpacePage, SpaceVideoItem } from "../types";
import { BatchFormatDialog } from "./BatchFormatDialog";

interface SpaceListViewProps {
  mid: number;
  active?: boolean;
  onEnqueued: () => void;
}

export function SpaceListView({ mid, active = true, onEnqueued }: SpaceListViewProps) {
  const [items, setItems] = useState<SpaceVideoItem[]>([]);
  const [total, setTotal] = useState(0);
  const [degraded, setDegraded] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [uploader, setUploader] = useState("");
  const [keyword, setKeyword] = useState("");
  const [searchInput, setSearchInput] = useState("");
  const [order, setOrder] = useState<"pubdate" | "click">("pubdate");
  const [selected, setSelected] = useState<Map<string, SpaceVideoItem>>(new Map());
  const [loading, setLoading] = useState(true);
  const [moreLoading, setMoreLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [summary, setSummary] = useState<string | null>(null);
  const [failedItems, setFailedItems] = useState<{ bvid: string; error: string }[]>([]);
  const requestIdRef = useRef(0);
  const pageRef = useRef(1);
  // The exact request that failed, so retry re-issues it: retrying a failed
  // load-more must re-append, not replace the accumulated pages with one.
  const failedReqRef = useRef<{ pn: number; append: boolean }>({ pn: 1, append: false });
  const selectAllRef = useRef<HTMLInputElement>(null);
  const summaryRef = useRef<HTMLParagraphElement>(null);

  const loadPage = useCallback(
    async (pn: number, append: boolean, kw: string, ord: "pubdate" | "click") => {
      const requestId = requestIdRef.current + 1;
      requestIdRef.current = requestId;
      if (append) {
        setMoreLoading(true);
      } else {
        setLoading(true);
        setError(null);
      }
      try {
        const page: SpacePage = await api.spaceListVideos(mid, pn, kw, ord);
        if (requestIdRef.current !== requestId) return;
        pageRef.current = pn;
        setItems((prev) => (append ? [...prev, ...page.items] : page.items));
        setTotal(page.total);
        setDegraded(page.degraded);
        setHasMore(page.has_more);
      } catch (err) {
        if (requestIdRef.current !== requestId) return;
        failedReqRef.current = { pn, append };
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (requestIdRef.current === requestId) {
          setLoading(false);
          setMoreLoading(false);
        }
      }
    },
    [mid],
  );

  useEffect(() => {
    let cancelled = false;
    pageRef.current = 1;
    setItems([]);
    setSummary(null);
    setFailedItems([]);
    void loadPage(1, false, keyword, order);
    void api
      .spaceInfo(mid)
      .then((info) => {
        if (!cancelled) setUploader(info.name);
      })
      .catch(() => {
        // Uploader name is cosmetic; the list request reports real failures.
      });
    return () => {
      cancelled = true;
    };
  }, [mid, keyword, order, loadPage]);

  useEffect(() => {
    // A tab switch hides the dialog via `open={dialogOpen && active}`; clear
    // the flag too so returning to this page does not silently re-open it.
    if (!active) {
      setDialogOpen(false);
    }
  }, [active]);

  function toggleSelect(item: SpaceVideoItem) {
    setSelected((prev) => {
      const next = new Map(prev);
      if (next.has(item.bvid)) {
        next.delete(item.bvid);
      } else {
        next.set(item.bvid, item);
      }
      return next;
    });
  }

  function toggleSelectVisible() {
    setSelected((prev) => {
      const next = new Map(prev);
      const allSelected = items.every((i) => next.has(i.bvid));
      for (const item of items) {
        if (allSelected) {
          next.delete(item.bvid);
        } else {
          next.set(item.bvid, item);
        }
      }
      return next;
    });
  }

  function submitSearch(value: string) {
    setKeyword(value.trim());
  }

  async function handleBatchSubmit(formatId: string, audioFormat: string | null) {
    setDialogOpen(false);
    setSummary(null);
    setFailedItems([]);
    try {
      const result = await api.spaceEnqueueBatch({
        items: [...selected.values()].map((i) => ({ bvid: i.bvid, title: i.title })),
        format_id: formatId,
        audio_format: audioFormat,
      });
      const failedNote =
        result.failed.length > 0 ? `，失败 ${result.failed.length} 个` : "";
      setSummary(
        `已入队 ${result.enqueued} 个，跳过已存在 ${result.skipped_existing} 个，队列重复 ${result.skipped_active} 个${failedNote}`,
      );
      setFailedItems(result.failed);
      setSelected(new Map());
      onEnqueued();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  const allVisibleSelected = items.length > 0 && items.every((i) => selected.has(i.bvid));

  useEffect(() => {
    // The summary renders below a potentially very long list; pull it into view.
    if (summary) {
      summaryRef.current?.scrollIntoView({ block: "nearest" });
    }
  }, [summary]);

  useEffect(() => {
    // "indeterminate" is a DOM property, not an attribute; set it imperatively.
    if (selectAllRef.current) {
      selectAllRef.current.indeterminate = selected.size > 0 && !allVisibleSelected;
    }
  }, [selected, allVisibleSelected]);

  return (
    <div className="space-list-block">
      <div className="space-header">
        <h2 className="page-title">
          {uploader ? `${uploader} · ` : ""}
          {degraded ? `已加载 ${items.length} 个视频` : `共 ${total} 个视频`}
        </h2>
      </div>

      {degraded && (
        <p className="space-degraded">降级模式：默认排序，不支持搜索</p>
      )}

      <div className="space-toolbar">
        <div className="mode-toggle" role="group" aria-label="排序">
          {(["pubdate", "click"] as const).map((value) => (
            <button
              key={value}
              type="button"
              className={order === value ? "mode-segment active" : "mode-segment"}
              aria-pressed={order === value}
              disabled={degraded}
              onClick={() => setOrder(value)}
            >
              {value === "pubdate" ? "最新发布" : "最多播放"}
            </button>
          ))}
        </div>
        <input
          className="space-search"
          type="search"
          placeholder="空间内搜索…（回车触发）"
          value={searchInput}
          disabled={degraded}
          onChange={(e) => setSearchInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submitSearch(e.currentTarget.value);
          }}
        />
      </div>

      {loading && <p className="loading-text">正在获取视频列表…</p>}
      {error && (
        <p className="url-hint error">
          {error}
          <button
            type="button"
            className="btn btn-sm"
            style={{ marginLeft: "0.5rem" }}
            onClick={() => {
            const failed = failedReqRef.current;
            void loadPage(failed.pn, failed.append, keyword, order);
          }}
          >
            重试
          </button>
        </p>
      )}
      {!loading && !error && items.length === 0 && !keyword && <p className="queue-empty">暂无视频</p>}
      {!loading && !error && items.length === 0 && keyword && (
        <p className="queue-empty">
          没有匹配「{keyword}」的视频
          <button
            type="button"
            className="btn btn-sm"
            style={{ marginLeft: "0.5rem" }}
            onClick={() => {
              setKeyword("");
              setSearchInput("");
            }}
          >
            清除搜索
          </button>
        </p>
      )}

      {items.length > 0 && (
        <ul className="space-list">
          <li className="space-row space-row-head">
            <label className="space-select-all">
              <input
                ref={selectAllRef}
                type="checkbox"
                checked={allVisibleSelected}
                onChange={toggleSelectVisible}
              />
              全选（已加载 {items.length} 项）
            </label>
          </li>
          {items.map((item) => (
            <li
              key={item.bvid}
              className={selected.has(item.bvid) ? "space-row selected" : "space-row"}
              data-action="space-row"
              onClick={() => toggleSelect(item)}
            >
              <input
                type="checkbox"
                checked={selected.has(item.bvid)}
                onChange={() => toggleSelect(item)}
                onClick={(e) => e.stopPropagation()}
              />
              {item.cover && <img className="space-thumb" src={item.cover} alt="" loading="lazy" />}
              <div className="space-row-main">
                <p className="space-title">{item.title}</p>
                <p className="space-meta">
                  <span>{formatDuration(item.duration_secs)}</span>
                  <span>{item.play === null ? "—" : formatPlayCount(item.play)}</span>
                  <span>{formatDate(item.pubdate)}</span>
                </p>
              </div>
            </li>
          ))}
        </ul>
      )}

      {!loading && !error && hasMore && (
        <div className="space-more">
          <button
            type="button"
            className="btn"
            disabled={moreLoading}
            onClick={() => void loadPage(pageRef.current + 1, true, keyword, order)}
          >
            {moreLoading
              ? "加载中…"
              : degraded
                ? `加载更多（已加载 ${items.length}）`
                : `加载更多（已加载 ${items.length}/${total}）`}
          </button>
        </div>
      )}

      {selected.size > 0 && (
        <>
          <div className="space-batch-spacer" />
          <div className="space-batch-bar">
            <span>已选 {selected.size} 个</span>
            <div className="space-batch-actions">
              <button type="button" className="btn btn-sm" onClick={() => setSelected(new Map())}>
                清空选择
              </button>
              <button
                type="button"
                className="btn btn-primary"
                data-action="space-batch-download"
                onClick={() => setDialogOpen(true)}
              >
                下载已选
              </button>
            </div>
          </div>
        </>
      )}

      {summary && (
        <p className="url-hint" ref={summaryRef}>
          {summary}
        </p>
      )}
      {failedItems.length > 0 && (
        <details className="space-failed-details">
          <summary>失败明细（{failedItems.length}）</summary>
          <ul>
            {failedItems.map((f) => (
              <li key={f.bvid}>
                {f.bvid}：{f.error}
              </li>
            ))}
          </ul>
        </details>
      )}

      <BatchFormatDialog
        open={dialogOpen && active}
        count={selected.size}
        onCancel={() => setDialogOpen(false)}
        onSubmit={(formatId, audioFormat) => void handleBatchSubmit(formatId, audioFormat)}
      />
    </div>
  );
}
