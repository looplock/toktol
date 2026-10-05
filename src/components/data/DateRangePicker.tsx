/**
 * 时间段选择器：按钮（日历图标 + 当前预设名或自定义区间）+ portal 双栏弹层。
 * 左栏：预设竖排（选中带勾）+ 自定义开始/结束日期输入；右栏：单月日历点选区间。
 * 点第一个日期 = 起点（高亮待选），点第二个日期 = 终点并生效；早于起点自动交换。
 * 定位/关闭策略与 MultiSelect 一致（fixed + 外点/Esc/滚动关闭）。
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { CalendarIcon, CheckIcon, ChevronIcon } from "../ui/icons";

export interface DateRange {
  /** 起点当日 00:00（本地时区）。 */
  readonly start: number;
  /** 终点当日 23:59:59.999（本地时区）。 */
  readonly end: number;
}

export type TimePreset = "today" | "24h" | "7d" | "30d" | "all";

export interface TimeSelection {
  readonly preset: TimePreset | "custom";
  readonly range: DateRange | null;
}

const DAY = 86_400_000;

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** 预设 → 绝对区间（按选择时刻计算）；all = 不限（null）。 */
export function presetRange(preset: TimePreset, now: number): DateRange | null {
  const today = startOfDay(now);
  switch (preset) {
    case "all":
      return null;
    case "today":
      return { start: today, end: today + DAY - 1 };
    case "24h":
      return { start: now - DAY, end: now };
    case "7d":
      return { start: today - 6 * DAY, end: today + DAY - 1 };
    case "30d":
      return { start: today - 29 * DAY, end: today + DAY - 1 };
  }
}

function toInputValue(ms: number): string {
  const d = new Date(ms);
  const pad = (v: number): string => String(v).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function fromInputValue(text: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (m === null) {
    return null;
  }
  return startOfDay(new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime());
}

interface Anchor {
  readonly left: number;
  readonly top: number;
  readonly bottom: number;
}

const PANEL_WIDTH_ESTIMATE = 400;
const PANEL_HEIGHT_ESTIMATE = 280;

export function DateRangePicker({
  label,
  active,
  value,
  onChange,
  presets,
  customLabel,
  startLabel,
  endLabel,
  weekdays,
}: {
  readonly label: string;
  /** 是否处于非"全部"状态（按钮高亮用）。 */
  readonly active: boolean;
  readonly value: TimeSelection;
  readonly onChange: (next: TimeSelection) => void;
  readonly presets: readonly { readonly id: TimePreset; readonly label: string }[];
  /** 左栏自定义日期输入的小标题。 */
  readonly customLabel: string;
  readonly startLabel: string;
  readonly endLabel: string;
  /** 星期表头，7 个短标签，周一开头。 */
  readonly weekdays: readonly string[];
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const [panelSize, setPanelSize] = useState<{ w: number; h: number } | null>(null);
  /** 日历当前展示的月份；null = 未打开。 */
  const [view, setView] = useState<{ year: number; month: number } | null>(null);
  /** 已点下的区间起点（等第二次点击收终点）。 */
  const [pendingStart, setPendingStart] = useState<number | null>(null);

  const close = useCallback(() => {
    setOpen(false);
    setPendingStart(null);
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
    const base = value.range !== null ? new Date(value.range.end) : new Date();
    setView({ year: base.getFullYear(), month: base.getMonth() });
    setPendingStart(null);
    setAnchor({ left: rect.left, top: rect.top, bottom: rect.bottom });
    setOpen(true);
  }

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
        setPendingStart(null);
      }
    }
    function onDismiss(): void {
      setOpen(false);
      setPendingStart(null);
    }
    function onKeyDown(event: KeyboardEvent): void {
      if (event.key === "Escape") {
        setOpen(false);
        setPendingStart(null);
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
  }, [open]);

  function pickPreset(preset: TimePreset): void {
    setPendingStart(null);
    onChange({ preset, range: presetRange(preset, Date.now()) });
  }

  function pickDay(day: number): void {
    if (pendingStart === null) {
      setPendingStart(day);
      return;
    }
    const a = Math.min(pendingStart, day);
    const b = Math.max(pendingStart, day);
    setPendingStart(null);
    onChange({ preset: "custom", range: { start: a, end: b + DAY - 1 } });
  }

  function setStart(text: string): void {
    const ms = fromInputValue(text);
    if (ms === null) {
      return;
    }
    const end = value.range !== null ? Math.max(value.range.end, ms + DAY - 1) : ms + DAY - 1;
    setPendingStart(null);
    onChange({ preset: "custom", range: { start: ms, end } });
  }

  function setEnd(text: string): void {
    const ms = fromInputValue(text);
    if (ms === null) {
      return;
    }
    const start = value.range !== null ? Math.min(value.range.start, ms) : ms;
    setPendingStart(null);
    onChange({ preset: "custom", range: { start, end: ms + DAY - 1 } });
  }

  let panel: ReactNode = null;
  if (open && anchor !== null && view !== null) {
    const w = panelSize?.w ?? PANEL_WIDTH_ESTIMATE;
    const h = panelSize?.h ?? PANEL_HEIGHT_ESTIMATE;
    const spaceBelow = window.innerHeight - anchor.bottom;
    const below = spaceBelow >= Math.min(h + 8, 340) || anchor.top < h + 8;
    const left = Math.min(Math.max(anchor.left, 8), window.innerWidth - w - 8);
    const top = below ? anchor.bottom + 6 : anchor.top - 6 - h;

    // 单月网格，周一开头：先补上月尾占位，再补到整周。
    const first = new Date(view.year, view.month, 1);
    const offset = (first.getDay() + 6) % 7;
    const dayCount = new Date(view.year, view.month + 1, 0).getDate();
    const cells: (number | null)[] = Array.from({ length: offset }, () => null);
    for (let day = 1; day <= dayCount; day += 1) {
      cells.push(new Date(view.year, view.month, day).getTime());
    }
    while (cells.length % 7 !== 0) {
      cells.push(null);
    }

    const rangeStart = value.range !== null ? startOfDay(value.range.start) : null;
    const rangeEnd = value.range !== null ? startOfDay(value.range.end) : null;
    const pad = (v: number): string => String(v).padStart(2, "0");

    panel = createPortal(
      <div
        ref={panelRef}
        aria-label={label}
        style={{ left, top }}
        className="fixed z-50 flex w-[400px] rounded-control border border-border bg-surface-raised shadow-raised"
      >
        <div className="flex w-36 shrink-0 flex-col gap-0.5 border-r border-border p-1.5">
          {presets.map((preset) => {
            const current = value.preset === preset.id;
            return (
              <button
                key={preset.id}
                type="button"
                onClick={() => pickPreset(preset.id)}
                aria-pressed={current}
                className={`flex items-center justify-between rounded-control px-2.5 py-1.5 text-left text-xs transition-colors ${
                  current
                    ? "bg-accent/10 font-medium text-ink-strong"
                    : "text-ink-muted hover:bg-row-hover hover:text-ink"
                }`}
              >
                {preset.label}
                {current ? <CheckIcon className="size-3 shrink-0 text-accent" /> : null}
              </button>
            );
          })}
          <div className="mx-1.5 my-1 h-px bg-border" />
          <span className="px-1.5 pb-0.5 text-[10px] text-ink-muted">{customLabel}</span>
          <input
            type="date"
            aria-label={startLabel}
            value={value.range !== null ? toInputValue(value.range.start) : ""}
            onChange={(event) => setStart(event.target.value)}
            className="h-7 rounded-control border border-border bg-surface px-1.5 font-mono text-xs text-ink focus:border-accent focus:outline-none"
          />
          <input
            type="date"
            aria-label={endLabel}
            value={value.range !== null ? toInputValue(value.range.end) : ""}
            onChange={(event) => setEnd(event.target.value)}
            className="h-7 rounded-control border border-border bg-surface px-1.5 font-mono text-xs text-ink focus:border-accent focus:outline-none"
          />
        </div>
        <div className="min-w-0 flex-1 p-3">
          <div className="mb-2 flex items-center justify-between">
            <button
              type="button"
              aria-label="<"
              onClick={() => {
                const next = new Date(view.year, view.month - 1, 1);
                setView({ year: next.getFullYear(), month: next.getMonth() });
              }}
              className="flex size-6 items-center justify-center rounded-control text-ink-muted transition-colors hover:bg-row-hover hover:text-ink"
            >
              <ChevronIcon left className="size-3.5" />
            </button>
            <span className="font-mono text-xs tabular-nums text-ink">
              {view.year}-{pad(view.month + 1)}
            </span>
            <button
              type="button"
              aria-label=">"
              onClick={() => {
                const next = new Date(view.year, view.month + 1, 1);
                setView({ year: next.getFullYear(), month: next.getMonth() });
              }}
              className="flex size-6 items-center justify-center rounded-control text-ink-muted transition-colors hover:bg-row-hover hover:text-ink"
            >
              <ChevronIcon right className="size-3.5" />
            </button>
          </div>
          <div className="grid grid-cols-7">
            {weekdays.map((day) => (
              <span key={day} className="py-1 text-center text-[10px] text-ink-muted">
                {day}
              </span>
            ))}
          </div>
          <div className="grid grid-cols-7 gap-y-0.5">
            {cells.map((ms, index) => {
              if (ms === null) {
                return <span key={`blank-${index}`} />;
              }
              const inRange =
                rangeStart !== null && rangeEnd !== null && ms >= rangeStart && ms <= rangeEnd;
              const isStart = ms === pendingStart || (rangeStart !== null && ms === rangeStart);
              const isEnd = rangeEnd !== null && ms === rangeEnd;
              // 区间带两头圆角、中间不断开，读起来是连续的一段。
              const radius =
                isStart && isEnd
                  ? "rounded-control"
                  : isStart
                    ? "rounded-l-md"
                    : isEnd
                      ? "rounded-r-md"
                      : "";
              return (
                <button
                  key={ms}
                  type="button"
                  onClick={() => pickDay(ms)}
                  className={`flex h-7 items-center justify-center text-xs tabular-nums transition-colors ${
                    isStart || isEnd
                      ? `bg-accent font-medium text-white ${radius}`
                      : inRange
                        ? "bg-accent/10 text-ink"
                        : "rounded-control text-ink-muted hover:bg-row-hover hover:text-ink"
                  }`}
                >
                  {new Date(ms).getDate()}
                </button>
              );
            })}
          </div>
        </div>
      </div>,
      document.body,
    );
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={toggle}
        aria-expanded={open}
        aria-label={label}
        className={`inline-flex h-8 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-control px-2.5 text-xs transition-colors hover:bg-row-hover ${
          active || open ? "bg-accent/10 font-medium text-ink-strong" : "text-ink-muted"
        }`}
      >
        <CalendarIcon className="size-3.5 shrink-0" />
        {label}
        <ChevronIcon className={`size-3.5 transition-transform ${open ? "rotate-180" : ""}`} />
      </button>
      {panel}
    </>
  );
}
