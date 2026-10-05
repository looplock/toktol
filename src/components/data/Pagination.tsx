/**
 * 分页条（一体胶囊式）。纯受控：不持有页码状态，翻页/改每页行数/跳页意图全部
 * 经回调交出；文案由调用方注入，组件不认 i18n（与其余组件一致）。
 * 页码多做省略号折叠：两端页码常驻，当前页 ±1 展开，中间缝用 … 占位。
 *
 * 视觉：整条收进一个圆角胶囊容器（rounded-full + border），从左到右依次为
 * 总数 → 页容量 chip（灰底胶囊，自绘下拉弹层）→ 分割线 → 圆形翻页钮（当前页
 * accent 实底）→ 分割线 → 跳至（下划线式输入框）。箭头全部自绘 SVG。
 *
 * 页容量不用原生 select：原生下拉弹层无法定制（系统灰高亮、直角白底），
 * 与其余弹层（MultiSelect/DateRangePicker）观感割裂——复用同一套 portal +
 * fixed 定位模式，翻面/夹取/外点/Esc/滚动关闭行为一致。
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { CheckIcon, ChevronIcon, ChevronsIcon } from "../ui/icons";

export interface PaginationLabels {
  readonly nav: string;
  readonly first: string;
  readonly prev: string;
  readonly next: string;
  readonly last: string;
  /** 含 {total}。 */
  readonly total: string;
  /** 页容量控件的可读名（仅 aria-label，视觉上是「50 / 页」chip）。 */
  readonly pageSize: string;
  /** 页容量 chip 里数字后的单位（如「/ 页」）。 */
  readonly pageSizeUnit: string;
  readonly jump: string;
}

interface PaginationProps {
  /** 1 起始。 */
  readonly page: number;
  readonly pageCount: number;
  readonly total: number;
  readonly pageSize: number;
  readonly pageSizeOptions: readonly number[];
  readonly labels: PaginationLabels;
  readonly onPageChange: (page: number) => void;
  readonly onPageSizeChange: (size: number) => void;
  /** 紧凑档：窄栏（如会话页左卡底部）只留前后翻页 + 页码指示 + 总数。 */
  readonly compact?: boolean;
}

type PageItem = number | "ellipsis";

function pageItems(page: number, pageCount: number): readonly PageItem[] {
  if (pageCount <= 7) {
    return Array.from({ length: pageCount }, (_, i) => i + 1);
  }

  const wanted = new Set<number>([1, 2, pageCount - 1, pageCount, page - 1, page, page + 1]);
  const pages = [...wanted].filter((p) => p >= 1 && p <= pageCount).sort((a, b) => a - b);

  const items: PageItem[] = [];
  let previous = 0;

  for (const p of pages) {
    if (p - previous > 1) {
      items.push("ellipsis");
    }
    items.push(p);
    previous = p;
  }

  return items;
}

/** 圆形翻页钮：箭头与页码共用同一轮廓，当前页换 accent 实底。
 * 三层色阶：可点击深墨（ink）＞纯文本中灰（ink-muted）＞禁用极浅（ink-muted/40）。
 */
const ROUND_BUTTON =
  "inline-flex size-7 cursor-pointer items-center justify-center rounded-full text-xs tabular-nums transition-colors";

const ARROW_BUTTON = `${ROUND_BUTTON} text-ink hover:bg-surface-muted disabled:cursor-not-allowed disabled:text-ink-muted/40 disabled:hover:bg-transparent`;

export function Pagination({
  page,
  pageCount,
  total,
  pageSize,
  pageSizeOptions,
  labels,
  onPageChange,
  onPageSizeChange,
  compact = false,
}: PaginationProps) {
  const [jump, setJump] = useState("");

  const chipRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [pageSizeOpen, setPageSizeOpen] = useState(false);
  const [chipBox, setChipBox] = useState<{ left: number; top: number; bottom: number; width: number } | null>(null);
  const [panelSize, setPanelSize] = useState<{ w: number; h: number } | null>(null);

  const closePageSize = useCallback(() => {
    setPageSizeOpen(false);
  }, []);

  function togglePageSize(): void {
    if (pageSizeOpen) {
      closePageSize();
      return;
    }
    const rect = chipRef.current?.getBoundingClientRect();
    if (rect === undefined) {
      return;
    }
    setChipBox({ left: rect.left, top: rect.top, bottom: rect.bottom, width: rect.width });
    setPageSizeOpen(true);
  }

  // 打开后量一次实际尺寸，布局效应保证首帧前纠正（与 MultiSelect 同法）。
  useLayoutEffect(() => {
    if (!pageSizeOpen) {
      setPanelSize(null);
      return;
    }
    const rect = panelRef.current?.getBoundingClientRect();
    if (rect !== undefined) {
      setPanelSize({ w: rect.width, h: rect.height });
    }
  }, [pageSizeOpen]);

  useEffect(() => {
    if (!pageSizeOpen) {
      return undefined;
    }
    function onPointerDown(event: PointerEvent): void {
      const target = event.target;
      if (
        target instanceof Node &&
        !panelRef.current?.contains(target) &&
        !chipRef.current?.contains(target)
      ) {
        setPageSizeOpen(false);
      }
    }
    function onDismiss(): void {
      setPageSizeOpen(false);
    }
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        setPageSizeOpen(false);
      }
    }
    document.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("scroll", onDismiss, true);
    window.addEventListener("resize", onDismiss);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("scroll", onDismiss, true);
      window.removeEventListener("resize", onDismiss);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [pageSizeOpen]);

  function commitJump(): void {
    const target = Number.parseInt(jump, 10);
    setJump("");

    if (!Number.isNaN(target)) {
      onPageChange(Math.min(Math.max(target, 1), pageCount));
    }
  }

  const atStart = page <= 1;
  const atEnd = page >= pageCount;

  // 紧凑档：与完整版同一套圆形翻页钮，只省略页码格/跳页/页容量，供窄栏使用。
  if (compact) {
    return (
      <nav
        aria-label={labels.nav}
        className="flex shrink-0 items-center justify-between gap-2 text-xs text-ink-muted"
      >
        <button
          type="button"
          aria-label={labels.prev}
          disabled={atStart}
          onClick={() => onPageChange(page - 1)}
          className={ARROW_BUTTON}
        >
          <ChevronIcon className="size-3.5" left />
        </button>
        <span className="flex items-baseline gap-2 tabular-nums">
          <span className="font-mono text-ink">
            {page} / {pageCount}
          </span>
          {labels.total.replace("{total}", String(total))}
        </span>
        <button
          type="button"
          aria-label={labels.next}
          disabled={atEnd}
          onClick={() => onPageChange(page + 1)}
          className={ARROW_BUTTON}
        >
          <ChevronIcon className="size-3.5" right />
        </button>
      </nav>
    );
  }

  let pageSizePanel = null;
  if (pageSizeOpen && chipBox !== null) {
    const w = panelSize?.w ?? chipBox.width;
    const h = panelSize?.h ?? pageSizeOptions.length * 30 + 8;
    const spaceBelow = window.innerHeight - chipBox.bottom;
    // 下方放得下就放下方；放不下且上方放得下才翻上去，否则宁可溢出（可滚动）。
    const below = spaceBelow >= h + 8 || chipBox.top < h + 8;
    const left = Math.min(Math.max(chipBox.left, 8), window.innerWidth - w - 8);
    const top = below ? chipBox.bottom + 6 : chipBox.top - 6 - h;

    pageSizePanel = createPortal(
      <div
        ref={panelRef}
        role="listbox"
        aria-label={labels.pageSize}
        style={{ left, top, minWidth: chipBox.width }}
        className="fixed z-50 rounded-control border border-border bg-surface-raised py-1 shadow-raised"
      >
        {pageSizeOptions.map((size) => {
          const current = size === pageSize;
          return (
            <button
              key={size}
              type="button"
              role="option"
              aria-selected={current}
              onClick={() => {
                onPageSizeChange(size);
                closePageSize();
              }}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left font-mono text-xs tabular-nums transition-colors hover:bg-row-hover"
            >
              <span className={current ? "text-accent" : "text-ink"}>{size}</span>
              {current ? <CheckIcon className="ml-auto size-3 shrink-0 text-accent" /> : null}
            </button>
          );
        })}
      </div>,
      document.body,
    );
  }

  return (
    <nav aria-label={labels.nav} className="flex shrink-0 justify-end">
      <div className="inline-flex h-10 items-center gap-3 rounded-full border border-border bg-surface px-4 text-xs">
        <span className="tabular-nums text-ink">
          {labels.total.replace("{total}", String(total))}
        </span>

        <button
          ref={chipRef}
          type="button"
          onClick={togglePageSize}
          aria-expanded={pageSizeOpen}
          aria-haspopup="listbox"
          aria-label={labels.pageSize}
          className="flex h-[26px] shrink-0 cursor-pointer items-center gap-1 rounded-full bg-surface-subtle pr-2 pl-2.5 text-xs transition-colors hover:bg-surface-muted"
        >
          <span className="font-mono tabular-nums text-ink">{pageSize}</span>
          <span className="text-ink-muted">{labels.pageSizeUnit}</span>
          <ChevronIcon
            className={`size-3 text-ink-muted transition-transform ${pageSizeOpen ? "rotate-180" : ""}`}
          />
        </button>

        <span aria-hidden="true" className="h-4 w-px shrink-0 bg-border" />

        <button
          type="button"
          aria-label={labels.first}
          disabled={atStart}
          onClick={() => onPageChange(1)}
          className={ARROW_BUTTON}
        >
          <ChevronsIcon className="size-3.5" direction="left" />
        </button>
        <button
          type="button"
          aria-label={labels.prev}
          disabled={atStart}
          onClick={() => onPageChange(page - 1)}
          className={ARROW_BUTTON}
        >
          <ChevronIcon className="size-3.5" left />
        </button>

        {pageItems(page, pageCount).map((item) =>
          item === "ellipsis" ? (
            <span key={`gap-${item}`} aria-hidden="true" className="w-5 shrink-0 text-center text-ink-muted">
              …
            </span>
          ) : (
            <button
              key={item}
              type="button"
              aria-current={item === page ? "page" : undefined}
              onClick={() => onPageChange(item)}
              className={
                item === page
                  ? `${ROUND_BUTTON} bg-accent font-medium text-accent-ink`
                  : `${ROUND_BUTTON} text-ink hover:bg-surface-muted`
              }
            >
              {item}
            </button>
          ),
        )}

        <button
          type="button"
          aria-label={labels.next}
          disabled={atEnd}
          onClick={() => onPageChange(page + 1)}
          className={ARROW_BUTTON}
        >
          <ChevronIcon className="size-3.5" right />
        </button>
        <button
          type="button"
          aria-label={labels.last}
          disabled={atEnd}
          onClick={() => onPageChange(pageCount)}
          className={ARROW_BUTTON}
        >
          <ChevronsIcon className="size-3.5" direction="right" />
        </button>

        <span aria-hidden="true" className="h-4 w-px shrink-0 bg-border" />

        <label className="flex shrink-0 items-center gap-1.5">
          <span className="text-ink-muted">{labels.jump}</span>
          <input
            type="text"
            inputMode="numeric"
            value={jump}
            onChange={(event) => setJump(event.target.value.replace(/\D/g, ""))}
            onKeyDown={(event) => {
              if (event.key === "Enter") commitJump();
            }}
            onBlur={commitJump}
            aria-label={`${labels.jump} 1-${pageCount}`}
            className="h-[26px] w-10 border-b border-border bg-transparent text-center text-ink tabular-nums transition-colors focus:border-accent focus:outline-none"
          />
          <span className="tabular-nums text-ink-muted">/ {pageCount}</span>
        </label>
      </div>
      {pageSizePanel}
    </nav>
  );
}
