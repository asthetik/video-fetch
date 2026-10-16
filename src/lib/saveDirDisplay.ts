/**
 * Save-dir input placeholder for the settings row. The backend keeps the
 * session's effective save dir out of `settings.save_dir` when the OS path is
 * not representable as a String (see `apply_save_dir`): on a first run with a
 * non-UTF-8 default, the raw path never crosses the IPC String channel and a
 * lossy melt would pin a U+FFFD directory the real path can never match. An
 * empty field must not read as "not configured" while this session really
 * downloads to the system default download directory.
 */

/**
 * Placeholder text for the read-only save-dir input: the stand-in label when
 * `save_dir` is empty, `""` (no placeholder) otherwise. A mounted settings
 * page always has a session-effective save dir (an `apply_save_dir`
 * invariant), so emptiness is the only condition to check. A configured path
 * is the input value and needs no placeholder. The label is prose, never a
 * melted path stand-in.
 */
export function saveDirPlaceholder(saveDir: string): string {
  if (saveDir !== "") {
    return "";
  }
  return "（本次会话使用系统默认下载目录）";
}
