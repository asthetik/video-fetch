// src/components/AuthChip.tsx
import { useAuthStatus } from "./AuthStatus";

const LABEL: Record<string, string> = {
  logged_in: "已登录",
  logged_out: "未登录",
  possibly_expired: "登录可能过期",
};

interface AuthChipProps {
  onOpenSettings: () => void;
}

/** Compact header status pill; full login/logout controls live in Settings. */
export function AuthChip({ onOpenSettings }: AuthChipProps) {
  const status = useAuthStatus();
  return (
    <button
      type="button"
      className={`auth-chip ${status}`}
      data-action="open-auth-settings"
      onClick={onOpenSettings}
      title="B 站登录管理（设置页）"
    >
      <span className="auth-chip-dot" />
      {LABEL[status]}
    </button>
  );
}
