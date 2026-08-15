import { useCallback, useEffect, useRef } from "react";
import { logUi } from "../lib/activityLog";

interface UrlBarProps {
  url: string;
  loading: boolean;
  error: string | null;
  onUrlChange: (url: string) => void;
  onResolve: (url: string) => void;
}

export function UrlBar({ url, loading, error, onUrlChange, onResolve }: UrlBarProps) {
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Avoid re-resolving the same URL (e.g. focus leaving the input for the quality select).
  const lastResolvedRef = useRef<string>("");

  const scheduleResolve = useCallback(
    (nextUrl: string) => {
      if (debounceRef.current) {
        clearTimeout(debounceRef.current);
      }
      const trimmed = nextUrl.trim();
      if (!trimmed || trimmed === lastResolvedRef.current) {
        return;
      }
      debounceRef.current = setTimeout(() => {
        lastResolvedRef.current = trimmed;
        onResolve(trimmed);
      }, 300);
    },
    [onResolve],
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

  return (
    <div className="url-bar">
      <label htmlFor="video-url">视频链接</label>
      <div className="url-input-row">
        <input
          id="video-url"
          className="url-input"
          type="url"
          placeholder="粘贴 B 站视频链接…"
          value={url}
          onChange={(e) => handleChange(e.target.value)}
          onPaste={() => logUi("input", "粘贴链接", "info")}
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
      {loading && <p className="url-hint">正在解析…</p>}
      {error && !loading && <p className="url-hint error">{error}</p>}
      {!loading && !error && url.trim() && (
        <p className="url-hint">粘贴或修改链接后自动解析</p>
      )}
    </div>
  );
}
