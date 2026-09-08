import type { LucideIcon } from "lucide-react";

interface IconButtonProps {
  icon: LucideIcon;
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
  action?: string;
}

export function IconButton({
  icon: Icon,
  label,
  onClick,
  danger = false,
  disabled = false,
  action,
}: IconButtonProps) {
  return (
    <button
      type="button"
      className={`icon-btn${danger ? " danger" : ""}`}
      aria-label={label}
      title={label}
      data-action={action}
      disabled={disabled}
      onClick={onClick}
    >
      <Icon size={14} strokeWidth={2} />
    </button>
  );
}
