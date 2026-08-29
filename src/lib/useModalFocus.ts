import { useEffect, type RefObject } from "react";

/** Move focus into an open modal panel and restore it to the previously
 * focused element when the modal closes. */
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
    return () => {
      previous?.focus();
    };
  }, [open, panelRef]);
}
