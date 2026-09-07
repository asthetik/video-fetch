import type { DownloadJob } from "../types";

export interface HistoryGroup {
  label: string;
  jobs: DownloadJob[];
}

/** SQLite datetime('now') strings are UTC "YYYY-MM-DD HH:MM:SS". */
function parseUtc(raw: string): Date | null {
  const d = new Date(`${raw.replace(" ", "T")}Z`);
  return Number.isNaN(d.getTime()) ? null : d;
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

function monthKey(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}`;
}

/**
 * Group finished jobs by local date: today / yesterday / within 7 days /
 * by month (newest first). Jobs without created_at land in a trailing
 * unlabeled "更早" bucket. Input order is preserved within groups.
 */
export function groupHistoryByDate(
  jobs: DownloadJob[],
  now: Date = new Date(),
): HistoryGroup[] {
  const nowDay = startOfDay(now);
  const buckets = new Map<string, DownloadJob[]>();
  const order: string[] = [];
  const noDate: DownloadJob[] = [];

  for (const job of jobs) {
    const d = job.created_at ? parseUtc(job.created_at) : null;
    if (!d) {
      noDate.push(job);
      continue;
    }
    const days = Math.floor((nowDay - startOfDay(d)) / 86_400_000);
    const label =
      days <= 0 ? "今天" : days === 1 ? "昨天" : days < 7 ? "近 7 天" : monthKey(d);
    if (!buckets.has(label)) {
      buckets.set(label, []);
      order.push(label);
    }
    buckets.get(label)!.push(job);
  }

  const groups = order.map((label) => ({ label, jobs: buckets.get(label)! }));
  // "近 7 天" must precede month groups even if a month bucket appeared first.
  groups.sort((a, b) => rank(a.label) - rank(b.label));
  if (noDate.length > 0) {
    groups.push({ label: "更早", jobs: noDate });
  }
  return groups;
}

function rank(label: string): number {
  if (label === "今天") return 0;
  if (label === "昨天") return 1;
  if (label === "近 7 天") return 2;
  return 3;
}
