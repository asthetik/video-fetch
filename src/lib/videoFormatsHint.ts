/**
 * What the video tab should say when it has no video formats to offer.
 *
 * An audio-only resolve renders an empty, disabled format select and a greyed
 * download button; without a hint that state reads as a broken card. The
 * both-empty case (degenerate metadata) needs its own wording: telling the
 * user to switch to a tab that is equally empty is a dead end.
 */
export function videoFormatsHint(
  videoCount: number,
  audioCount: number,
): string | null {
  if (videoCount > 0) {
    return null;
  }
  if (audioCount > 0) {
    return "该视频仅提供音频格式，请切换到「仅音频」";
  }
  return "该视频暂无可下载的清晰度";
}
