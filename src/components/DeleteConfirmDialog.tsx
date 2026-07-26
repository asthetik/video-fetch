export type DeleteChoice = "cancel" | "record" | "record_and_file";

interface DeleteConfirmDialogProps {
  open: boolean;
  jobTitle: string;
  /** When set, offer deleting the local file as well. */
  filePath?: string | null;
  onChoose: (choice: DeleteChoice) => void;
}

export function DeleteConfirmDialog({
  open,
  jobTitle,
  filePath,
  onChoose,
}: DeleteConfirmDialogProps) {
  if (!open) {
    return null;
  }

  const canDeleteFile = Boolean(filePath);

  return (
    <div
      className="modal-backdrop"
      role="presentation"
      onClick={() => onChoose("cancel")}
    >
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="delete-dialog-title"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 id="delete-dialog-title">删除任务</h3>

        {canDeleteFile && filePath ? (
          <>
            <p className="modal-desc">
              「{jobTitle}」
              <br />
              请选择要执行的操作：
            </p>
            <p className="modal-path">{filePath}</p>
            <div className="modal-actions">
              <button
                type="button"
                className="btn"
                onClick={() => onChoose("cancel")}
              >
                取消
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => onChoose("record")}
              >
                只删除记录
              </button>
              <button
                type="button"
                className="btn btn-danger"
                onClick={() => onChoose("record_and_file")}
              >
                删除记录和本地文件
              </button>
            </div>
          </>
        ) : (
          <>
            <p className="modal-desc">
              确定删除下载任务「{jobTitle}」？
              <br />
              仅移除队列中的任务，不会影响本地文件。
            </p>
            <div className="modal-actions">
              <button
                type="button"
                className="btn"
                onClick={() => onChoose("cancel")}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-danger"
                onClick={() => onChoose("record")}
              >
                删除任务
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
