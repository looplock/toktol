// 卡片空状态（方案 A · 静默留白）：与图表类型对应的细线图标 + 一行灰字，
// 其余全部留白。图标只表意图，不画骨架，避免被误读成真实数据。

import type { ReactNode } from "react";

export type EmptyKind =
  | "donut" // 构成环形 / 占比
  | "bars" // 横向条形
  | "line" // 趋势折线
  | "flow" // 桑基流向
  | "box" // 箱线分布
  | "stairs" // 瀑布阶梯
  | "scatter" // 散点气泡
  | "grid"; // 热力图格子

const ICONS: Record<EmptyKind, ReactNode> = {
  donut: (
    <>
      <circle cx="12" cy="12" r="7.5" />
      <circle cx="12" cy="12" r="3" />
    </>
  ),
  bars: <path d="M4.5 7h11M4.5 12h7.5M4.5 17h14" />,
  line: (
    <>
      <path d="M4 15.5l4.5-5 3.5 3 6-7.5" />
      <path d="M4 20.5h16" />
    </>
  ),
  flow: (
    <>
      <path d="M4 12h5" />
      <path d="M9 12c4 0 4-5.5 8-5.5h3" />
      <path d="M9 12c4 0 4 5.5 8 5.5h3" />
    </>
  ),
  box: (
    <>
      <rect x="6.5" y="9.5" width="11" height="5" />
      <path d="M12 9.5V7m0 7.5V17m-2.5-10h5m-5 10h5" />
    </>
  ),
  stairs: <path d="M4 19h4v-4h4v-4h4V7h4" />,
  scatter: (
    <>
      <circle cx="8" cy="15" r="1.8" />
      <circle cx="13" cy="10" r="2.6" />
      <circle cx="18.5" cy="6.5" r="1.6" />
    </>
  ),
  grid: (
    <>
      <rect x="5" y="5" width="5.5" height="5.5" rx="1" />
      <rect x="13.5" y="5" width="5.5" height="5.5" rx="1" />
      <rect x="5" y="13.5" width="5.5" height="5.5" rx="1" />
      <rect x="13.5" y="13.5" width="5.5" height="5.5" rx="1" />
    </>
  ),
};

export function EmptyState({ kind, label }: { readonly kind: EmptyKind; readonly label: string }) {
  return (
    <div className="flex h-full min-h-0 flex-col items-center justify-center gap-2 text-ink-muted">
      <svg
        width="28"
        height="28"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        aria-hidden="true"
      >
        {ICONS[kind]}
      </svg>
      <p className="text-xs">{label}</p>
    </div>
  );
}
