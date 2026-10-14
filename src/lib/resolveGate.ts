/**
 * Dedupe gate for URL-resolution dispatches.
 *
 * A URL is skipped while it is already in flight, and skipped after it has
 * resolved successfully (re-focusing or re-pasting the same link must not
 * re-resolve). A failed URL stays retryable: the gate must never record a
 * dispatch as a settled resolution — doing that is what once turned a failed
 * paste into a dead end (re-pasting the same link did nothing).
 *
 * Settles are keyed by dispatch id, not by URL: the late result of a dead
 * dispatch must not clear the slot of a newer dispatch for the same URL.
 */
export interface ResolveGate {
  /** Dispatch id for this URL, or null when it should be skipped. */
  begin(url: string): number | null;
  /** Report the outcome of the dispatch `begin` returned. */
  settle(id: number, ok: boolean): void;
  /** Forget everything (the input was emptied). */
  clear(): void;
}

export function createResolveGate(): ResolveGate {
  let seq = 0;
  let inFlight: { id: number; url: string } | null = null;
  let resolved: string | null = null;
  return {
    begin(url: string): number | null {
      if (!url || url === inFlight?.url || url === resolved) {
        return null;
      }
      // The screen is about to change: whatever was resolved before is no
      // longer on it, so that URL becomes dispatchable again.
      resolved = null;
      seq += 1;
      inFlight = { id: seq, url };
      return seq;
    },
    settle(id: number, ok: boolean): void {
      // A late result of a superseded dispatch settles nothing.
      if (inFlight?.id !== id) {
        return;
      }
      const url = inFlight.url;
      inFlight = null;
      if (ok) {
        resolved = url;
      }
    },
    clear(): void {
      inFlight = null;
      resolved = null;
    },
  };
}
