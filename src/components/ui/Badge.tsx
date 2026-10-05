/**
 * 徽章。tone 直接对应语义令牌，不新增"看起来差不多"的第四种灰。
 * `unknown` 是给隐私红线用的：未定价模型显示未知，绝不编造数字。
 */

import type { ReactNode } from "react";

export type BadgeTone = "neutral" | "up" | "down" | "ok" | "danger" | "unknown";

interface BadgeProps {
  readonly tone?: BadgeTone;
  /** 全圆角胶囊。工具与模型名用它：两者都是"标签"语义，不该长得像状态徽章。 */
  readonly pill?: boolean;
  readonly children: ReactNode;
}

const TONE_CLASS: Record<BadgeTone, string> = {
  // 有底色的档不描边：灰胶囊加边框只会显重（用户反馈）。
  neutral: "bg-surface-subtle text-ink-muted",
  up: "bg-surface-subtle text-up",
  down: "bg-surface-subtle text-down",
  ok: "bg-surface-subtle text-ok",
  danger: "bg-surface-subtle text-danger",
  // 透明底要自己带边框宽度：虚线是"这里没有值"的语义载体。
  unknown: "border border-dashed border-border bg-transparent text-ink-muted",
};

export function Badge({ tone = "neutral", pill = false, children }: BadgeProps) {
  return (
    <span
      className={`inline-flex max-w-full items-center gap-1.5 px-2.5 py-0.5 text-xs ${
        pill ? "rounded-full" : "rounded-control"
      } ${TONE_CLASS[tone]}`}
    >
      {children}
    </span>
  );
}
