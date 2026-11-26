import { useEffect, type RefObject } from "react";

/** Move focus into an open modal panel, keep Tab cycling inside it while the
 * modal is open, and restore focus to the previously focused element when the
 * modal closes. */
const FOCUSABLE_SELECTOR = [
  "a[href]",
  "button:not([disabled])",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  "[tabindex]:not([tabindex='-1'])",
].join(",");

export function useModalFocus(
  open: boolean,
  panelRef: RefObject<HTMLElement | null>,
) {
  useEffect(() => {
    if (!open) {
      return;
    }
    const previous =
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    panelRef.current?.focus();
    // Tab is trapped inside the panel while it is open: a modal is a focus
    // boundary — letting focus escape behind the backdrop leaves keyboard
    // users operating an invisible page. Both ends wrap: Tab from the last
    // control returns to the first; Shift+Tab from the first (or from the
    // panel itself, whose index is -1) lands on the last.
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Tab") {
        return;
      }
      const panel = panelRef.current as HTMLElement | null;
      if (!panel) {
        return;
      }
      const focusables = Array.from(
        panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
      ).filter((el) => el.offsetParent !== null);
      if (focusables.length === 0) {
        e.preventDefault();
        return;
      }
      const current = document.activeElement;
      const index = focusables.indexOf(current as HTMLElement);
      const next = e.shiftKey
        ? focusables[(index <= 0 ? focusables.length : index) - 1]
        : focusables[(index + 1) % focusables.length];
      next.focus();
      e.preventDefault();
    };
    const panelEl = panelRef.current;
    panelEl?.addEventListener("keydown", onKeyDown);
    return () => {
      panelEl?.removeEventListener("keydown", onKeyDown);
      previous?.focus();
    };
  }, [open, panelRef]);
}
