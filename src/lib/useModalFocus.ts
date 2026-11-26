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
    // Tab is trapped inside the modal while it is open: a modal is a focus
    // boundary — letting focus escape behind the backdrop leaves keyboard
    // users operating an invisible page. The listener rides on document in
    // the capture phase: a busy dialog disables the focused button and the
    // browser drops focus onto body; worse, the dialog backdrops stop
    // keydown propagation while it bubbles up through React's portal
    // delegation on document.body, so a bubbling document listener would
    // never see the event at all. Focus outside the panel (body, or the
    // tabIndex=-1 panel itself) counts as index -1, so Tab pulls it back to
    // the first control and Shift+Tab to the last. Both ends wrap.
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
        // A busy modal can have every control disabled; pin focus on the
        // panel itself instead of letting Tab traverse the hidden page.
        panel.focus();
        e.preventDefault();
        return;
      }
      const current = document.activeElement;
      const index =
        current instanceof HTMLElement && panel.contains(current)
          ? focusables.indexOf(current)
          : -1;
      const next = e.shiftKey
        ? focusables[(index <= 0 ? focusables.length : index) - 1]
        : focusables[(index + 1) % focusables.length];
      next.focus();
      e.preventDefault();
    };
    // Capture phase only: bubbling Tab is stopped at the backdrop before it
    // can reach a document-level bubble listener.
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("keydown", onKeyDown, true);
      // A tab switch can have hidden the trigger (hidden+inert home subtree)
      // while the dialog was open; restoring focus there would fail
      // silently and leave it on body.
      if (previous && previous.isConnected && previous.offsetParent !== null) {
        previous.focus();
      }
    };
  }, [open, panelRef]);
}
