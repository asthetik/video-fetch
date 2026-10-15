/**
 * Unified handling for rejected invoke() promises and other async failures:
 * one place to turn unknown rejection values into user-facing messages, map a
 * one-shot load onto a terminal load state, and keep action failures from
 * escaping as unhandledrejection events.
 */

/** Convert an unknown rejection value into a user-facing message. */
export function toErrorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  if (
    typeof err === "object" &&
    err !== null &&
    typeof (err as { message?: unknown }).message === "string"
  ) {
    return (err as { message: string }).message;
  }
  return String(err);
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
