/**
 * 信息提示（tooltip）：info 图标 + 悬浮面板。
 * 面板不挂在触发器旁边，而是 portal 到 body 用 fixed 定位——表格单元格里
 * overflow-auto 的滚动容器会把普通绝对定位的面板裁掉，portal 是唯一出路。
 * 面板带一个漫画气泡式的小尖（旋转 45° 的方块），永远指向触发图标。
 * 图标是自绘的圆圈 i（circle + 点 + 竖笔），随 currentColor 变色。
 */

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

/** 手绘 info 图标：圆环 + 上点 + 竖笔。点用超短段 + 圆头笔帽画的，不是文本字符。 */
export function InfoIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <circle cx="12" cy="12" r="9" strokeWidth="2.2" />
      <path d="M12 8.1h.01" strokeWidth="3" />
      <path d="M12 11.6v4.6" strokeWidth="2.2" />
    </svg>
  );
}

/** 触发器的屏幕位置：tooltip 依据它做定位、翻转与小尖指向。 */
interface Anchor {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

/** 面板尺寸的保守估计：测量完成前的首帧用，随后的 useLayoutEffect 会纠正。 */
const PANEL_WIDTH_ESTIMATE = 256;
const PANEL_HEIGHT_ESTIMATE = 150;
/** 面板与触发器的间距（小尖从这个缝隙里伸出来）。 */
const PANEL_GAP = 10;
/** 小尖的边长（旋转 45° 后对角线 ≈ 11px）。 */
const CARET_SIZE = 8;
/** 悬停多少毫秒后才弹出：快速划过一列图标时不闪烁。 */
const OPEN_DELAY_MS = 150;

export function InfoTooltip({
  content,
  label,
  panelClassName = "",
  iconClassName = "size-4",
}: {
  readonly content: ReactNode;
  /** 无障碍名：图标本身对读屏器是空的，语义全靠这个标签。 */
  readonly label: string;
  /** 追加到面板上的类（控制宽度/换行等）。 */
  readonly panelClassName?: string;
  readonly iconClassName?: string;
}) {
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<number | undefined>(undefined);
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const [panelSize, setPanelSize] = useState<{ w: number; h: number } | null>(null);

  const close = useCallback(() => {
    window.clearTimeout(timerRef.current);
    setAnchor(null);
  }, []);

  const open = useCallback(() => {
    window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => {
      const rect = triggerRef.current?.getBoundingClientRect();
      if (rect !== undefined) {
        setAnchor({ left: rect.left, top: rect.top, width: rect.width, height: rect.height });
      }
    }, OPEN_DELAY_MS);
  }, []);

  useEffect(() => close, [close]);

  // 打开后先量一次实际尺寸（w-max 面板的宽度估不准），布局效应保证首帧前纠正，
  // 不然小尖的位置和翻转判断都会抖一下。
  useLayoutEffect(() => {
    if (anchor === null) {
      setPanelSize(null);
      return;
    }
    const rect = panelRef.current?.getBoundingClientRect();
    if (rect !== undefined) {
      setPanelSize({ w: rect.width, h: rect.height });
    }
  }, [anchor !== null]);

  // 滚动/改窗口时锚点会失效（fixed 面板不会跟着源走），直接收起最省事也最不骗人。
  useEffect(() => {
    if (anchor === null) {
      return undefined;
    }
    const onScroll = (): void => {
      window.clearTimeout(timerRef.current);
      setAnchor(null);
    };
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [anchor !== null]);

  let panel: ReactNode = null;
  if (anchor !== null) {
    const w = panelSize?.w ?? PANEL_WIDTH_ESTIMATE;
    const h = panelSize?.h ?? PANEL_HEIGHT_ESTIMATE;
    const centerX = anchor.left + anchor.width / 2;

    // 上方放得下就放上方（默认），放不下翻到下方。
    const below = anchor.top - PANEL_GAP - h < 8;
    const panelLeft = Math.min(
      Math.max(centerX - w / 2, 8),
      window.innerWidth - w - 8,
    );
    const panelTop = below ? anchor.top + anchor.height + PANEL_GAP : anchor.top - PANEL_GAP - h;

    // 小尖水平方向永远指着图标，但夹在面板范围内（面板被视口夹取时不能跟着出界）。
    const caretCenter = Math.min(
      Math.max(centerX, panelLeft + CARET_SIZE),
      panelLeft + w - CARET_SIZE,
    );

    panel = createPortal(
      <div
        ref={panelRef}
        role="tooltip"
        style={{ left: panelLeft, top: panelTop }}
        className={`pointer-events-none fixed z-50 w-max max-w-[320px] rounded-control border border-border bg-surface-raised px-3.5 py-3 text-xs text-ink shadow-raised ${panelClassName}`}
      >
        <span
          aria-hidden="true"
          style={{ left: caretCenter - panelLeft - CARET_SIZE / 2 }}
          className={`absolute h-2 w-2 rotate-45 border-border bg-surface-raised ${
            below ? "-top-1 border-l border-t" : "-bottom-1 border-b border-r"
          }`}
        />
        {content}
      </div>,
      document.body,
    );
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        aria-label={label}
        className="inline-flex cursor-help items-center justify-center rounded-full p-0.5 text-ink-muted"
        onMouseEnter={open}
        onMouseLeave={close}
        onFocus={open}
        onBlur={close}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            close();
          }
        }}
      >
        <InfoIcon className={iconClassName} />
      </button>
      {panel}
    </>
  );
}
