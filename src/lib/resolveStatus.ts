import type { VideoMeta } from "../types.ts";

/**
 * Loading/error state for the resolve flow: the in-card formats row (spinner +
 * error with retry) plus the page-level error shown by the URL bar when no
 * card is rendered.
 */
export interface ResolveStatus {
  /** Page-level error; only meaningful while no card is rendered. */
  error: string | null;
  /** In-card "正在获取清晰度…" spinner. */
  formatsLoading: boolean;
  /** In-card formats error; the card folds it into its retry button. */
  formatsError: string | null;
}

export const initialResolveStatus: ResolveStatus = {
  error: null,
  formatsLoading: false,
  formatsError: null,
};

export type ResolveStatusEvent =
  /** A resolve command started (first resolve or an in-card retry). */
  | { type: "resolve_started" }
  /** A space (or similar) resolved: no card will render, nothing failed. */
  | { type: "settled_idle" }
  /** The link itself is bad: show `message` in the URL bar, no card. */
  | { type: "settled_page_error"; message: string }
  /** `resolve://partial`: card rendered, formats still coming. */
  | { type: "partial" }
  /** `resolve://formats_progress`: formats still coming. */
  | { type: "formats_progress" }
  /** `resolve://complete`, or a resolve result that already carried formats. */
  | { type: "complete" }
  /** `resolve://formats_failed`: this request ended in failure. */
  | { type: "formats_failed"; message: string }
  /**
   * The resolve command was rejected (catch path). Some Err exits emit no
   * terminal event (the cache-write failure emits `formats_failed` before
   * returning Err; others do not), so the rejection itself must settle the
   * state. `hasCard` only decides where the message is surfaced; the formats
   * spinner reset is unconditional, so a card already rendered (partial
   * arrived earlier, or a force refresh kept old meta) never spins forever.
   */
  | { type: "resolve_rejected"; message: string; hasCard: boolean };

/** Pure state transitions for the resolve loading/error flow. */
export function reduceResolveStatus(
  state: ResolveStatus,
  event: ResolveStatusEvent,
): ResolveStatus {
  switch (event.type) {
    case "resolve_started":
    case "partial":
      return { error: null, formatsLoading: true, formatsError: null };
    case "settled_idle":
      return { error: null, formatsLoading: false, formatsError: null };
    case "settled_page_error":
      return {
        error: event.message,
        formatsLoading: false,
        formatsError: null,
      };
    case "formats_progress":
      return { ...state, formatsLoading: true };
    case "complete":
      return { ...state, formatsLoading: false, formatsError: null };
    case "formats_failed":
      return { ...state, formatsLoading: false, formatsError: event.message };
    case "resolve_rejected":
      return {
        error: event.hasCard ? state.error : event.message,
        formatsLoading: false,
        formatsError: event.message,
      };
  }
}

/**
 * Whether a resolve command result already carries formats and should
 * dispatch `complete` itself. Any meta with video or audio formats counts:
 * the cache-hit path returns Ok(meta) with no events at all, so without this
 * dispatch an audio-only result would leave the formats spinner stuck.
 * A result with neither format kind is left to the `complete` /
 * `formats_failed` events (degenerate meta; they are the only terminal).
 */
export function shouldDispatchComplete(
  result: Pick<VideoMeta, "formats" | "audio_formats">,
): boolean {
  return result.formats.length > 0 || result.audio_formats.length > 0;
}
