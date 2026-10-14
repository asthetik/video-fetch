import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { toast } from "sonner";
import {
  api,
  type ResolveFormatsFailedEvent,
  type ResolveMetaEvent,
} from "../lib/tauri";
import {
  initialResolveStatus,
  reduceResolveStatus,
  type ResolveStatus,
} from "../lib/resolveStatus";
import { DownloadQueue } from "../components/DownloadQueue";
import { SpaceListView } from "../components/SpaceListView";
import { UrlBar } from "../components/UrlBar";
import { VideoCard } from "../components/VideoCard";
import type { AuthStatus as AuthStatusType, VideoMeta } from "../types";

function hasUsableCookies(status: AuthStatusType): boolean {
  return status === "logged_in";
}

interface HomePageProps {
  queueRefresh: number;
  onQueueRefresh: () => void;
  onOpenHistory: () => void;
  active?: boolean;
}

export function HomePage({
  queueRefresh,
  onQueueRefresh,
  onOpenHistory,
  active = true,
}: HomePageProps) {
  const [url, setUrl] = useState("");
  const [meta, setMeta] = useState<VideoMeta | null>(null);
  const [space, setSpace] = useState<{ mid: number } | null>(null);
  const [loading, setLoading] = useState(false);
  const [resolveStatus, setResolveStatus] =
    useState<ResolveStatus>(initialResolveStatus);
  const [authStatus, setAuthStatus] = useState<AuthStatusType>("logged_out");

  const urlRef = useRef(url);
  const metaRef = useRef(meta);
  const authRef = useRef(authStatus);
  const requestIdRef = useRef(0);
  urlRef.current = url;
  metaRef.current = meta;
  authRef.current = authStatus;

  const handleResolve = useCallback(
    async (nextUrl: string, force = false, clearExisting = !force) => {
      const requestId = requestIdRef.current + 1;
      requestIdRef.current = requestId;
      setLoading(true);
      setResolveStatus((s) => reduceResolveStatus(s, { type: "resolve_started" }));
      if (clearExisting) {
        setMeta(null);
      }
      try {
        const kind = await api.detectUrl(nextUrl);
        if (requestIdRef.current !== requestId) {
          return;
        }
        if (kind.kind === "space") {
          setSpace({ mid: kind.mid });
          setMeta(null);
          setResolveStatus((s) => reduceResolveStatus(s, { type: "settled_idle" }));
          setLoading(false);
          return;
        }
        if (kind.kind === "invalid_space") {
          setSpace(null);
          setMeta(null);
          setResolveStatus((s) =>
            reduceResolveStatus(s, {
              type: "settled_page_error",
              message: "无效的空间链接",
            }),
          );
          setLoading(false);
          return;
        }
        setSpace(null);
        const result = await api.resolveUrl(nextUrl, force, requestId);
        if (requestIdRef.current !== requestId) {
          return;
        }
        setMeta(result);
        if (result.formats.length > 0) {
          setResolveStatus((s) => reduceResolveStatus(s, { type: "complete" }));
        }
        // Empty formats: leave formatsLoading/formatsError to complete / formats_failed events.
      } catch (err) {
        if (requestIdRef.current !== requestId) {
          return;
        }
        // Unconditional: a rejected resolve always ends the formats spinner and
        // folds the message into formatsError (in-card retry), even when a card
        // is already rendered. Only the page-level error needs to know about
        // the card — without one, the URL bar is where the message is read.
        const message = err instanceof Error ? err.message : String(err);
        setResolveStatus((s) =>
          reduceResolveStatus(s, {
            type: "resolve_rejected",
            message,
            hasCard: metaRef.current !== null,
          }),
        );
      } finally {
        if (requestIdRef.current === requestId) {
          setLoading(false);
        }
      }
    },
    [],
  );

  useEffect(() => {
    void api.getAuthStatus().then((status) => {
      setAuthStatus(status);
      authRef.current = status;
    });
  }, []);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];

    void listen<ResolveMetaEvent>("resolve://partial", (event) => {
      const { request_id, meta: partial } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setMeta(partial);
      setResolveStatus((s) => reduceResolveStatus(s, { type: "partial" }));
      setLoading(false);
    }).then((fn) => unlisteners.push(fn));

    void listen<ResolveMetaEvent>("resolve://formats_progress", (event) => {
      const { request_id, meta: progress } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setMeta(progress);
      setResolveStatus((s) =>
        reduceResolveStatus(s, { type: "formats_progress" }),
      );
    }).then((fn) => unlisteners.push(fn));

    void listen<ResolveMetaEvent>("resolve://complete", (event) => {
      const { request_id, meta: complete } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setMeta(complete);
      setResolveStatus((s) => reduceResolveStatus(s, { type: "complete" }));
      setLoading(false);
    }).then((fn) => unlisteners.push(fn));

    void listen<ResolveFormatsFailedEvent>("resolve://formats_failed", (event) => {
      const { request_id, error: message } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setResolveStatus((s) =>
        reduceResolveStatus(s, { type: "formats_failed", message }),
      );
      setLoading(false);
    }).then((fn) => unlisteners.push(fn));

    void listen<AuthStatusType>("auth://status", (event) => {
      const next = event.payload;
      const prev = authRef.current;
      setAuthStatus(next);
      authRef.current = next;
      // Resolve cache is keyed by cookie scope (logged_in vs guest); re-resolve when usability flips.
      if (
        hasUsableCookies(prev) !== hasUsableCookies(next) &&
        metaRef.current &&
        urlRef.current.trim()
      ) {
        // Prefer new-scope cache; do not clear card while loading.
        void handleResolve(urlRef.current.trim(), false, false);
      }
    }).then((fn) => unlisteners.push(fn));

    return () => {
      for (const fn of unlisteners) {
        fn();
      }
    };
  }, [handleResolve]);

  function handleEnqueued(count: number) {
    onQueueRefresh();
    // All-skipped batch: queue refresh is still needed, but a "queued" toast would lie.
    if (count > 0) {
      toast.success(count > 1 ? `已加入下载队列 · ${count} 项` : "已加入下载队列");
    }
  }

  return (
    <div className="home-page">
      <UrlBar
        url={url}
        loading={loading}
        error={resolveStatus.error}
        onUrlChange={setUrl}
        onResolve={(next) => void handleResolve(next)}
      />

      {loading && !meta && !space && <p className="loading-text">正在获取视频信息…</p>}

      {space ? (
        <SpaceListView
          key={space.mid}
          mid={space.mid}
          active={active}
          onEnqueued={handleEnqueued}
        />
      ) : (
        meta && (
          <VideoCard
            meta={meta}
            url={url.trim()}
            active={active}
            refreshing={loading}
            formatsLoading={resolveStatus.formatsLoading}
            formatsError={resolveStatus.formatsError}
            onEnqueued={handleEnqueued}
            onRefresh={() => void handleResolve(url.trim(), true)}
          />
        )
      )}

      <DownloadQueue refreshToken={queueRefresh} onOpenHistory={onOpenHistory} />
    </div>
  );
}
