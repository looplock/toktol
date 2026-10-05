// 拖拽只认手柄：整卡可拖的话，点一下图例就把卡片拖走了。

import { GridStack } from "gridstack";
import { useEffect, useRef, type ReactNode } from "react";

import { LAYOUT_COLUMNS, type LayoutWidget } from "../../lib/overview/layout";

import "gridstack/dist/gridstack.min.css";
import "../../styles/dashboard.css";

/** 松手 300ms 后写盘：拖动过程中每帧都存会让存储跟不上一帧。 */
const SAVE_DEBOUNCE_MS = 300;

interface DashboardGridProps {
  readonly className?: string | undefined;
  readonly onWidgetsChange: (widgets: readonly LayoutWidget[]) => void;
  readonly children: ReactNode;
}

export function DashboardGrid({ className, onWidgetsChange, children }: DashboardGridProps) {
  const host = useRef<HTMLDivElement>(null);
  const latestChange = useRef(onWidgetsChange);
  latestChange.current = onWidgetsChange;

  useEffect(() => {
    const element = host.current;
    if (element === null) return;

    const grid = GridStack.init(
      {
        // 列数决定横向吸附粒度（容器宽/列数），要跟纵向的行高（44px）一样细：
        // 48 列在约 2100px 宽以内的任何窗口下，横向都不比纵向粗。
        column: LAYOUT_COLUMNS,
        cellHeight: 44,
        margin: 6,
        mode: "top",
        animate: true,
        draggable: { handle: ".tt-grip" },
        resizable: { handles: "n,e,s,w,ne,nw,se,sw" },
        alwaysShowResizeHandle: false,
      },
      element,
    );

    if (grid === null) return;

    let timer: number | undefined;

    const emit = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        const saved = grid.save(false) as LayoutWidget[] | undefined;
        if (saved === undefined) return;

        latestChange.current(
          saved.map((widget) => ({
            id: widget.id,
            x: widget.x,
            y: widget.y,
            w: widget.w,
            h: widget.h,
          })),
        );
      }, SAVE_DEBOUNCE_MS);
    };

    grid.on("change", emit);

    return () => {
      window.clearTimeout(timer);
      grid.destroy(false);
    };
  }, []);

  return (
    <div className={className === undefined ? "grid-stack" : `grid-stack ${className}`} ref={host}>
      {children}
    </div>
  );
}
