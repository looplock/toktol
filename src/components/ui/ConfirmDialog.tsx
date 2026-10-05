/**
 * 轻确认弹层：标题 + 一行说明 + 取消/确认两键的居中小对话框。受控组件：
 * open=false 渲染 null，不挂任何节点。遮罩点击与 Esc 都等价于"取消"；
 * 确认键是全应用唯一的危险色实体按钮（破坏性操作的最后道闸），busy 时
 * 禁点防重复提交。焦点自动落在确认键上——回车即确认，Esc 即取消。
 */

import { useEffect } from "react";

interface ConfirmDialogProps {
  readonly open: boolean;
  readonly title: string;
  readonly body: string;
  readonly confirmLabel: string;
  readonly cancelLabel: string;
  /** 确认在途：禁点两个键，确认键文案由调用方换成进行时。 */
  readonly busy?: boolean;
  readonly onConfirm: () => void;
  readonly onCancel: () => void;
}

export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel,
  cancelLabel,
  busy = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  // Esc 取消：与页面级 Esc 语义一致（详情视图的 Esc 返回监听在弹层
  // 打开时同样触发，但弹层自身先关掉，返回列表要再按一次——直觉顺序）。
  useEffect(() => {
    if (!open) return undefined;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onCancel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onCancel]);

  if (!open) return null;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-ink/40 p-6"
      onClick={onCancel}
    >
      <div
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog-title"
        aria-describedby="confirm-dialog-body"
        onClick={(event) => event.stopPropagation()}
        className="w-full max-w-sm rounded-card border border-border bg-surface p-5 shadow-xl"
      >
        <h3 id="confirm-dialog-title" className="text-sm font-semibold text-ink-strong">
          {title}
        </h3>
        <p id="confirm-dialog-body" className="mt-1.5 text-xs leading-relaxed text-ink-muted">
          {body}
        </p>
        <div className="mt-4 flex justify-end gap-2">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="rounded-control border border-border bg-surface-subtle px-3 py-1.5 text-sm text-ink transition-colors hover:bg-surface-muted disabled:cursor-not-allowed disabled:opacity-50"
          >
            {cancelLabel}
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            autoFocus
            className="rounded-control border border-danger bg-danger px-3 py-1.5 text-sm text-white transition-colors hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
