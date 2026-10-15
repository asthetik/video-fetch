import { Link2, Loader2 } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import {
  EMPTY_RESOLVE_GATE,
  gateInputChange,
  gateResolveOutcome,
  type ResolveGate,
} from "../lib/resolveGate";

interface UrlBarProps {
  url: string;
  loading: boolean;
  error: string | null;
  onUrlChange: (url: string) => void;
  /** Resolves the URL; resolves to whether the resolve succeeded. */
  onResolve: (url: string) => Promise<boolean> | boolean;
}

export function UrlBar({ url, loading, error, onUrlChange, onResolve }: UrlBarProps) {
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Dedup gate: remembers only URLs whose resolve succeeded, so re-pasting a
  // link after a failure schedules a fresh attempt instead of dead-ending.
  const gateRef = useRef<ResolveGate>(EMPTY_RESOLVE_GATE);

  const runResolve = useCallback(
    (trimmed: string) => {
      void Promise.resolve(onResolve(trimmed)).then((succeeded) => {
        gateRef.current = gateResolveOutcome(gateRef.current, trimmed, succeeded);
      });
    },
    [onResolve],
  );

  const scheduleResolve = useCallback(
    (nextUrl: string) => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
      const { schedule, gate } = gateInputChange(gateRef.current, nextUrl);
      gateRef.current = gate;
      if (!schedule) {
        return;
      }
      debounceRef.current = setTimeout(() => {
        runResolve(nextUrl.trim());
      }, 300);
    },
    [runResolve],
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
    scheduleResolve(value);
  }

  function handleRetry() {
    const trimmed = url.trim();
    if (!trimmed) {
      return;
    }
    if (debounceRef.current) {
      clearTimeout(debounceRef.current);
    }
    // Retry bypasses the dedup gate: an error state must always re-run.
    runResolve(trimmed);
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
      {error && !loading && (
        <p className="url-hint error">
          {error}
          <button
            type="button"
            className="btn btn-sm hint-action"
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
