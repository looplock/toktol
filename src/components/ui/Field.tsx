/**
 * 表单行：标签 + 控件 + 说明。
 * 根元素刻意用 div 而不是 label：Field 里会包按钮（如网关页只读值 + 复制按钮），
 * label 会把悬停/点击转发给它内部第一个可标注控件——鼠标在只读框上时复制按钮
 * 跟着高亮甚至被点击。需要「点文字聚焦输入框」的场合由控件自身 aria-label 兜底。
 */

import type { ReactNode } from "react";

interface FieldProps {
  readonly label: string;
  readonly hint?: string | undefined;
  readonly children: ReactNode;
}

export function Field({ label, hint, children }: FieldProps) {
  return (
    <div className="block">
      <span className="text-sm">{label}</span>
      <span className="mt-1 block">{children}</span>
      <span
        className={`mt-1 block text-xs text-ink-muted ${hint === undefined ? "hidden" : ""}`}
      >
        {hint}
      </span>
    </div>
  );
}
