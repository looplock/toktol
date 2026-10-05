/**
 * 会话详情的刻度式目录：一根刻度 = 一轮，1:1 渲染全部轮次，轨道是滚动
 * 视口——会话很长时刻度串超出视口，滚动条（或滚轮）上下查看，两端渐隐
 * 遮罩示意「还有」；active 刻度变化时自动滚入视野，与转录滚动联动无缝。
 * 与列表目录共用 OutlineListHandle。刻度长度与高亮走命令式 DOM 更新（同
 * OutlineList：滚动联动零 React 重渲染）；悬停以光标刻度为中心邻近衰减
 * 加长——宽度动画用 scaleX（transform 合成器路径，零重排），摘要卡为
 * React 态（仅跨档时重渲染），位置命令式跟随（滚动时不重渲染）。
 */

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type MouseEvent,
} from "react";
import type { TranscriptTurn } from "../../lib/api";
import type { Strings } from "../../i18n/strings";
import type { OutlineListHandle } from "./OutlineList";
import { ImageIcon } from "../../components/ui/icons";

/** 悬停衰减分档 d0..d3 的刻度视觉宽度（px）；d≥4 与静止基准同为 BASE_WIDTH。
 * 超出布局盒（BOX_WIDTH）的档位靠 scaleX > 1 向右伸展，右缘超出部分由轨道
 * 裁齐——中心档可见长度封顶在「栏宽 − 锚点」，故衰减曲线要陡才能拉开差距。 */
const TIER_WIDTH = [30, 19, 14, 11] as const;
const BASE_COLOR = "bg-ink-muted/40";
/** 刻度布局盒宽度（居中放置）；视觉宽度由 scaleX 缩放得出。 */
const BOX_WIDTH = 26;
const BASE_WIDTH = 10;
/** 衰减影响半径（光标上下各 3 根）。 */
const FALLOFF = 3;
/** 相邻刻度的固定槽距（px）：紧凑刻度尺的核心节奏。 */
const SLOT = 10;
/** 可见刻度串的根数上限：滚动视口 maxHeight = MAX_TICKS × SLOT，
 * 栏再高刻度尺也不会铺满整栏；其余刻度滚查看。 */
const MAX_TICKS = 40;
/** 轨道上/下内边距（对应 py-2）：悬停命中要扣掉它。 */
const PAD_Y = 8;
/**
 * 刻度串的右移量（px）：布局盒居中、scaleX 用左缘原点，静止基准刻度的
 * 中心恰在栏中心（视觉居中）；悬停加长时左缘锚定不动，只向右伸展。
 */
const GROW_SHIFT = (BOX_WIDTH - BASE_WIDTH) / 2;

export const TurnRuler = forwardRef<
  OutlineListHandle,
  {
    readonly turns: readonly TranscriptTurn[];
    readonly strings: Strings;
    /** 初值轮次：重挂载（换会话）时由父级给最后已知轮次。 */
    readonly initialTurn: number;
    readonly onJump: (index: number) => void;
  }
>(function TurnRuler({ turns, strings, initialTurn, onJump }, ref) {
  const asideRef = useRef<HTMLElement | null>(null);
  const scrollerRef = useRef<HTMLDivElement>(null);
  const stackRef = useRef<HTMLDivElement>(null);
  const tooltipRef = useRef<HTMLDivElement | null>(null);
  const tickRefs = useRef<(HTMLSpanElement | null)[]>([]);
  const activeTurnRef = useRef(-1);
  const hoverRef = useRef(-1);
  const [hoverIdx, setHoverIdx] = useState<number | null>(null);
  // 滚动视口高度：驱动「是否可滚」（遮罩显隐）。SSR / 首帧未知时为 0。
  const [availH, setAvailH] = useState(0);
  // 滚动边界：顶/底遮罩只在「这一端还有更多」时显示，滚到位即消失。
  const [atTop, setAtTop] = useState(true);
  const [atBottom, setAtBottom] = useState(false);

  // 依滚动位置刷新边界态（setState 同值时 React 自动跳过，滚动帧无谓重渲染）。
  const syncScrollBounds = useCallback(() => {
    const el = scrollerRef.current;
    if (el === null) return;
    setAtTop(el.scrollTop <= 0);
    setAtBottom(el.scrollTop >= el.scrollHeight - el.clientHeight - 1);
  }, []);

  // 测量滚动视口高度；ResizeObserver 缺失（测试环境）只测一次。
  useLayoutEffect(() => {
    const el = scrollerRef.current;
    if (el === null) return undefined;
    const measure = () => {
      setAvailH(el.clientHeight);
      syncScrollBounds();
    };
    measure();
    if (typeof ResizeObserver === "undefined") return undefined;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [syncScrollBounds]);

  // 刻度串超出视口才可滚：两端遮罩只在这种时候有意义。
  const scrollable = turns.length * SLOT + 2 * PAD_Y > availH;

  // 单根刻度重绘：长度（scaleX）与颜色由 active / 悬停档位推导。长度只响应
  // 悬停（active 不加长，点击不改变几何）；颜色两态——active 用 accent 常驻
  // 高亮，悬浮中心那根加深，其余（含衰减邻档）一律基准色。className 只在
  // 变色时改写（dataset 去重，防无谓的 DOM class 写入）。
  const paint = useCallback((i: number) => {
    const el = tickRefs.current[i];
    if (el === null || el === undefined) return;
    const d = hoverRef.current < 0 ? -1 : Math.abs(i - hoverRef.current);
    const hovered = d >= 0 && d <= FALLOFF;
    const isActive = i === activeTurnRef.current;
    const width = hovered ? (TIER_WIDTH[d] ?? BASE_WIDTH) : BASE_WIDTH;
    el.style.transform = `translateX(${GROW_SHIFT}px) scaleX(${width / BOX_WIDTH})`;
    const color = isActive ? "bg-accent" : d === 0 ? "bg-ink-strong" : BASE_COLOR;
    if (el.dataset.c !== color) {
      el.dataset.c = color;
      el.className = `block h-[2px] rounded-full ${color}`;
    }
  }, []);

  // 重绘 [from,to] 各自 ±FALLOFF 的覆盖带：悬停档位移动时新旧位置一起刷，
  // 移出端的衰减尾与移入端对称恢复。
  const paintRange = useCallback(
    (from: number, to: number) => {
      const lo = Math.max(0, Math.min(from, to) - FALLOFF);
      const hi = Math.min(turns.length - 1, Math.max(from, to) + FALLOFF);
      for (let i = lo; i <= hi; i += 1) paint(i);
    },
    [paint, turns.length],
  );

  // 摘要卡命令式定位：对齐当前悬停刻度的视口位置（随轨道滚动实时跟随）。
  const positionTooltip = useCallback(() => {
    const tip = tooltipRef.current;
    const tick = hoverRef.current >= 0 ? tickRefs.current[hoverRef.current] : null;
    if (tip === null || tick === null || tick === undefined || asideRef.current === null)
      return;
    const top =
      tick.getBoundingClientRect().top - asideRef.current.getBoundingClientRect().top + SLOT / 2;
    tip.style.top = `${top}px`;
  }, []);

  // 只在纵向上把目标刻度滚进视口（对齐 scrollIntoView 的 nearest 语义）。
  // 不能用 scrollIntoView：它会连横向溢出一起「滚进视野」——悬停加长的刻度
  // 超出布局盒，形成横向可滚动区，点击后 scrollLeft 被推动，整列刻度左移。
  const scrollTickIntoView = useCallback((turn: number) => {
    const sc = scrollerRef.current;
    const el = tickRefs.current[turn];
    if (sc === null || el === null || el === undefined) return;
    const scRect = sc.getBoundingClientRect();
    const elRect = el.getBoundingClientRect();
    if (elRect.top < scRect.top) {
      sc.scrollTop += elRect.top - scRect.top;
    } else if (elRect.bottom > scRect.bottom) {
      sc.scrollTop += elRect.bottom - scRect.bottom;
    }
  }, []);

  const setActive = useCallback(
    (turn: number) => {
      if (turn === activeTurnRef.current) return;
      const prev = activeTurnRef.current;
      activeTurnRef.current = turn;
      if (prev >= 0) paint(prev);
      paint(turn);
      // active 换到视口外时把刻度滚进来（nearest：本就可见则不动）。
      scrollTickIntoView(turn);
    },
    [paint, scrollTickIntoView],
  );

  useImperativeHandle(ref, () => ({ setActive }), [setActive]);

  // 挂载时应用初值（换会话重挂，由父级给最后已知轮次）。
  useLayoutEffect(() => {
    setActive(initialTurn);
  }, [setActive, initialTurn]);

  // 轮次增减 / 视口尺寸变化后：刻度元素按 key 复用或重建，重刷全部刻度，
  // 避免新建元素停在默认样式。
  useEffect(() => {
    for (let i = 0; i < turns.length; i += 1) paint(i);
  }, [turns, availH, paint]);

  const onMove = (event: MouseEvent<HTMLDivElement>) => {
    // 以刻度串（滚动后的实际位置）为命中基准，而非整个轨道；
    // 光标在串上/下的空白里不算悬停任何刻度（含 clamp 后误中首尾的情形）。
    const el = stackRef.current;
    if (el === null || turns.length === 0) return;
    const rect = el.getBoundingClientRect();
    if (event.clientY < rect.top || event.clientY > rect.bottom) {
      if (hoverRef.current >= 0) onLeave();
      return;
    }
    const idx = Math.min(
      turns.length - 1,
      Math.max(0, Math.floor((event.clientY - rect.top) / SLOT)),
    );
    if (idx === hoverRef.current) return;
    const prev = hoverRef.current;
    hoverRef.current = idx;
    paintRange(prev < 0 ? idx : prev, idx);
    setHoverIdx(idx);
  };

  const onLeave = () => {
    const prev = hoverRef.current;
    hoverRef.current = -1;
    if (prev >= 0) paintRange(prev, prev);
    setHoverIdx(null);
  };

  // 摘要卡挂载 / 跨档后对位（初始 top 是占位值，必须在绘制前校正）。
  useLayoutEffect(() => {
    if (hoverIdx !== null) positionTooltip();
  }, [hoverIdx, positionTooltip]);

  const hoveredTurn = hoverIdx === null ? undefined : turns[hoverIdx];

  return (
    <aside
      ref={asideRef}
      aria-label={strings.sessionsViewRuler}
      className="relative w-10 shrink-0"
    >
      {/* 外层只做留白与居中；滚动视口由内层承担，maxHeight 封顶可见刻度串
          长度（≈ MAX_TICKS 根）——栏再高刻度尺也不会铺满整栏。 */}
      <div
        className="flex h-full flex-col py-2"
        onMouseMove={onMove}
        onMouseLeave={onLeave}
      >
        {/* my-auto：视口（含封顶）不足一栏时纵向居中；wrapper 撑到允许的
            最大高度后渐隐遮罩正好钉在它的上下边缘。 */}
        <div className="relative my-auto min-h-0 w-full">
          <div
            ref={scrollerRef}
            className="min-h-0 overflow-y-auto overflow-x-clip py-2 [&::-webkit-scrollbar]:[display:none] [scrollbar-width:none]"
            style={{ maxHeight: MAX_TICKS * SLOT }}
            onScroll={() => {
              syncScrollBounds();
              positionTooltip();
            }}
          >
            <div ref={stackRef}>
              {/* button 的 width:auto 是 fit-content（不像 div 撑满父级），
                  必须 w-full 才能让 justify-center 真正居中刻度。 */}
              {turns.map((turn, i) => (
                <button
                  key={turn.seq}
                  type="button"
                  aria-label={strings.sessionsRulerTurn.replace("{n}", String(i + 1))}
                  aria-current={i === activeTurnRef.current ? "true" : undefined}
                  onClick={() => onJump(i)}
                  className="flex w-full cursor-pointer items-center justify-center"
                  style={{ height: SLOT }}
                >
                  <span
                    ref={(el) => {
                      tickRefs.current[i] = el;
                    }}
                    className={`block h-[2px] rounded-full ${BASE_COLOR}`}
                    style={{
                      width: BOX_WIDTH,
                      transform: `translateX(${GROW_SHIFT}px) scaleX(${BASE_WIDTH / BOX_WIDTH})`,
                      // 左缘原点 + 右移量：静止居中、悬停只向右伸展（见 GROW_SHIFT）。
                      transformOrigin: "0 50%",
                      transition: "transform 150ms",
                    }}
                  />
                </button>
              ))}
            </div>
          </div>
          {/* 可滚时两端渐隐遮罩（钉在视口边缘，不随内容滚）：滚到顶/底时
              对应端遮罩消失，表示「这一端到底了」。pointer-events-none
              不挡悬停与滚动条。 */}
          {scrollable && !atTop && (
            <div
              aria-hidden
              className="pointer-events-none absolute inset-x-0 top-0 h-8 bg-gradient-to-b from-surface-raised via-surface-raised/70 to-transparent"
            />
          )}
          {scrollable && !atBottom && (
            <div
              aria-hidden
              className="pointer-events-none absolute inset-x-0 bottom-0 h-8 bg-gradient-to-t from-surface-raised via-surface-raised/70 to-transparent"
            />
          )}
        </div>
      </div>
      {hoveredTurn !== undefined && hoverIdx !== null && (
        <div
          ref={tooltipRef}
          className="absolute left-full z-10 ml-2 w-56 -translate-y-1/2 rounded-control border border-border bg-surface-raised px-3 py-2 shadow-overlay"
          style={{ top: -9999 }}
        >
          <p className="text-xs text-ink-muted">
            {strings.sessionsRulerTurn.replace("{n}", String(hoverIdx + 1))}
            {hoveredTurn.tsMs !== null &&
              ` · ${new Date(hoveredTurn.tsMs).toLocaleTimeString([], { hour12: false })}`}
          </p>
          {hoveredTurn.isSummary ? (
            <p className="mt-1 text-xs text-ink">{strings.transcriptCompacted}</p>
          ) : hoveredTurn.parts.length > 0 ? (
            /* 文本与图片胶囊按原位交错（parts 保序）：一段 flex-wrap 文本流。 */
            <p className="mt-1 flex flex-wrap items-center gap-x-1 gap-y-0.5 text-xs text-ink">
              {hoveredTurn.parts.map((part, i) =>
                part.kind === "text" ? (
                  <span key={i}>{part.text}</span>
                ) : (
                  <span
                    key={i}
                    className="inline-flex max-w-full items-center gap-1 rounded-full border border-border bg-surface-subtle px-1.5 py-px text-ink-muted"
                  >
                    <ImageIcon className="size-3 shrink-0" />
                    <span className="min-w-0 truncate">
                      {part.filename !== "" ? part.filename : strings.transcriptImage}
                    </span>
                  </span>
                ),
              )}
            </p>
          ) : (
            <p className="mt-1 text-xs text-ink">
              {hoveredTurn.snippet !== "" ? hoveredTurn.snippet : strings.transcriptRoleUser}
            </p>
          )}
        </div>
      )}
    </aside>
  );
});
