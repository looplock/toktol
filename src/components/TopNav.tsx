/**
 * 顶部 page 导航。不认识 i18n（标签由调用方传），遵循 tablist 键盘约定：
 * roving tabindex + 方向键 / Home / End，移动即选中。
 *
 * 选中态是一枚滑动胶囊：单个绝对定位的指示条，在布局效应里量出目标按钮的
 * 位置，用 CSS transition 滑过去——切换动效顺滑，且不引入动画库。
 * 胶囊既是选中指示也是焦点指示（roving tabindex 下焦点永远跟着选中走）。
 */

import { useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";

import { stepPage, type PageId } from "../lib/routes";

export interface NavItem {
  readonly id: PageId;
  readonly label: string;
}

interface TopNavProps {
  readonly items: readonly NavItem[];
  readonly active: PageId;
  readonly navLabel: string;
  readonly onChange: (page: PageId) => void;
}

export function TopNav({ items, active, navLabel, onChange }: TopNavProps) {
  const tabs = useRef(new Map<PageId, HTMLButtonElement>());
  const [pill, setPill] = useState<{ left: number; width: number } | null>(null);

  // 选中变化后量目标按钮：offsetLeft/offsetWidth 相对 tablist（relative 容器）。
  // 首帧胶囊直接出现在最终位置（元素此时才挂载，无过渡起点，不会从 0 滑入）。
  useLayoutEffect(() => {
    function measure(): void {
      const node = tabs.current.get(active);
      if (node === undefined) return;
      const left = node.offsetLeft;
      const width = node.offsetWidth;
      setPill((prev) => (prev !== null && prev.left === left && prev.width === width ? prev : { left, width }));
    }

    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [active, items]);

  // 字体异步加载完成后按钮宽度会变，胶囊位置需要校准一次。
  useEffect(() => {
    document.fonts?.ready.then(() => {
      const node = tabs.current.get(active);
      if (node !== undefined) {
        setPill({ left: node.offsetLeft, width: node.offsetWidth });
      }
    });
  }, [active]);

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const first = items[0]?.id;
    const last = items[items.length - 1]?.id;

    let target: PageId | undefined;
    if (event.key === "ArrowRight") target = stepPage(active, 1);
    else if (event.key === "ArrowLeft") target = stepPage(active, -1);
    else if (event.key === "Home") target = first;
    else if (event.key === "End") target = last;

    if (target === undefined) return;

    event.preventDefault();
    onChange(target);
    // 焦点跟着选中走，否则高亮和焦点会分家。
    tabs.current.get(target)?.focus();
  }

  return (
    // 外壳（边框、内边距、居中）由调用方负责，导航只管 tab 本身。
    // 焦点环刻意关掉：roving tabindex 下焦点永远跟着选中走，选中胶囊就是
    // 焦点指示器；再画一个环，滚动手势把 :focus-visible 启发式带活时会
    // 在选中 tab 外面再套一圈紫环，纯噪音。
    <nav aria-label={navLabel} className="min-w-0">
      <div
        role="tablist"
        onKeyDown={handleKeyDown}
        className="relative flex justify-center gap-1 overflow-x-auto"
      >
        {/* 滑动胶囊：绝对定位铺满行高，随选中平移；挂在按钮之前，按钮文字覆盖其上。 */}
        {pill !== null ? (
          <div
            aria-hidden="true"
            style={{ width: pill.width, transform: `translateX(${pill.left}px)` }}
            className="absolute top-0 left-0 h-full rounded-full bg-accent transition-[transform,width] duration-150 ease-out"
          />
        ) : null}
        {items.map((item) => {
          const selected = item.id === active;

          return (
            <button
              key={item.id}
              ref={(node) => {
                if (node) tabs.current.set(item.id, node);
                else tabs.current.delete(item.id);
              }}
              type="button"
              role="tab"
              id={`tab-${item.id}`}
              aria-selected={selected}
              aria-controls={`panel-${item.id}`}
              tabIndex={selected ? 0 : -1}
              onClick={() => onChange(item.id)}
              className={`relative shrink-0 rounded-full px-4 py-1.5 text-sm transition-colors duration-150 focus-visible:outline-none ${
                selected ? "cursor-default font-medium text-accent-ink" : "text-ink-muted hover:bg-surface-muted hover:text-ink"
              }`}
            >
              {item.label}
            </button>
          );
        })}
      </div>
    </nav>
  );
}
