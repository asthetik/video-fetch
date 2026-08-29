/** Bilibili-style play count in wan (1e4) / yi (1e8) units, e.g. 32000 -> "3.2wan"; below 10000 shown raw. */
export function formatPlayCount(n: number): string {
  const trim = (s: string) => s.replace(/\.0$/, "");
  if (n >= 100000000) return `${trim((n / 100000000).toFixed(1))}亿`;
  if (n >= 10000) return `${trim((n / 10000).toFixed(1))}万`;
  return String(n);
}

/** Unix seconds → YYYY-MM-DD in Bilibili's fixed UTC+8 so dates match the
 * source site regardless of the viewer's timezone; 0 renders as "—". */
export function formatDate(secs: number): string {
  if (!secs) return "—";
  const d = new Date(secs * 1000 + 8 * 3600 * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}`;
}

/** Seconds → mm:ss / hh:mm:ss; 0 renders as "—". */
export function formatDuration(secs: number): string {
  if (!secs) return "—";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const p = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${p(m)}:${p(s)}` : `${m}:${p(s)}`;
}
