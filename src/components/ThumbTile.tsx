import { useEffect, useState } from "react";
import { Music, Play } from "lucide-react";
import { formatDuration } from "../lib/spaceFormat";
import type { DownloadJob } from "../types";

// A flat dark scrim under the white play/note glyph: palette pairs alone
// drop the icon below DESIGN.md's 3:1 non-text floor (the amber end of the
// old celebrate pair reached 1.8:1), and the scrim keeps every stop above
// it in both themes.
const SCRIM = "linear-gradient(rgba(0, 0, 0, 0.3), rgba(0, 0, 0, 0.3))";
const FALLBACK_GRADIENTS = [
  "linear-gradient(135deg, var(--pink), var(--accent))",
  "linear-gradient(135deg, var(--success), var(--pink))",
  "linear-gradient(135deg, var(--warn), var(--accent))",
  "linear-gradient(135deg, var(--accent), var(--success))",
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
  const [failed, setFailed] = useState(false);
  // A different thumbnail (recycled row) deserves a fresh load attempt.
  useEffect(() => setFailed(false), [job.thumbnail_url]);
  const gradient = FALLBACK_GRADIENTS[hashIndex(job.id, FALLBACK_GRADIENTS.length)];
  const duration = job.duration_secs ? formatDuration(job.duration_secs) : null;

  return (
    <span className={`thumb-tile${audio ? " audio" : ""}`}>
      {!failed && job.thumbnail_url ? (
        <img
          src={job.thumbnail_url}
          alt=""
          loading="lazy"
          onError={() => setFailed(true)}
        />
      ) : (
        <span
          className="thumb-fallback"
          style={{ background: `${SCRIM}, ${gradient}` }}
        >
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
