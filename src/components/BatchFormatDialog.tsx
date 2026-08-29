import { useEffect, useId, useRef, useState } from "react";
import { useModalFocus } from "../lib/useModalFocus";
import { ConfirmDialog } from "./ConfirmDialog";

export interface BatchFormatDialogProps {
  open: boolean;
  count: number;
  onCancel: () => void;
  onSubmit: (formatId: string, audioFormat: string | null) => void;
}

/** Fixed tier set (spec §4.4): the vh prefix is expanded by the backend's
 * dash_format_selector; "highest" uses a pass-through selector so yt-dlp's
 * `best` cannot land on a low-res pre-muxed stream. */
const VIDEO_TIERS = [
  { formatId: "bestvideo+bestaudio/best", label: "最高画质" },
  { formatId: "vh1080", label: "1080P 及以下" },
  { formatId: "vh720", label: "720P 及以下" },
  { formatId: "vh480", label: "480P 及以下" },
];
const AUDIO_TIERS = [
  { formatId: "bestaudio", audioFormat: "m4a", label: "m4a（AAC 最高音质）" },
  { formatId: "bestaudio", audioFormat: "mp3", label: "mp3" },
  { formatId: "bestaudio", audioFormat: "flac", label: "FLAC（需 Hi-Res，无音源的视频将失败）" },
];

export function BatchFormatDialog({ open, count, onCancel, onSubmit }: BatchFormatDialogProps) {
  const [mode, setMode] = useState<"video" | "audio">("video");
  const [videoTierId, setVideoTierId] = useState(VIDEO_TIERS[0].formatId);
  const [audioIdx, setAudioIdx] = useState(0);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement>(null);
  useModalFocus(open, panelRef);

  useEffect(() => {
    if (open) {
      setMode("video");
      setVideoTierId(VIDEO_TIERS[0].formatId);
      setAudioIdx(0);
      setConfirmOpen(false);
    }
  }, [open]);

  const audioTier = AUDIO_TIERS[audioIdx];
  const formatId = mode === "video" ? videoTierId : audioTier.formatId;
  const audioFormat = mode === "audio" ? audioTier.audioFormat : null;

  function handleSubmit() {
    if (count > 50) {
      setConfirmOpen(true);
      return;
    }
    onSubmit(formatId, audioFormat);
  }

  return (
    <>
      {open && (
        <div
          className="modal-backdrop"
          onClick={onCancel}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              onCancel();
            }
          }}
        >
          <div
            className="modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby={titleId}
            tabIndex={-1}
            ref={panelRef}
            onClick={(e) => e.stopPropagation()}
          >
            <h3 id={titleId}>批量下载 {count} 个视频</h3>
            <div className="mode-toggle" role="group" aria-label="下载类型">
              {(["video", "audio"] as const).map((value) => (
                <button
                  key={value}
                  type="button"
                  className={mode === value ? "mode-segment active" : "mode-segment"}
                  aria-pressed={mode === value}
                  onClick={() => setMode(value)}
                >
                  {value === "video" ? "视频" : "仅音频"}
                </button>
              ))}
            </div>
            {mode === "video" ? (
              <div>
                <label className="field-label" htmlFor="batch-video-tier">
                  清晰度（统一应用到全部选中视频）
                </label>
                <select
                  id="batch-video-tier"
                  className="format-select"
                  value={videoTierId}
                  onChange={(e) => setVideoTierId(e.target.value)}
                >
                  {VIDEO_TIERS.map((t) => (
                    <option key={t.formatId} value={t.formatId}>
                      {t.label}
                    </option>
                  ))}
                </select>
              </div>
            ) : (
              <div>
                <label className="field-label" htmlFor="batch-audio-tier">
                  音频格式
                </label>
                <select
                  id="batch-audio-tier"
                  className="format-select"
                  value={audioIdx}
                  onChange={(e) => setAudioIdx(Number(e.target.value))}
                >
                  {AUDIO_TIERS.map((t, idx) => (
                    <option key={t.audioFormat} value={idx}>
                      {t.label}
                    </option>
                  ))}
                </select>
              </div>
            )}
            <div className="modal-actions">
              <button type="button" className="btn" onClick={onCancel}>
                取消
              </button>
              <button type="button" className="btn btn-primary" data-action="batch-submit" onClick={handleSubmit}>
                添加 {count} 个任务
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmOpen}
        title="确认批量下载"
        message={`将添加 ${count} 个下载任务，确认继续？`}
        confirmLabel="继续"
        cancelLabel="取消"
        busy={false}
        onCancel={() => setConfirmOpen(false)}
        onConfirm={() => {
          setConfirmOpen(false);
          onSubmit(formatId, audioFormat);
        }}
      />
    </>
  );
}
