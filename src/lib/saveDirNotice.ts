/**
 * Session save-dir fallback notice. The backend sets a notice when the
 * configured save directory is unusable and this session's downloads redirect
 * to the app-data fallback without touching the saved setting (see
 * get_save_dir_notice). The startup toast is easy to miss, so the settings
 * save-dir row keeps the condition visible for the whole session.
 */

/**
 * Compact hint for the settings save-dir row: non-null iff `notice` is set.
 * The copy names the condition and points out where downloads go this session
 * and that the configured path is unchanged.
 */
export function saveDirRowHint(notice: string | null | undefined): string | null {
  if (!notice) return null;
  return `${notice}，本次下载暂存至应用数据目录，设置中的保存路径未更改`;
}
