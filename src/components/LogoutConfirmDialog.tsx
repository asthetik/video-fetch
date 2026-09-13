import { ConfirmDialog } from "./ConfirmDialog";

const LOGOUT_CONFIRM = "确定退出登录？退出后将按未登录状态解析与下载。";

interface LogoutConfirmDialogProps {
  open: boolean;
  busy: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Logout confirmation shared by the header auth chip and Settings' auth row. */
export function LogoutConfirmDialog({
  open,
  busy,
  onConfirm,
  onCancel,
}: LogoutConfirmDialogProps) {
  return (
    <ConfirmDialog
      open={open}
      title="退出登录"
      message={LOGOUT_CONFIRM}
      confirmLabel="退出登录"
      cancelLabel="关闭"
      danger
      busy={busy}
      onCancel={onCancel}
      onConfirm={onConfirm}
    />
  );
}
