/**
 * 多选下拉筛选：幽灵按钮 + portal 弹层（搜索框 + 复选选项 + 计数 + 清除）。
 * 弹层 portal 到 body 用 fixed 定位——筛选栏在 overflow 容器旁，普通绝对定位
 * 会被相邻结构裁掉/遮住；位置在布局效应里按面板实际尺寸纠正，下方放不下翻到
 * 上方，贴视口边夹取。外点/Esc/滚动关闭。
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { BrandIcon } from "../ui/BrandIcon";
import { CheckIcon, ChevronIcon, SearchIcon } from "../ui/icons";
import type { BrandIconAsset } from "../../lib/brandIcons";

export interface MultiSelectOption {
  readonly value: string;
  readonly label: string;
  /** 该选项在当前数据里的条数；缺省不展示（无分面计数的维度，如会话页工具）。 */
  readonly count?: number;
  /** 可选品牌图标（工具/模型维度用），缺失就不占位。 */
  readonly icon?: BrandIconAsset | undefined;
}

interface Anchor {
  readonly left: number;
  readonly top: number;
  readonly bottom: number;
}

const PANEL_WIDTH_ESTIMATE = 240;
const PANEL_HEIGHT_ESTIMATE = 300;

export function MultiSelect({
  label,
  options,
  selected,
  onToggle,
  onClear,
  searchPlaceholder,
  clearLabel,
  noMatchLabel,
  bordered = false,
}: {
  readonly label: string;
  readonly options: readonly MultiSelectOption[];
  readonly selected: ReadonlySet<string>;
  readonly onToggle: (value: string) => void;
  readonly onClear: () => void;
  readonly searchPlaceholder: string;
  readonly clearLabel: string;
  readonly noMatchLabel: string;
  /** 与带框控件（搜索框）同排时加描边，独占工具条时保持幽灵样式。 */
  readonly bordered?: boolean;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const [panelSize, setPanelSize] = useState<{ w: number; h: number } | null>(null);
  const [query, setQuery] = useState("");

  const close = useCallback(() => {
    setOpen(false);
    setQuery("");
  }, []);

  function toggle(): void {
    if (open) {
      close();
      return;
    }
    const rect = triggerRef.current?.getBoundingClientRect();
    if (rect === undefined) {
      return;
    }
    setAnchor({ left: rect.left, top: rect.top, bottom: rect.bottom });
    setOpen(true);
  }

  // 打开后量一次实际尺寸（w-max/内容变化都会让估算失准），布局效应保证首帧前纠正。
  useLayoutEffect(() => {
    if (!open) {
      setPanelSize(null);
      return;
    }
    const rect = panelRef.current?.getBoundingClientRect();
    if (rect !== undefined) {
      setPanelSize({ w: rect.width, h: rect.height });
    }
  }, [open]);

  useEffect(() => {
    if (!open) {
      return undefined;
    }
    function onPointerDown(event: PointerEvent): void {
      const target = event.target;
      if (
        target instanceof Node &&
        !panelRef.current?.contains(target) &&
        !triggerRef.current?.contains(target)
      ) {
        setOpen(false);
        setQuery("");
      }
    }
    function onDismiss(): void {
      setOpen(false);
      setQuery("");
    }
    function onScroll(event: Event): void {
      // 面板自身选项列表的滚动属于弹层内部行为，不关闭；其余滚动（页面/
      // 背后表格）才视为锚点失位。
      if (event.target instanceof Node && panelRef.current?.contains(event.target)) {
        return;
      }
      onDismiss();
    }
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        setOpen(false);
        setQuery("");
      }
    }
    document.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onDismiss);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onDismiss);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  const needle = query.trim().toLowerCase();
  const visible =
    needle === "" ? options : options.filter((o) => o.label.toLowerCase().includes(needle));

  let panel: ReactNode = null;
  if (open && anchor !== null) {
    const w = panelSize?.w ?? PANEL_WIDTH_ESTIMATE;
    const h = panelSize?.h ?? PANEL_HEIGHT_ESTIMATE;
    const spaceBelow = window.innerHeight - anchor.bottom;
    // 下方放得下就放下方；放不下且上方放得下才翻上去，否则宁可溢出（可滚动）。
    const below = spaceBelow >= Math.min(h + 8, 280) || anchor.top < h + 8;
    const left = Math.min(Math.max(anchor.left, 8), window.innerWidth - w - 8);
    const top = below ? anchor.bottom + 6 : anchor.top - 6 - h;

    panel = createPortal(
      <div
        ref={panelRef}
        aria-label={label}
        style={{ left, top }}
        className="fixed z-50 w-60 rounded-control border border-border bg-surface-raised py-2 shadow-raised"
      >
        <div className="px-2.5 pb-2">
          <div className="flex h-8 items-center gap-1.5 rounded-control border border-border px-2">
            <SearchIcon className="size-3.5 shrink-0 text-ink-muted" />
            <input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={searchPlaceholder}
              className="h-full w-full min-w-0 bg-transparent text-xs text-ink placeholder:text-ink-muted/60 focus:outline-none"
            />
          </div>
        </div>
        <div className="max-h-64 overflow-y-auto">
          {visible.map((option) => {
            const checked = selected.has(option.value);
            return (
              <button
                key={option.value}
                type="button"
                onClick={() => onToggle(option.value)}
                aria-pressed={checked}
                className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left transition-colors hover:bg-row-hover"
              >
                <span
                  aria-hidden="true"
                  className={`flex size-3.5 shrink-0 items-center justify-center rounded-control border ${
                    checked ? "border-accent bg-accent" : "border-border bg-surface"
                  }`}
                >
                  {checked ? <CheckIcon className="size-2.5 text-white" /> : null}
                </span>
                {option.icon === undefined ? null : (
                  <BrandIcon icon={option.icon} size={14} />
                )}
                <span className="truncate font-mono text-xs text-ink">{option.label}</span>
                {option.count === undefined ? null : (
                  <span className="ml-auto flex items-center gap-1.5 pl-2 font-mono text-xs tabular-nums text-ink-muted">
                    {option.count}
                  </span>
                )}
              </button>
            );
          })}
          {visible.length === 0 ? (
            <div className="px-2.5 py-3 text-center text-xs text-ink-muted">{noMatchLabel}</div>
          ) : null}
        </div>
        <div className="mt-1 border-t border-border px-2.5 pt-1.5">
          <button
            type="button"
            onClick={onClear}
            disabled={selected.size === 0}
            className="text-xs text-accent disabled:text-ink-muted/50"
          >
            {clearLabel}
          </button>
        </div>
      </div>,
      document.body,
    );
  }

  const active = selected.size > 0;

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={toggle}
        aria-expanded={open}
        aria-label={label}
        className={`inline-flex h-8 shrink-0 items-center gap-1 whitespace-nowrap rounded-control px-2.5 text-xs transition-colors hover:bg-row-hover ${
          active ? "font-medium text-ink-strong" : "text-ink-muted"
        } ${bordered ? "border border-border" : ""}`}
      >
        {label}
        {active ? <span className="font-mono tabular-nums">· {selected.size}</span> : null}
        <ChevronIcon className={`size-3.5 transition-transform ${open ? "rotate-180" : ""}`} />
      </button>
      {panel}
    </>
  );
}
