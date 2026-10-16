/**
 * Visibility rule for the URL-bar error hint's retry button.
 *
 * The error message stays on screen after the input is cleared so the user
 * can still read why the last attempt failed, but the retry action itself
 * must disappear with its target: on an empty input the click is a dead
 * affordance (handleRetry early-returns on empty, so it ran nothing, and it
 * gave no feedback either) — the same dead-end kind A6 fixed. Gating the
 * button on a non-empty URL removes the dead click while keeping the error
 * text as context. When the input has content, retry stays a
 * deliberate bypass of the resolve dedup gate (see resolveGate.ts).
 */

/**
 * Whether the retry button should render: an error is showing AND the input
 * holds a non-empty (after trim) URL. Whitespace-only counts as empty.
 */
export function isRetryVisible(error: string | null, url: string): boolean {
  return Boolean(error) && url.trim().length > 0;
}
