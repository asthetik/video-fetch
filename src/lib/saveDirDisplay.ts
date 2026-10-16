/**
 * Save-dir input placeholder for the settings row. The backend keeps the
 * session's effective save dir out of `settings.save_dir` when the OS path is
 * not representable as a String (see `apply_save_dir`): on a first run with a
 * non-UTF-8 default, the raw path never crosses the IPC String channel and a
 * lossy melt would pin a U+FFFD directory the real path can never match. An
 * empty field must not read as "not configured" while this session really
 * downloads somewhere real.
 */

/**
 * Placeholder text for the read-only save-dir input: the stand-in label when
 * `save_dir` is empty and no session notice is shown, `""` (no placeholder)
 * otherwise. A configured path is the input value and needs no placeholder.
 *
 * An empty `save_dir` has two shapes, both decided by `apply_save_dir`: with
 * no notice, the session downloads to the unprintable system default, so the
 * stand-in is accurate; with the unavailability notice shown, the session
 * fell back to the app-data directory — the hint below the field says so —
 * and a "system default" stand-in would contradict it, so the placeholder
 * stands down and the hint speaks alone.
 */
export function saveDirPlaceholder(saveDir: string, noticeShown: boolean): string {
  if (saveDir !== "" || noticeShown) {
    return "";
  }
  return "（本次会话使用系统默认下载目录）";
}
