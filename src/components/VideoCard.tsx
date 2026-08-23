import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/tauri";
import { logUi } from "../lib/activityLog";
import type { AuthStatus, FormatOption, VideoMeta } from "../types";

/** Highest resolution first, then highest bitrate within the same resolution. */
function sortFormats(formats: FormatOption[]): FormatOption[] {
  return [...formats].sort((a, b) => {
    const heightDiff = (b.height ?? 0) - (a.height ?? 0);
    if (heightDiff !== 0) {
      return heightDiff;
    }
    return (b.tbr ?? 0) - (a.tbr ?? 0);
  });
}

function pickDefaultFormat(formats: FormatOption[]): FormatOption | null {
  const sorted = sortFormats(formats);
  return sorted[0] ?? null;
}

interface VideoCardProps {
  meta: VideoMeta;
  url: string;
  authStatus: AuthStatus;
  onEnqueued: () => void;
  onRefresh: () => void;
  refreshing?: boolean;
  formatsLoading?: boolean;
  formatsError?: string | null;
}

export function VideoCard({
  meta,
  url,
  authStatus,
  onEnqueued,
  onRefresh,
  refreshing = false,
  formatsLoading = false,
  formatsError = null,
}: VideoCardProps) {
  const [selectedFormatId, setSelectedFormatId] = useState("");
  const [mode, setMode] = useState<"video" | "audio">("video");
  const [selectedAudioId, setSelectedAudioId] = useState("");
  const [audioFormat, setAudioFormat] = useState<"m4a" | "mp3" | "flac">("m4a");
  const [selectedPages, setSelectedPages] = useState<Set<number>>(new Set());
  const [downloading, setDownloading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const prevVideoIdRef = useRef<string | null>(null);

  const sortedFormats = useMemo(() => sortFormats(meta.formats), [meta.formats]);
  const sortedAudioFormats = useMemo(
    () => sortFormats(meta.audio_formats),
    [meta.audio_formats],
  );
  const audioIsHires =
    sortedAudioFormats.find((f) => f.format_id === selectedAudioId)?.hires ??
    false;

  useEffect(() => {
    if (!audioIsHires && audioFormat === "flac") {
      setAudioFormat("m4a");
    }
  }, [audioIsHires, audioFormat]);

  useEffect(() => {
    const videoChanged = prevVideoIdRef.current !== meta.id;
    prevVideoIdRef.current = meta.id;
    if (videoChanged) {
      setMode("video");
      setAudioFormat("m4a");
      setSelectedPages(new Set(meta.pages.map((p) => p.index)));
      setError(null);
      setInfo(null);
    }
    setSelectedFormatId((prev) => {
      if (!videoChanged && prev && meta.formats.some((f) => f.format_id === prev)) {
        return prev;
      }
      return pickDefaultFormat(meta.formats)?.format_id ?? "";
    });
    setSelectedAudioId((prev) => {
      if (!videoChanged && prev && meta.audio_formats.some((f) => f.format_id === prev)) {
        return prev;
      }
      return pickDefaultFormat(meta.audio_formats)?.format_id ?? "";
    });
  }, [meta]);

  const showLoginHint = authStatus === "logged_out";

  const allSelected = selectedPages.size === meta.pages.length;
  const noneSelected = selectedPages.size === 0;

  function togglePage(index: number) {
    setSelectedPages((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  }

  function selectAll() {
    setSelectedPages(new Set(meta.pages.map((p) => p.index)));
  }

  function selectNone() {
    setSelectedPages(new Set());
  }

  async function handleDownload() {
    const selectedId = mode === "audio" ? selectedAudioId : selectedFormatId;
    if (!selectedId || selectedPages.size === 0) {
      setError(mode === "audio" ? "请选择音质并至少勾选一个分 P" : "请选择清晰度并至少勾选一个分 P");
      setInfo(null);
      return;
    }

    setDownloading(true);
    setError(null);
    setInfo(null);
    try {
      const pageIndexes = [...selectedPages].sort((a, b) => a - b);
      const chosen =
        mode === "audio"
          ? sortedAudioFormats.find((f) => f.format_id === selectedAudioId)?.label ??
            selectedAudioId
          : sortedFormats.find((f) => f.format_id === selectedFormatId)?.label ??
            selectedFormatId;
      const container =
        mode === "audio"
          ? audioFormat === "flac" && !audioIsHires
            ? "m4a"
            : audioFormat
          : null;
      const mediaLabel = mode === "audio" ? "音频" : "视频";
      logUi(
        "download",
        `点击下载（${mode === "audio" ? "音频" : "视频"}，${chosen}，${pageIndexes.length} P）`,
        "info",
      );
      const settings = await api.getSettings();
      const conflict = await api.checkDownloadConflict({
        video_id: meta.id,
        page_indexes: pageIndexes,
        format_id: selectedId,
        title: meta.title,
        uploader: meta.uploader ?? "",
        audio_format: container,
      });

      if (conflict.downloading) {
        setError(`该${mediaLabel}已在下载队列中，请等待完成或取消后再试`);
        return;
      }

      let saveAsCopy = false;
      if (conflict.file_exists) {
        if (settings.skip_existing) {
          setInfo(
            pageIndexes.length > 1
              ? "所选分 P 中已有本地文件，已跳过"
              : `本地已有该${mediaLabel}文件，已跳过`,
          );
          return;
        }
        saveAsCopy = true;
      }

      await api.enqueue({
        url,
        video_id: meta.id,
        title: meta.title,
        page_indexes: pageIndexes,
        format_id: selectedId,
        audio_format: container,
        save_as_copy: saveAsCopy,
        uploader: meta.uploader ?? "",
      });
      onEnqueued();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setDownloading(false);
    }
  }

  return (
    <div className="video-card">
      <div className="video-card-header">
        {meta.thumbnail && (
          <img className="video-thumb" src={meta.thumbnail} alt="" />
        )}
        <div className="video-meta">
          <h2>{meta.title}</h2>
          {meta.uploader && <p>{meta.uploader}</p>}
        </div>
      </div>

      <div className="mode-toggle" role="tablist">
        <button
          type="button"
          className={mode === "video" ? "mode-segment active" : "mode-segment"}
          onClick={() => setMode("video")}
        >
          视频
        </button>
        <button
          type="button"
          className={mode === "audio" ? "mode-segment active" : "mode-segment"}
          onClick={() => setMode("audio")}
        >
          仅音频
        </button>
      </div>

      {mode === "video" ? (
        <div>
          <label className="field-label" htmlFor="format-select">
            清晰度
          </label>
          {formatsLoading && meta.formats.length === 0 ? (
            <p className="loading-text">正在获取清晰度…</p>
          ) : (
            <select
              id="format-select"
              className="format-select"
              value={selectedFormatId}
              onChange={(e) => setSelectedFormatId(e.target.value)}
              disabled={formatsLoading || meta.formats.length === 0}
            >
              {sortedFormats.map((f) => (
                <option key={f.format_id} value={f.format_id}>
                  {f.label}
                </option>
              ))}
            </select>
          )}
          {formatsError && (
            <p className="url-hint error">
              清晰度获取失败：{formatsError}
              <button
                type="button"
                className="btn btn-sm"
                style={{ marginLeft: "0.5rem" }}
                onClick={onRefresh}
                disabled={refreshing || downloading}
              >
                重试
              </button>
            </p>
          )}
          {showLoginHint && !formatsLoading && (
            <p className="login-hint">登录后可能获得更高码率清晰度</p>
          )}
        </div>
      ) : (
        <div>
          <div className="audio-format-row">
            <div>
              <label className="field-label" htmlFor="audio-format-select">
                音质
              </label>
              {formatsLoading && sortedAudioFormats.length === 0 ? (
                <p className="loading-text">正在获取音质…</p>
              ) : formatsError ? (
                <p className="url-hint error">
                  音质获取失败：{formatsError}
                  <button
                    type="button"
                    className="btn btn-sm"
                    style={{ marginLeft: "0.5rem" }}
                    onClick={onRefresh}
                    disabled={refreshing || downloading}
                  >
                    重试
                  </button>
                </p>
              ) : sortedAudioFormats.length === 0 ? (
                <p className="loading-text">该视频暂无可下载的音频</p>
              ) : (
                <select
                  id="audio-format-select"
                  className="format-select"
                  value={selectedAudioId}
                  onChange={(e) => setSelectedAudioId(e.target.value)}
                >
                  {sortedAudioFormats.map((f) => (
                    <option key={f.format_id} value={f.format_id}>
                      {f.label}
                    </option>
                  ))}
                </select>
              )}
            </div>
            <div>
              <label className="field-label" htmlFor="audio-container-select">
                格式
              </label>
              <select
                id="audio-container-select"
                className="format-select audio-container-select"
                value={audioFormat}
                disabled={sortedAudioFormats.length === 0}
                onChange={(e) =>
                  setAudioFormat(e.target.value as "m4a" | "mp3" | "flac")
                }
              >
                {(["m4a", "mp3", "flac"] as const).map((fmt) => (
                  <option key={fmt} value={fmt} disabled={fmt === "flac" && !audioIsHires}>
                    {fmt === "flac" ? "FLAC" : fmt}
                  </option>
                ))}
              </select>
            </div>
          </div>
          {!audioIsHires && (
            <p className="url-hint">FLAC 仅 Hi-Res 音源可选</p>
          )}
        </div>
      )}

      {meta.pages.length > 1 && (
        <div className="pages-section">
          <div className="pages-toolbar">
            <span className="field-label" style={{ margin: 0 }}>
              分 P（已选 {selectedPages.size}/{meta.pages.length}）
            </span>
            <div className="pages-actions">
              <button
                type="button"
                className="btn btn-sm"
                onClick={selectAll}
                disabled={allSelected}
              >
                全选
              </button>
              <button
                type="button"
                className="btn btn-sm"
                onClick={selectNone}
                disabled={noneSelected}
              >
                全不选
              </button>
            </div>
          </div>
          <div className="pages-list">
            {meta.pages.map((page) => (
              <label key={page.page_id} className="page-item">
                <input
                  type="checkbox"
                  checked={selectedPages.has(page.index)}
                  onChange={() => togglePage(page.index)}
                />
                <span>
                  P{page.index} {page.title}
                </span>
              </label>
            ))}
          </div>
        </div>
      )}

      {info && <p className="url-hint">{info}</p>}
      {error && <p className="url-hint error">{error}</p>}

      <div className="video-card-actions">
        <button
          type="button"
          className="btn"
          onClick={onRefresh}
          disabled={refreshing || downloading}
        >
          {refreshing ? "刷新中…" : "刷新"}
        </button>
        <button
          type="button"
          data-action="download"
          className="btn btn-primary"
          onClick={() => void handleDownload()}
          disabled={
            refreshing ||
            downloading ||
            (mode === "video" &&
              (formatsLoading || !!formatsError || !selectedFormatId)) ||
            (mode === "audio" &&
              (formatsLoading ||
                !!formatsError ||
                !selectedAudioId ||
                sortedAudioFormats.length === 0)) ||
            noneSelected
          }
        >
          {downloading
            ? "加入队列…"
            : formatsLoading
              ? mode === "audio"
                ? "等待音质…"
                : "等待清晰度…"
              : "下载"}
        </button>
      </div>
    </div>
  );
}
