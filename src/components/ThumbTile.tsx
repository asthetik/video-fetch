import { Music, Play } from "lucide-react";
import { formatDuration } from "../lib/spaceFormat";
import type { DownloadJob } from "../types";

const FALLBACK_GRADIENTS = [
  "linear-gradient(135deg, #5e6ad2, #8b5cf6)",
  "linear-gradient(135deg, #2f8f6b, #5e6ad2)",
  "linear-gradient(135deg, #c2477e, #8b5cf6)",
  "linear-gradient(135deg, #5e6ad2, #2f8f6b)",
];

// Deterministic gradient pick: the multiply-accumulate is exact in double
// precision below 2^53, then the int32 OR-wrap keeps it bounded before mod.
function hashIndex(id: string, mod: number): number {
  let h = 0;
  for (const ch of id) {
    h = (h * 31 + ch.charCodeAt(0)) | 0;
  }
  return Math.abs(h) % mod;
}

function NoteBadge() {
  return (
    <span className="thumb-note" aria-hidden="true">
      <Music size={9} strokeWidth={2.6} />
    </span>
  );
}

/** History-row thumbnail: 16:9 for video, square tile for audio-only. */
export function ThumbTile({ job }: { job: DownloadJob }) {
  const audio = job.audio_format != null;
  const gradient = FALLBACK_GRADIENTS[hashIndex(job.id, FALLBACK_GRADIENTS.length)];
  const duration = job.duration_secs ? formatDuration(job.duration_secs) : null;

  return (
    <span className={`thumb-tile${audio ? " audio" : ""}`}>
      {job.thumbnail_url ? (
        <img src={job.thumbnail_url} alt="" loading="lazy" />
      ) : (
        <span className="thumb-fallback" style={{ background: gradient }}>
          {audio ? (
            <Music size={16} strokeWidth={2} aria-hidden="true" />
          ) : (
            <Play size={16} strokeWidth={2} aria-hidden="true" />
          )}
        </span>
      )}
      {audio ? (
        <NoteBadge />
      ) : (
        duration && <span className="thumb-dur">{duration}</span>
      )}
    </span>
  );
}
