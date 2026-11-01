/**
 * Whether a backend-allocated resolve id that just settled may still run.
 *
 * The claim is async (one IPC round-trip), and id comparison alone cannot
 * tell "superseded by a newer claim" (ids are monotonic, so an older id is
 * stale) from "discarded mid-window": discard bumps the request-id ref
 * itself, and the fresh id can come back equal to that bumped ref, which
 * would let a discarded resolve resurrect. The discard epoch closes that
 * hole — any discard during the window changes the epoch.
 */
export function claimSurvivesDiscard(
  claimedId: number,
  currentRequestId: number,
  epochAtClaim: number,
  currentEpoch: number,
): boolean {
  return epochAtClaim === currentEpoch && claimedId >= currentRequestId;
}
