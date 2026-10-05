/**
 * 卡片壳：rounded-card + border + 底色 + 内边距的唯一出处。
 * 默认与画布同色、靠边框区分（亮色不堆灰阶）；`raised` 换浮起底色（侧栏、
 * 内容面板这类要抬一档的容器）；`dashed` 虚线给空状态。弹层的阴影各自带
 * shadow-raised，不在这里。布局类（滚动、弹性）与自定义留白走 className +
 * padding="none"；语义标签用 as 切换，其余属性（aria-label、hidden 等）原样透传。
 */

import type { ComponentPropsWithoutRef } from "react";

const PADDING = { "p-5": "p-5", "p-4": "p-4", "p-2": "p-2", none: "" } as const;

interface CardProps extends ComponentPropsWithoutRef<"div"> {
  readonly as?: "div" | "nav" | "section";
  readonly raised?: boolean;
  readonly dashed?: boolean;
  readonly padding?: keyof typeof PADDING;
}

export function Card({
  as: Tag = "div",
  raised = false,
  dashed = false,
  padding = "p-5",
  className = "",
  children,
  ...rest
}: CardProps) {
  return (
    <Tag
      className={`rounded-card border border-border ${dashed ? "border-dashed" : ""} ${
        raised ? "bg-surface-raised" : "bg-surface"
      } ${PADDING[padding]} ${className}`}
      {...rest}
    >
      {children}
    </Tag>
  );
}
