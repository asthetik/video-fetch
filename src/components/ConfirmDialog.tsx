import { useId, useRef } from "react";
import { useModalFocus } from "../lib/useModalFocus";

interface ConfirmDialogProps {
  open: boolean;
  title: string;
  message: string;
  confirmLabel: string;
  /** Label for the dismiss button. Defaults to Cancel. */
  cancelLabel?: string;
  /** When true, confirm button uses btn-danger */
  danger?: boolean;
  busy?: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}

export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel,
  cancelLabel = "取消",
  danger = false,
  busy = false,
  onCancel,
  onConfirm,
}: ConfirmDialogProps) {
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement>(null);
  useModalFocus(open, panelRef);

  if (!open) {
    return null;
  }

  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onClick={() => {
        if (!busy) onCancel();
      }}
      onKeyDown={(e) => {
        // Consume Escape here even while busy so a nested dialog underneath stays open.
        e.stopPropagation();
        if (e.key === "Escape" && !busy) {
          onCancel();
        }
      }}
    >
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        ref={panelRef}
        onClick={(e) => e.stopPropagation()}
      >
        <h3 id={titleId}>{title}</h3>
        <p className="modal-desc">{message}</p>
        <div className="modal-actions">
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={onCancel}
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            className={`btn${danger ? " btn-danger" : ""}`}
            disabled={busy}
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
