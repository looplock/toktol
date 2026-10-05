// 手柄常驻 DOM、只改可见性：条件渲染的话，拖到一半指针移出卡片会让 gridstack 丢掉绑定。
// 顶部带子同样常驻，手柄出现时不会把图表往下推。

import type { ReactNode } from "react";

import "../../styles/dashboard.css";

interface DashboardCardProps {
  readonly id: string;
  readonly x: number;
  readonly y: number;
  readonly w: number;
  readonly h: number;
  readonly title: string;
  readonly subtitle?: string;
  /** 悬浮才出现的卡片自配置（指标、图表类型等）。 */
  readonly actions?: ReactNode;
  /** 内容高度固定（非图表）的卡片开启：缩得比内容小时纵向滚动，而不是裁掉。 */
  readonly scrollBody?: boolean;
  readonly children: ReactNode;
}

export function DashboardCard({
  id,
  x,
  y,
  w,
  h,
  title,
  subtitle,
  actions,
  scrollBody = false,
  children,
}: DashboardCardProps) {
  return (
    <div className="tt-card grid-stack-item" gs-id={id} gs-x={x} gs-y={y} gs-w={w} gs-h={h}>
      <div className="tt-card-inner grid-stack-item-content">
        {/* L 角标与边中点刻度只是视觉提示；真正的热区是 gridstack 的八向透明手柄。 */}
        <span className="tt-rz-corner tl" aria-hidden="true" />
        <span className="tt-rz-corner tr" aria-hidden="true" />
        <span className="tt-rz-corner bl" aria-hidden="true" />
        <span className="tt-rz-corner br" aria-hidden="true" />
        <span className="tt-rz-edge t" aria-hidden="true" />
        <span className="tt-rz-edge b" aria-hidden="true" />
        <span className="tt-rz-edge l" aria-hidden="true" />
        <span className="tt-rz-edge r" aria-hidden="true" />
        <div className="tt-grip-band">
          <span className="tt-grip" aria-hidden="true">
            {Array.from({ length: 6 }, (_, index) => (
              <i key={index} />
            ))}
          </span>
        </div>

        <header className="tt-card-head">
          <h2 className="truncate text-sm font-semibold">{title}</h2>
          {subtitle === undefined ? null : (
            <span className="truncate text-xs text-ink-muted">{subtitle}</span>
          )}
          <div className="tt-card-actions">{actions}</div>
        </header>

        <div className={`tt-card-body${scrollBody ? " tt-scroll" : ""}`}>{children}</div>
      </div>
    </div>
  );
}
