import { Link2, Loader2 } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import { createResolveGate, type ResolveGate } from "../lib/resolveGate";

interface UrlBarProps {
  url: string;
  loading: boolean;
  error: string | null;
  onUrlChange: (url: string) => void;
  /** Resolves the URL; resolves false when it failed (keeps the URL retryable). */
  onResolve: (url: string) => Promise<boolean>;
}

export function UrlBar({ url, loading, error, onUrlChange, onResolve }: UrlBarProps) {
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Re-dispatch rules live in the gate: same URL skipped while in flight and
  // after a success (focusing the quality select must not re-resolve), but a
  // failed URL stays retryable — it must never become a dead end.
  const gateRef = useRef<ResolveGate | null>(null);
  if (!gateRef.current) {
    gateRef.current = createResolveGate();
  }

  const dispatch = useCallback(
    (nextUrl: string) => {
      const gate = gateRef.current;
      if (!gate || !gate.shouldDispatch(nextUrl)) {
        return;
      }
      void onResolve(nextUrl).then(
        (ok) => gate.settle(nextUrl, ok),
        () => gate.settle(nextUrl, false),
      );
    },
    [onResolve],
  );

  const scheduleResolve = useCallback(
    (nextUrl: string) => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
      const trimmed = nextUrl.trim();
      if (!trimmed) {
        return;
      }
      debounceRef.current = setTimeout(() => dispatch(trimmed), 300);
    },
    [dispatch],
  );

  useEffect(() => {
    return () => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
    };
  }, []);

  function handleChange(value: string) {
    onUrlChange(value);
    if (!value.trim()) {
      // The link is gone: forget both memories so a re-paste resolves again.
      gateRef.current?.clear();
      return;
    }
    scheduleResolve(value);
  }

  function handleRetry() {
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    dispatch(url.trim());
  }

  return (
    <div className="url-bar">
      <label htmlFor="video-url" className="field-label">
        视频链接
      </label>
      <div className={`url-input-row${loading ? " resolving" : ""}`}>
        {loading ? (
          <Loader2 className="url-status-icon" size={16} strokeWidth={2} />
        ) : (
          <Link2 className="url-status-icon" size={16} strokeWidth={2} />
        )}
        <input
          id="video-url"
          className="url-input"
          type="url"
          placeholder="粘贴 B 站视频或 UP 主空间链接，自动解析"
          value={url}
          onChange={(e) => handleChange(e.target.value)}
          onFocus={(e) => {
            if (url.trim()) {
              e.currentTarget.select();
            }
          }}
          onClick={(e) => {
            if (url.trim()) {
              e.currentTarget.select();
            }
          }}
        />
      </div>
      {error && !loading && url.trim() && (
        <p className="url-hint error">
          {error}
          <button
            type="button"
            className="btn btn-sm"
            style={{ marginLeft: "0.5rem" }}
            onClick={handleRetry}
          >
            重试
          </button>
        </p>
      )}
      {!loading && !error && url.trim() && (
        <p className="url-hint">粘贴或修改链接后自动解析</p>
      )}
    </div>
  );
}
