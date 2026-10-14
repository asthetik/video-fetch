/**
 * Dedupe gate for URL-resolution dispatches.
 *
 * A URL is skipped while it is already in flight, and skipped after it has
 * resolved successfully (re-focusing or re-pasting the same link must not
 * re-resolve). A failed URL stays retryable: the gate must never record a
 * dispatch as a settled resolution — doing that is what once turned a failed
 * paste into a dead end (re-pasting the same link did nothing).
 */
export interface ResolveGate {
  /** True when this URL should be dispatched now; records it as in flight. */
  shouldDispatch(url: string): boolean;
  /** Report an outcome for a dispatched URL. */
  settle(url: string, ok: boolean): void;
  /** Forget everything (the input was emptied). */
  clear(): void;
}

export function createResolveGate(): ResolveGate {
  let inFlight: string | null = null;
  let resolved: string | null = null;
  return {
    shouldDispatch(url: string): boolean {
      if (!url || url === inFlight || url === resolved) {
        return false;
      }
      // The screen is about to change: whatever was resolved before is no
      // longer on it, so that URL becomes dispatchable again.
      resolved = null;
      inFlight = url;
      return true;
    },
    settle(url: string, ok: boolean): void {
      // A late result of a superseded dispatch settles nothing.
      if (inFlight !== url) {
        return;
      }
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
