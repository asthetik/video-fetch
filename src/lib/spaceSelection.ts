/**
 * Selection state after the Space list's keyword/order filter changes.
 *
 * A keyword or order change replaces the visible rows the user picked from.
 * Keeping the old selection would leave the batch bar counting rows that are
 * no longer on screen, and "下载已选" would silently enqueue them. Any
 * filter/sort change therefore drops every previously selected id.
 */
export function selectionAfterFilterChange<T>(
  _prevSelected: ReadonlyMap<string, T>,
): Map<string, T> {
  return new Map();
}

/**
 * Uploader state after the Space list's mid (space id) changes.
 *
 * The batch dialog passes the space owner's name into `spaceEnqueueBatch`,
 * which expands `%(uploader)s` in the skip-existing prediction with it. The
 * previous space's name is stale the moment the mid changes — it would
 * linger in state until the new `spaceInfo` resolves — so a mid change
 * clears it as part of the same list reset. Until the new name arrives the
 * batch path sends an empty uploader, the backend's pre-existing fallback
 * for callers that omit the name.
 */
export function uploaderAfterMidChange(_prevUploader: string): string {
  return "";
}
