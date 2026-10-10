// 指标与图表类型只改渲染、不改查询范围——数据口径由筛选栏决定，卡片不许偷偷改它。
// 头部只留一个 ⚙ 入口：切换控件住进悬浮面板（portal 到 body、锚在按钮下方、可拖动）。
// 面板开着时当前卡保留描边高亮、⚙ 绿色常驻（见 dashboard.css 的 :has 规则）；
// 面板常驻 DOM、只切透明度/位移，收放都有过渡；标题栏即拖拽把手。

import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";

import { Segmented, type SegmentedOption } from "../../components/ui/Segmented";
import { Switch } from "../../components/ui/Switch";
import type { Strings } from "../../i18n/strings";
import type { CardConfig, CardPaletteScheme } from "../../lib/overview/layout";
import type { BreakdownDimension, ChartKind, Metric } from "../../lib/overview/types";

const ICON = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

// 画法选择用的四个彩色小图标：从"图表类型一览"的同款缩来，比线稿更像它们代表的图。
// 颜色走 --tt-chart-* 令牌（SVG 表现属性里不能用 var()，所以经 style 注入），跟随主题。

function BarIcon() {
  // 四根高低不一的彩柱，就是堆叠柱状档去掉堆叠关系后的样子。
  return (
    <svg width={26} height={18} viewBox="0 0 64 44" aria-hidden="true">
      <rect x="8" y="20" width="10" height="20" rx="2" style={{ fill: "var(--tt-chart-1)" }} />
      <rect x="22" y="10" width="10" height="30" rx="2" style={{ fill: "var(--tt-chart-2)" }} />
      <rect x="36" y="16" width="10" height="24" rx="2" style={{ fill: "var(--tt-chart-5)" }} />
      <rect x="50" y="6" width="10" height="34" rx="2" style={{ fill: "var(--tt-chart-4)" }} />
    </svg>
  );
}

function LineIcon() {
  // 折线 + 数据点：折线档的两个视觉签名。
  return (
    <svg width={26} height={18} viewBox="0 0 64 44" aria-hidden="true">
      <polyline
        points="6,34 22,18 38,26 58,10"
        style={{ fill: "none", stroke: "var(--tt-chart-1)" }}
        strokeWidth={5}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <circle cx="22" cy="18" r="4.5" style={{ fill: "var(--tt-chart-1)" }} />
      <circle cx="38" cy="26" r="4.5" style={{ fill: "var(--tt-chart-1)" }} />
      <circle cx="58" cy="10" r="4.5" style={{ fill: "var(--tt-chart-1)" }} />
    </svg>
  );
}

function ShareIcon() {
  // 百分比堆叠面积：一个填满的框按份额分两层——模型用量/缓存占比的占比档就是这种画法。
  // 构成卡的占比档是环形 pie，图标由卡片用 chartIcons 覆盖成 DonutIcon。
  return (
    <svg width={26} height={18} viewBox="0 0 64 44" aria-hidden="true">
      <path d="M6 8 H58 V22 C 34 26, 18 20, 6 24 Z" style={{ fill: "var(--tt-chart-2)" }} />
      <path d="M6 24 C 18 20, 34 26, 58 22 V38 H6 Z" style={{ fill: "var(--tt-chart-1)" }} />
    </svg>
  );
}

/** 环形三段弧：构成卡占比档的真实形态（radius 数组的环形 pie），供 chartIcons 覆盖用。 */
export function DonutIcon() {
  return (
    <svg width={26} height={18} viewBox="0 0 64 44" aria-hidden="true">
      <path d="M32 6 A16 16 0 0 1 45.86 30" style={{ fill: "none", stroke: "var(--tt-chart-1)" }} strokeWidth={9} />
      <path d="M45.86 30 A16 16 0 0 1 24 35.86" style={{ fill: "none", stroke: "var(--tt-chart-2)" }} strokeWidth={9} />
      <path d="M24 35.86 A16 16 0 0 1 32 6" style={{ fill: "none", stroke: "var(--tt-chart-5)" }} strokeWidth={9} />
    </svg>
  );
}

function TreemapIcon() {
  // 大小不一的相邻色块，面积即用量。
  return (
    <svg width={26} height={18} viewBox="0 0 64 44" aria-hidden="true">
      <g style={{ stroke: "var(--tt-surface)", strokeWidth: 2 }}>
        <rect x="6" y="4" width="28" height="22" style={{ fill: "var(--tt-chart-1)" }} />
        <rect x="6" y="30" width="28" height="12" style={{ fill: "var(--tt-chart-4)" }} />
        <rect x="38" y="4" width="20" height="16" style={{ fill: "var(--tt-chart-2)" }} />
        <rect x="38" y="24" width="20" height="18" style={{ fill: "var(--tt-chart-5)" }} />
      </g>
    </svg>
  );
}

function GearIcon() {
  // 齿轮造型（Feather settings）：上一版是"圆心 + 放射短线"，14px 下读起来像亮度/太阳图标。
  return (
    <svg {...ICON} width={14} height={14} aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

function GripIcon() {
  // 六点握把：提示"标题栏可以按住拖"。
  return (
    <svg width={10} height={14} viewBox="0 0 10 14" aria-hidden="true" fill="currentColor" className="shrink-0 text-ink-muted">
      <circle cx="3" cy="3" r="1.3" />
      <circle cx="7" cy="3" r="1.3" />
      <circle cx="3" cy="7" r="1.3" />
      <circle cx="7" cy="7" r="1.3" />
      <circle cx="3" cy="11" r="1.3" />
      <circle cx="7" cy="11" r="1.3" />
    </svg>
  );
}

const METRICS: readonly Metric[] = ["cost", "tokens", "calls"];

const METRIC_KEYS: Record<Metric, keyof Strings> = {
  cost: "metricCost",
  tokens: "metricTokens",
  calls: "metricCalls",
};

const CHART_LABEL_KEYS: Record<ChartKind, keyof Strings> = {
  bar: "chartBar",
  line: "chartLine",
  share: "chartShare",
  treemap: "chartTreemap",
  calendar: "chartCalendar",
};

const CHART_ICONS: Record<ChartKind, () => React.JSX.Element> = {
  bar: BarIcon,
  line: LineIcon,
  share: ShareIcon,
  treemap: TreemapIcon,
  calendar: CalendarIcon,
};

/** 日历卡的图标：坐标日历——等宽方格铺满一季。 */
function CalendarIcon() {
  return (
    <svg width={26} height={18} viewBox="0 0 64 44" aria-hidden="true">
      {Array.from({ length: 7 }, (_, row) =>
        Array.from({ length: 14 }, (_, col) => (
          <rect
            key={`${row}-${col}`}
            x={4 + col * 4}
            y={4 + row * 5.2}
            width={3}
            height={4}
            rx={0.5}
            style={{ fill: `var(--tt-chart-${(row + col) % 5 === 0 ? "4" : "2"})`, opacity: (row + col) % 3 === 0 ? 1 : 0.5 }}
          />
        )),
      )}
    </svg>
  );
}

const PALETTE_LABEL_KEYS: Record<CardPaletteScheme, keyof Strings> = {
  default: "paletteDefault",
  indigo: "paletteIndigo",
  warm: "paletteWarm",
  gray: "paletteGray",
};

/** 滑出动画的时长：退出比进入快，关闭要干脆。 */
const EXIT_MS = 200;

/** 面板宽度，与 JSX 里的 w-80 保持一致：打开时按它把面板夹进视口。 */
const PANEL_WIDTH = 320;

/** 一行设置：左标签右控件；面板窄时允许控件换行到标签下面。my-1 给行与行留出呼吸感。 */
function SettingRow({ label, children }: { readonly label: string; readonly children: ReactNode }) {
  return (
    <div className="my-1 flex min-h-8 flex-wrap items-center justify-between gap-x-2 gap-y-1">
      <span className="text-xs text-ink-muted">{label}</span>
      {children}
    </div>
  );
}

/** 画法选择：图标 + 文字的卡片式按钮，一行排开。不用 Segmented：纯图标在面板里可读性差。 */
function ChartPicker({
  label,
  options,
  value,
  onChange,
}: {
  readonly label: string;
  readonly options: readonly { readonly value: ChartKind; readonly icon: () => React.JSX.Element; readonly text: string }[];
  readonly value: ChartKind;
  readonly onChange: (value: ChartKind) => void;
}) {
  const name = useId();

  return (
    <fieldset className="flex gap-1.5">
      <legend className="sr-only">{label}</legend>
      {options.map((option) => {
        const Icon = option.icon;
        const selected = option.value === value;
        // 选中态用灰调而不是强调色：彩色图标本身已是视觉主角，绿底会盖过它们。

        return (
          <label key={option.value} className="min-w-0 flex-1 cursor-pointer">
            <input
              type="radio"
              name={name}
              value={option.value}
              checked={selected}
              onChange={() => onChange(option.value)}
              className="peer sr-only"
            />
            <span
              className={`flex flex-col items-center gap-1 rounded-control border px-1 py-1.5 peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-ring ${
                selected ? "border-ink-muted bg-surface-muted text-ink" : "border-border text-ink-muted hover:text-ink"
              }`}
            >
              <Icon />
              <span className="text-[11px] leading-none">{option.text}</span>
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}

interface CardActionsProps {
  readonly config: CardConfig;
  readonly strings: Strings;
  readonly charts: readonly ChartKind[];
  readonly onConfigChange: (config: CardConfig) => void;
  readonly showMetric?: boolean;
  /** 可选的指标子集；缺省给全量。缓存卡片等口径有限的场景用它收窄。 */
  readonly metrics?: readonly Metric[];
  /** 构成卡的拆分维度切换；缺省不出现维度行。 */
  readonly dimensions?:
    | readonly {
        readonly value: BreakdownDimension;
        readonly label: string;
      }[]
    | undefined;
  /** 合并卡的视图切换；缺省不出现视图行。取值集与接线由调用方定义
   * （用量卡三档、热力图卡两档），这里只管受控渲染。 */
  readonly views?: readonly { readonly value: string; readonly label: string }[] | undefined;
  readonly viewValue?: string | undefined;
  readonly onViewChange?: ((value: string) => void) | undefined;
  /** "外观"组里各开关的适用性按图表类型声明：占比档没有零点、树图没有网格线，
   * 抽屉里的开关要跟着当前画法增减。查不到当前类型时回落到 default 档。 */
  readonly appearance?:
    | Partial<
        Record<
          ChartKind | "default",
          {
            readonly legend?: boolean;
            readonly gridLines?: boolean;
            readonly yAxisZero?: boolean;
          }
        >
      >
    | undefined;
  /** 同一档位在不同卡里的真实形态可能不同（share 在构成卡是环形 pie、在别处是百分比
   * 面积）：按类型覆盖画法图标，缺省用上面那套通用的。 */
  readonly chartIcons?: Partial<Record<ChartKind, () => React.JSX.Element>> | undefined;
  /** 卡片专属设置行（时间窗口、颜色分档这类只有个别卡有的口径）：接好受控状态的
   * 现成控件，渲染在"数据口径"组末尾。 */
  readonly extraRows?:
    | readonly { readonly label: string; readonly node: React.JSX.Element }[]
    | undefined;
}

export function CardActions({
  config,
  strings,
  charts,
  onConfigChange,
  showMetric = true,
  metrics = METRICS,
  dimensions,
  views,
  viewValue,
  onViewChange,
  appearance,
  chartIcons,
  extraRows,
}: CardActionsProps) {
  // open 管状态驱动过渡类；visible 管内容挂载——外壳常驻做淡出动画，
  // 内容等退出动画放完再卸载，避免过渡途中变空壳。
  const [open, setOpen] = useState(false);
  const [visible, setVisible] = useState(false);
  const [cardTitle, setCardTitle] = useState("");
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const drawerRef = useRef<HTMLDivElement>(null);
  const timerRef = useRef<number | undefined>(undefined);
  const dragRef = useRef<{ pointerX: number; pointerY: number; posX: number; posY: number } | undefined>(
    undefined,
  );

  const openDrawer = useCallback(() => {
    window.clearTimeout(timerRef.current);
    // 面板在 body 下，不知道自己服务哪张卡；卡片标题就近从 DOM 里取，免得改 8 个调用点。
    const title = triggerRef.current?.closest(".tt-card")?.querySelector("h2")?.textContent ?? "";
    setCardTitle(title);
    // 每次打开都重新锚到 ⚙ 下方（右对齐）、夹在视口内；之后用户可以拖走。
    const rect = triggerRef.current?.getBoundingClientRect();
    if (rect === undefined) {
      setPos({ x: Math.max(window.innerWidth - PANEL_WIDTH - 12, 8), y: 72 });
    } else {
      const x = Math.min(Math.max(rect.right - PANEL_WIDTH, 8), window.innerWidth - PANEL_WIDTH - 8);
      const y = Math.min(Math.max(rect.bottom + 8, 8), window.innerHeight - 120);
      setPos({ x, y });
    }
    setVisible(true);
    setOpen(true);
  }, []);

  const close = useCallback(() => {
    setOpen(false);
    timerRef.current = window.setTimeout(() => setVisible(false), EXIT_MS);
  }, []);

  const toggle = useCallback(() => {
    if (open) {
      close();
    } else {
      openDrawer();
    }
  }, [open, close, openDrawer]);

  useEffect(() => () => window.clearTimeout(timerRef.current), []);

  // 拖动面板：标题栏即把手。pointer capture 让指针滑出面板也持续跟随；
  // 起手记下指针与面板左上角的相对关系，移动时只做平移并夹在视口内。
  // ✕ 按钮在把手区内，起手前要排除，否则按下关闭键会先触发一次拖动。
  const startDrag = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || (event.target as HTMLElement).closest("button")) {
      return;
    }
    const panel = drawerRef.current;
    if (panel === null) {
      return;
    }
    event.preventDefault();
    const rect = panel.getBoundingClientRect();
    dragRef.current = {
      pointerX: event.clientX,
      pointerY: event.clientY,
      posX: rect.left,
      posY: rect.top,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  }, []);

  const moveDrag = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    const panel = drawerRef.current;
    if (drag === undefined || panel === null) {
      return;
    }
    const rect = panel.getBoundingClientRect();
    const x = Math.min(
      Math.max(drag.posX + event.clientX - drag.pointerX, 8),
      window.innerWidth - rect.width - 8,
    );
    const y = Math.min(
      Math.max(drag.posY + event.clientY - drag.pointerY, 8),
      window.innerHeight - rect.height - 8,
    );
    setPos({ x, y });
  }, []);

  const endDrag = useCallback((event: React.PointerEvent<HTMLDivElement>) => {
    if (dragRef.current === undefined) {
      return;
    }
    dragRef.current = undefined;
    event.currentTarget.releasePointerCapture(event.pointerId);
  }, []);

  // Esc 关闭。
  useEffect(() => {
    if (!open) {
      return undefined;
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (event.key === "Escape") {
        close();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, close]);

  // 点面板外关闭。不用盖一层透明命中层：页面滚在内层 <main>（App.tsx）而不是 body，
  // 盖层不是它的后代，滚轮链到不了滚动容器，左侧就没法滚了。改为监听 pointerdown，
  // 落点不在面板内（也不在触发按钮上——那颗由 toggle 自己处理）就收起。
  useEffect(() => {
    if (!open) {
      return undefined;
    }
    const onPointerDown = (event: PointerEvent): void => {
      const target = event.target as Node;
      if (!drawerRef.current?.contains(target) && !triggerRef.current?.contains(target)) {
        close();
      }
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open, close]);

  // 打开后把焦点搬进面板：键盘用户不用 Tab 穿过整个页面。
  useEffect(() => {
    if (open) {
      drawerRef.current?.focus();
    }
  }, [open]);

  const metricOptions: SegmentedOption<Metric>[] = metrics.map((metric) => ({
    value: metric,
    label: strings[METRIC_KEYS[metric]],
  }));
  const paletteOptions: SegmentedOption<CardPaletteScheme>[] = (
    ["default", "indigo", "warm", "gray"] as const
  ).map((scheme) => ({
    value: scheme,
    label: strings[PALETTE_LABEL_KEYS[scheme]],
  }));
  const chartOptions = charts.map((kind) => ({
    value: kind,
    icon: chartIcons?.[kind] ?? CHART_ICONS[kind],
    text: strings[CHART_LABEL_KEYS[kind]],
  }));
  const hasDataGroup =
    dimensions !== undefined || views !== undefined || showMetric || (extraRows?.length ?? 0) > 0;
  const hasChartGroup = charts.length > 0;
  // 外观开关按当前画法取：画法切换后抽屉里的开关行会跟着增减。
  const kindAppearance = appearance?.[config.chart] ?? appearance?.["default"] ?? {};

  // SSR（node 测试环境）没有 body：抽屉是打开后的浮层，首帧不存在也无需注水。
  const drawer =
    typeof document === "undefined" ? null : createPortal(
    <div
      ref={drawerRef}
      role="dialog"
      aria-label={strings.cardSettings}
      aria-hidden={!open}
      tabIndex={-1}
      style={pos === null ? undefined : { left: pos.x, top: pos.y }}
      className={`fixed z-50 flex max-h-[min(70vh,600px)] w-80 flex-col rounded-card border border-border bg-surface-raised shadow-overlay outline-none transition-[opacity,transform] motion-reduce:transition-none ${
        open
          ? "pointer-events-auto translate-y-0 scale-100 opacity-100 duration-200 ease-out"
          : "pointer-events-none -translate-y-1 scale-[0.98] opacity-0 duration-150 ease-in"
      }`}
    >
      {visible ? (
        <>
          <div
            onPointerDown={startDrag}
            onPointerMove={moveDrag}
            onPointerUp={endDrag}
            onPointerCancel={endDrag}
            className="flex cursor-grab touch-none select-none items-center gap-2 rounded-t-lg border-b border-border bg-surface-subtle px-3 py-2.5 active:cursor-grabbing"
          >
            <GripIcon />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium leading-5">{strings.cardSettings}</p>
              {cardTitle === "" ? null : (
                <p className="truncate text-xs text-ink-muted">{cardTitle}</p>
              )}
            </div>
            <button
              type="button"
              aria-label={strings.configClose}
              onClick={close}
              className="inline-flex size-6 shrink-0 cursor-pointer items-center justify-center rounded-control text-ink-muted transition-colors hover:text-ink"
            >
              <svg {...ICON} width={14} height={14} aria-hidden="true">
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          </div>
            {/* min-h-0 必须有：flex 子项默认 min-height:auto，没有它 max-h 压不出滚动条。 */}
            <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
              {hasDataGroup ? (
                <>
                  <p className="text-[11px] leading-4 text-ink-muted">{strings.settingsGroupData}</p>
                  {views === undefined || viewValue === undefined || onViewChange === undefined ? null : (
                    <SettingRow label={strings.cardViewLabel}>
                      <Segmented
                        label={strings.cardViewLabel}
                        size="sm"
                        options={views}
                        value={viewValue}
                        onChange={onViewChange}
                      />
                    </SettingRow>
                  )}
                  {dimensions === undefined ? null : (
                    <SettingRow label={strings.cardDimensionLabel}>
                      <Segmented
                        label={strings.cardDimensionLabel}
                        size="sm"
                        options={dimensions.map(({ value, label: dimensionLabel }) => ({ value, label: dimensionLabel }))}
                        value={config.dimension ?? "model"}
                        onChange={(dimension) => onConfigChange({ ...config, dimension })}
                      />
                    </SettingRow>
                  )}
                  {showMetric ? (
                    <SettingRow label={strings.cardMetricLabel}>
                      <Segmented
                        label={strings.cardMetricLabel}
                        size="sm"
                        options={metricOptions}
                        value={config.metric}
                        onChange={(metric) => onConfigChange({ ...config, metric })}
                      />
                    </SettingRow>
                  ) : null}
                  {(extraRows ?? []).map((row) => (
                    <SettingRow key={row.label} label={row.label}>
                      {row.node}
                    </SettingRow>
                  ))}
                </>
              ) : null}
        {hasDataGroup && hasChartGroup ? <div className="my-2 border-t border-border" /> : null}
        {hasChartGroup ? (
          <>
            <p className="text-[11px] leading-4 text-ink-muted">{strings.settingsGroupChart}</p>
            <div className="mt-1.5">
              <ChartPicker
                label={strings.cardChartLabel}
                options={chartOptions}
                value={config.chart}
                onChange={(chart) => onConfigChange({ ...config, chart })}
              />
            </div>
          </>
        ) : null}
        {hasDataGroup || hasChartGroup ? <div className="my-2 border-t border-border" /> : null}
        <p className="text-[11px] leading-4 text-ink-muted">{strings.settingsGroupAppearance}</p>
        <SettingRow label={strings.paletteSchemeLabel}>
          <Segmented
            label={strings.paletteSchemeLabel}
            size="sm"
            options={paletteOptions}
            value={config.palette ?? "default"}
            onChange={(palette) => onConfigChange({ ...config, palette })}
          />
        </SettingRow>
        {kindAppearance.legend ? (
          <SettingRow label={strings.settingShowLegend}>
            {/* SettingRow 的可见标签在左，开关文字对读屏保留即可。 */}
            <Switch
              checked={config.showLegend ?? true}
              label={strings.settingShowLegend}
              labelHidden
              onChange={(showLegend) => onConfigChange({ ...config, showLegend })}
            />
          </SettingRow>
        ) : null}
        {kindAppearance.gridLines ? (
          <SettingRow label={strings.settingShowGridLines}>
            <Switch
              checked={config.showGridLines ?? true}
              label={strings.settingShowGridLines}
              labelHidden
              onChange={(showGridLines) => onConfigChange({ ...config, showGridLines })}
            />
          </SettingRow>
        ) : null}
        {kindAppearance.yAxisZero ? (
          <SettingRow label={strings.settingYAxisFromZero}>
            <Switch
              checked={config.yAxisFromZero ?? false}
              label={strings.settingYAxisFromZero}
              labelHidden
              onChange={(yAxisFromZero) => onConfigChange({ ...config, yAxisFromZero })}
            />
          </SettingRow>
        ) : null}
            </div>
          </>
        ) : null}
    </div>,
    document.body,
  );

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        data-open={open ? "" : undefined}
        aria-label={strings.cardSettings}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={toggle}
        className={`tt-settings-trigger inline-flex size-6 cursor-pointer items-center justify-center rounded-control border transition-colors ${
          open
            ? "border-accent bg-accent text-accent-ink"
            : "border-border bg-surface text-ink-muted hover:text-ink"
        }`}
      >
        <GearIcon />
      </button>
      {drawer}
    </>
  );
}
