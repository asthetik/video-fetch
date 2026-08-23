import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  type ResolveFormatsFailedEvent,
  type ResolveMetaEvent,
} from "../lib/tauri";
import { DownloadQueue } from "../components/DownloadQueue";
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
  const [loading, setLoading] = useState(false);
  const [formatsLoading, setFormatsLoading] = useState(false);
  const [formatsError, setFormatsError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
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
      setError(null);
      setFormatsError(null);
      setFormatsLoading(true);
      if (clearExisting) {
        setMeta(null);
      }
      try {
        const result = await api.resolveUrl(nextUrl, force, requestId);
        if (requestIdRef.current !== requestId) {
          return;
        }
        setMeta(result);
        if (result.formats.length > 0) {
          setFormatsLoading(false);
          setFormatsError(null);
        }
        // Empty formats: leave formatsLoading/formatsError to complete / formats_failed events.
      } catch (err) {
        if (requestIdRef.current !== requestId) {
          return;
        }
        if (!metaRef.current) {
          setFormatsLoading(false);
          setError(err instanceof Error ? err.message : String(err));
        }
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
      setFormatsLoading(true);
      setFormatsError(null);
      setError(null);
      setLoading(false);
    }).then((fn) => unlisteners.push(fn));

    void listen<ResolveMetaEvent>("resolve://formats_progress", (event) => {
      const { request_id, meta: progress } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setMeta(progress);
      setFormatsLoading(true);
    }).then((fn) => unlisteners.push(fn));

    void listen<ResolveMetaEvent>("resolve://complete", (event) => {
      const { request_id, meta: complete } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setMeta(complete);
      setFormatsLoading(false);
      setFormatsError(null);
      setLoading(false);
    }).then((fn) => unlisteners.push(fn));

    void listen<ResolveFormatsFailedEvent>("resolve://formats_failed", (event) => {
      const { request_id, error: message } = event.payload;
      if (request_id !== requestIdRef.current) {
        return;
      }
      setFormatsLoading(false);
      setFormatsError(message);
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

  function handleEnqueued() {
    onQueueRefresh();
  }

  return (
    <div className="home-page">
      <div className="home-toolbar">
        <UrlBar
          url={url}
          loading={loading}
          error={error}
          onUrlChange={setUrl}
          onResolve={(next) => void handleResolve(next)}
        />
      </div>

      {loading && !meta && <p className="loading-text">正在获取视频信息…</p>}

      {meta && (
        <VideoCard
          meta={meta}
          url={url.trim()}
          active={active}
          refreshing={loading}
          formatsLoading={formatsLoading}
          formatsError={formatsError}
          onEnqueued={handleEnqueued}
          onRefresh={() => void handleResolve(url.trim(), true)}
        />
      )}

      <DownloadQueue
        refreshToken={queueRefresh}
        onOpenHistory={onOpenHistory}
      />
    </div>
  );
}
