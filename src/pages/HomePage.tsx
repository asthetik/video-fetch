import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/tauri";
import { DownloadQueue } from "../components/DownloadQueue";
import { UrlBar } from "../components/UrlBar";
import { VideoCard } from "../components/VideoCard";
import type { AuthStatus as AuthStatusType, VideoMeta } from "../types";

function hasUsableCookies(status: AuthStatusType): boolean {
  return status === "logged_in";
}

export function HomePage() {
  const [url, setUrl] = useState("");
  const [meta, setMeta] = useState<VideoMeta | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [authStatus, setAuthStatus] = useState<AuthStatusType>("logged_out");
  const [queueRefresh, setQueueRefresh] = useState(0);

  const urlRef = useRef(url);
  const metaRef = useRef(meta);
  const authRef = useRef(authStatus);
  urlRef.current = url;
  metaRef.current = meta;
  authRef.current = authStatus;

  const handleResolve = useCallback(
    async (nextUrl: string, force = false, clearExisting = !force) => {
      setLoading(true);
      setError(null);
      if (clearExisting) {
        setMeta(null);
      }
      try {
        const result = await api.resolveUrl(nextUrl, force);
        setMeta(result);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
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
    let unlisten: (() => void) | undefined;
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
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [handleResolve]);

  function handleEnqueued() {
    setQueueRefresh((n) => n + 1);
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
          authStatus={authStatus}
          refreshing={loading}
          onEnqueued={handleEnqueued}
          onRefresh={() => void handleResolve(url.trim(), true)}
        />
      )}

      <DownloadQueue refreshToken={queueRefresh} />
    </div>
  );
}
