/**
 * Dedup gate for UrlBar's auto-resolve.
 *
 * Auto-resolve fires shortly after typing stops; blur/refocus churn (e.g.
 * focus leaving the input for the quality select) re-enters the same path
 * with an unchanged value. The gate skips those repeats by remembering the
 * URL of the last resolve that *succeeded*. A failed resolve is never
 * remembered: re-pasting the same link after an error must schedule a fresh
 * attempt instead of dead-ending the input. Clearing the input drops the
 * memory so the same link can resolve again.
 */

export interface ResolveGate {
  /** URL of the last resolve that succeeded ("" = none). */
  lastResolved: string;
}

export const EMPTY_RESOLVE_GATE: ResolveGate = { lastResolved: "" };

/**
 * Classify a (possibly unchanged) input value: whether to schedule a resolve
 * and which gate to keep afterwards. Empty input resets the gate; an
 * unchanged value equal to `lastResolved` is skipped; anything else
 * schedules and leaves the gate for the outcome to update.
 */
export function gateInputChange(
  gate: ResolveGate,
  nextUrl: string,
): { schedule: boolean; gate: ResolveGate } {
  const trimmed = nextUrl.trim();
  if (!trimmed) {
    // Input cleared: drop the memory so a re-paste of the same URL works.
    return { schedule: false, gate: EMPTY_RESOLVE_GATE };
  }
  if (trimmed === gate.lastResolved) {
    return { schedule: false, gate };
  }
  // Do not remember the URL here: writing it before the outcome is known is
  // what used to turn a failed resolve into a dead-end input. Only
  // gateResolveOutcome() after a successful resolve may write it.
  return { schedule: true, gate };
}

/**
 * Record a resolve outcome: only a successful resolve is remembered, a
 * failure leaves the gate untouched (it never plants the failed URL).
 */
export function gateResolveOutcome(
  gate: ResolveGate,
  url: string,
  succeeded: boolean,
): ResolveGate {
  return succeeded ? { lastResolved: url.trim() } : gate;
}
