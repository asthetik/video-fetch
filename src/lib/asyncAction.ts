/**
 * Unified handling for rejected invoke() promises and other async failures:
 * one place to turn unknown rejection values into user-facing messages, map a
 * one-shot load onto a terminal load state, and keep action failures from
 * escaping as unhandledrejection events.
 */

/**
 * Structured kinds the backend marks on errors the frontend must branch on
 * (B20): cancelled surfaces stay silent, io/db carry mapped Chinese copy.
 * Generic errors arrive as bare strings (no kind).
 */
export type ApiErrorKind = "cancelled" | "db" | "io";

export interface ApiError {
  message: string;
  kind?: ApiErrorKind;
}

/**
 * Convert an unknown rejection value into its user-facing message plus the
 * structured kind, if the backend annotated one (B20). Tolerates every
 * historical shape: Error instances, bare strings (the generic wire shape
 * and tauri-internal rejections), {message} objects, and full envelopes.
 */
export function toApiError(err: unknown): ApiError {
  if (err instanceof Error) return { message: err.message };
  if (typeof err === "string") return { message: err };
  if (
    typeof err === "object" &&
    err !== null &&
    typeof (err as { message?: unknown }).message === "string"
  ) {
    const raw = err as { message?: unknown; kind?: unknown };
    const kind =
      raw.kind === "cancelled" || raw.kind === "db" || raw.kind === "io"
        ? (raw.kind as ApiErrorKind)
        : undefined;
    return { message: raw.message as string, kind };
  }
  return { message: String(err) };
}

/** Convert an unknown rejection value into a user-facing message. */
export function toErrorMessage(err: unknown): string {
  return toApiError(err).message;
}

/**
 * Terminal states of a one-shot async load. A settled load is never
 * "loading": failures land in "error" and stay there until a retry runs.
 */
export type LoadState<T> =
  | { status: "loading" }
  | { status: "ready"; data: T }
  | { status: "error"; message: string };

/** Run a fetcher and map its settlement to a LoadState. */
export async function toLoadState<T>(
  fetcher: () => Promise<T>,
): Promise<LoadState<T>> {
  try {
    return { status: "ready", data: await fetcher() };
  } catch (err) {
    return { status: "error", message: toErrorMessage(err) };
  }
}

/**
 * Run an async action and surface failures through notifyError instead of
 * letting the rejection escape as an unhandledrejection. Resolves to
 * undefined on failure so callers can branch on the result.
 */
export async function withToast<T>(
  run: () => Promise<T>,
  notifyError: (message: string) => void,
): Promise<T | undefined> {
  try {
    return await run();
  } catch (err) {
    notifyError(toErrorMessage(err));
    return undefined;
  }
}
