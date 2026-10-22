import { Link2, Loader2 } from "lucide-react";
import { useCallback, useEffect, useRef } from "react";
import {
  EMPTY_RESOLVE_GATE,
  gateInputChange,
  gateResolveOutcome,
  gateSkipAction,
  type ResolveGate,
} from "../lib/resolveGate";
import { isRetryVisible } from "../lib/retryButton";

interface UrlBarProps {
  url: string;
  loading: boolean;
  error: string | null;
  onUrlChange: (url: string) => void;
  /** Resolves the URL; resolves to whether the resolve succeeded. */
  onResolve: (url: string) => Promise<boolean> | boolean;
  /** Drops any in-flight resolve without starting a new one. */
  onDiscard: () => void;
}

export function UrlBar({ url, loading, error, onUrlChange, onResolve, onDiscard }: UrlBarProps) {
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Dedup gate: remembers only URLs whose resolve succeeded, so re-pasting a
  // link after a failure schedules a fresh attempt instead of dead-ending.
  const gateRef = useRef<ResolveGate>(EMPTY_RESOLVE_GATE);
  // URL of the resolve currently in flight, and an attempt counter that makes
  // a superseded outcome inert (it must not rewrite the gate).
  const pendingRef = useRef<string | null>(null);
  const attemptRef = useRef(0);
  // Whether the screen currently shows the remembered URL's result. Starting
  // a resolve clears the card (HomePage clears it), and only a successful
  // outcome puts one back.
  const resultShownRef = useRef(false);
  const urlRef = useRef(url);
  urlRef.current = url;

  const runResolve = useCallback(
    (trimmed: string) => {
      const attempt = ++attemptRef.current;
      pendingRef.current = trimmed;
      resultShownRef.current = false;
      void Promise.resolve(onResolve(trimmed)).then((succeeded) => {
        if (attempt !== attemptRef.current) {
          return;
        }
        pendingRef.current = null;
        resultShownRef.current = succeeded;
        // A late outcome for a URL the input moved away from must not rewrite
        // the gate: the screen no longer shows what this attempt produced.
        if (urlRef.current.trim() !== trimmed) {
          return;
        }
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
      if (schedule) {
        debounceRef.current = setTimeout(() => {
          runResolve(nextUrl.trim());
        }, 300);
        return;
      }
      switch (gateSkipAction(nextUrl, pendingRef.current, resultShownRef.current)) {
        case "rerun":
          // The display moved away from the remembered URL (an in-flight or
          // failed attempt cleared it). Resolving it again also bumps the
          // parent's request id, discarding the other attempt's late result.
          runResolve(nextUrl.trim());
          break;
        case "discard":
          // The input cleared while another resolve is in flight. Make its
          // outcome inert and let the parent retire the spinner.
          attemptRef.current += 1;
          pendingRef.current = null;
          resultShownRef.current = false;
          onDiscard();
          break;
        case "none":
          break;
      }
    },
    [onDiscard, runResolve],
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
          {/* The error stays readable after the input empties, but retry only
              offers itself while there is a URL to retry — see isRetryVisible. */}
          {isRetryVisible(error, url) && (
            <button
              type="button"
              className="btn btn-sm hint-action"
              onClick={handleRetry}
            >
              重试
            </button>
          )}
        </p>
      )}
      {!loading && !error && url.trim() && (
        <p className="url-hint">粘贴或修改链接后自动解析</p>
      )}
    </div>
  );
}
