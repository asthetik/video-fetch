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
