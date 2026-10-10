/**
 * 左侧导航（配置页工具列表、设置页分区、网关控制台共用）：卡片式侧栏 +
 * 高亮按钮列。三处原本各写一份按钮样式且高亮细节已经漂移（字重、悬浮色、
 * aria-current 取值），这里统一；图标槽收 ReactNode——各页的图标来源不同
 * （ToolIcon 的内联 SVG / 品牌图标组件），统一组件不该知道它们。
 */

import type { ReactNode } from "react";

import { Card } from "./Card";

export interface SideNavItem<T extends string> {
  readonly id: T;
  readonly label: string;
  /** 可选图标：由调用方渲染好传入，选中态着色交给按钮的 currentColor。 */
  readonly icon?: ReactNode;
  /** 右侧弱化小字（如网关各面板的计数）。 */
  readonly meta?: string;
}

interface SideNavProps<T extends string> {
  /** 传给 Card as="nav" 的 aria-label。 */
  readonly label: string;
  /** 顶部分区标题；缺省没有——导航即页名的页面（配置/设置）不重复标题。 */
  readonly title?: string;
  readonly items: readonly SideNavItem<T>[];
  readonly active: T;
  readonly onChange: (id: T) => void;
}

export function SideNav<T extends string>({
  label,
  title,
  items,
  active,
  onChange,
}: SideNavProps<T>) {
  return (
    <Card as="nav" aria-label={label} raised padding="p-2" className="self-stretch">
      {title !== undefined ? (
        <p className="px-3 py-2 text-sm font-semibold">{title}</p>
      ) : null}
      {/* 项间留 1 档空隙：相邻项的高亮/悬浮底色不能连成一片。 */}
      <div className="space-y-1">
        {items.map((item) => {
          const selected = item.id === active;

          return (
            <button
              key={item.id}
              type="button"
              onClick={() => onChange(item.id)}
              aria-current={selected ? "page" : undefined}
              className={`flex w-full items-center gap-2 rounded-control px-3 py-2 text-left text-sm transition-colors ${
                selected
                  ? "bg-accent/10 font-medium text-ink"
                  : "text-ink-muted hover:bg-accent/5 hover:text-ink"
              }`}
            >
              {item.icon !== undefined ? (
                <span className="flex size-4 shrink-0 items-center justify-center">
                  {item.icon}
                </span>
              ) : null}
              <span className="min-w-0 flex-1 truncate">{item.label}</span>
              {item.meta !== undefined ? (
                <span className="shrink-0 text-xs text-ink-muted">{item.meta}</span>
              ) : null}
            </button>
          );
        })}
      </div>
    </Card>
  );
}
