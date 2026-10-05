/**
 * 按钮。四个变体各自对应一种"重量"，不再加第五种——
 * 选项越多，同一页面里越容易同时出现两种同样重的按钮。
 * icon：圆形纯图标钮（紧凑位的复制/翻页/目录切换），必须配 aria-label。
 */

import type { ButtonHTMLAttributes, ReactNode } from "react";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "icon";
export type ButtonSize = "md" | "sm";

interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className"> {
  readonly variant?: ButtonVariant;
  /** sm：固定 h-8，与 h-8 输入框同行使用；md：默认文档流高度。 */
  readonly size?: ButtonSize;
  readonly children: ReactNode;
}

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  primary: "border-accent bg-accent text-accent-ink hover:opacity-90",
  secondary: "border-border bg-surface-subtle text-ink hover:bg-surface-muted",
  ghost: "border-transparent bg-transparent text-ink-muted hover:bg-surface-subtle hover:text-ink",
  icon: "size-7 rounded-full border-transparent bg-transparent p-0 text-ink-muted hover:bg-surface-subtle hover:text-ink",
};

const SIZE_CLASS: Record<Exclude<ButtonSize, "md">, string> = {
  sm: "h-8 rounded-control px-2.5",
};

export function Button({ variant = "secondary", size = "md", children, ...rest }: ButtonProps) {
  return (
    <button
      type="button"
      {...rest}
      className={`inline-flex items-center justify-center gap-2 border transition-colors disabled:cursor-not-allowed disabled:opacity-50 ${
        variant === "icon"
          ? VARIANT_CLASS.icon
          : `${size === "sm" ? SIZE_CLASS.sm : "rounded-control px-3 py-1.5"} text-sm ${VARIANT_CLASS[variant]}`
      }`}
    >
      {children}
    </button>
  );
}
