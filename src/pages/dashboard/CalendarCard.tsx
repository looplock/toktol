// 活跃日历：GitHub 风格的年度热力图。列 = 周、行 = 星期，一格一天，颜色深浅 =
// 当天用量。时间窗口是卡片自己的设置，不跟随顶部筛选；数据固定取 data.daily 的
// 尾部 N 天，缺日按 0 处理（没动的日子最浅）。
// 格子必须是正方形——尺寸从容器宽/高预算里取小者，日历居中，多余空间留白
//（宽高比随用户拖拽变化，不可能同时填满两个方向）。

import { useEffect, useMemo, useRef, useState } from "react";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import { Segmented } from "../../components/ui/Segmented";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCount, formatCostMicros, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import {
  CALENDAR_STEPS_CONTINUOUS,
  CALENDAR_STEPS_DEFAULT,
  CALENDAR_WINDOW_DEFAULT,
  type CardConfig,
} from "../../lib/overview/layout";
import type { DashboardData, DailyPoint } from "../../lib/overview/types";
import { CardActions } from "./CardActions";
import { EmptyState } from "./EmptyState";
import type { CardGeometry } from "./geometry";
import { barFocus } from "./optionBase";

interface CalendarCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
}

interface CalendarTip {
  readonly seriesIndex?: number;
  readonly value?: readonly [string, number];
}

/** 颜色按 t 插值：给分档图例与渐变色带生成中间色（echarts 不认 CSS 变量）。
 * 色带浅端是首次混合的产物（rgb(...) 格式），所以这里两种格式都得吃。 */
function mixHex(a: string, b: string, t: number): string {
  const parse = (color: string): [number, number, number] => {
    if (color.startsWith("#")) {
      return [
        parseInt(color.slice(1, 3), 16),
        parseInt(color.slice(3, 5), 16),
        parseInt(color.slice(5, 7), 16),
      ];
    }

    const rgb = color.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/);
    return rgb ? [Number(rgb[1]), Number(rgb[2]), Number(rgb[3])] : [0, 0, 0];
  };
  const [r1, g1, b1] = parse(a);
  const [r2, g2, b2] = parse(b);
  const mix = (x: number, y: number): number => Math.round(x + (y - x) * t);

  return `rgb(${mix(r1, r2)},${mix(g1, g2)},${mix(b1, b2)})`;
}

/** 容器尺寸跟踪：格子尺寸的唯一事实来源，容器变了就得重算 option。 */
function useContainerSize(ref: React.RefObject<HTMLDivElement | null>): { w: number; h: number } {
  const [size, setSize] = useState({ w: 0, h: 0 });

  useEffect(() => {
    const el = ref.current;
    if (el === null) return;

    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect !== undefined) {
        setSize({ w: Math.round(rect.width), h: Math.round(rect.height) });
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);

  return size;
}

/** Date → 本地 'YYYY-MM-DD'。不用 toISOString：它走 UTC，GMT+8 的本地午夜会偏到前一天。 */
function formatDate(d: Date): string {
  const month = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");

  return `${d.getFullYear()}-${month}-${day}`;
}

/** 本地日期字符串 + 天数偏移 → 本地日期字符串（ISO 日期字典序 = 时间序）。 */
function shiftDate(date: string, days: number): string {
  const d = new Date(`${date}T00:00:00`);
  d.setDate(d.getDate() + days);

  return formatDate(d);
}

export function CalendarCard({ geo, strings, data, config, onConfigChange }: CalendarCardProps) {
  const palette = useChartPalette(config.palette);
  const numberUnit = useNumberUnit();
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const box = useContainerSize(bodyRef);

  const windowDays = config.calendarWindow ?? CALENDAR_WINDOW_DEFAULT;
  const steps = config.calendarSteps ?? CALENDAR_STEPS_DEFAULT;
  const cost = config.metric === "cost";
  const amountOf = (point: DailyPoint): number =>
    cost
      ? point.costMicros / 1_000_000
      : config.metric === "tokens"
        ? point.tokens
        : point.calls;
  const format = (amount: number): string =>
    cost ? formatCostMicros(amount * 1_000_000) : formatTokens(amount, numberUnit);

  // 时间窗口按自然日切（卡片设置独立于顶部筛选）：终点取最后一条有数据的
  // 日子与今天 的较大者，起点往前推 windowDays 天。daily 只含有数据的天，
  // 按记录条数 slice 会把"一年"缩成"最早数据以来的天数"——缺数据的日子
  // 补 0，数据少也画出完整窗口的网格框架。
  const pointByDate = useMemo(
    () => new Map(data.daily.map((point) => [point.date, point])),
    [data],
  );
  const windowEnd = useMemo(() => {
    const today = formatDate(new Date());
    const last = data.daily[data.daily.length - 1]?.date ?? today;

    return last > today ? last : today;
  }, [data]);
  const windowStart = useMemo(
    () => shiftDate(windowEnd, -(windowDays - 1)),
    [windowEnd, windowDays],
  );
  // 全量日子一个序列：无记录日标记为 -1，落在色带 min 之外，由 visualMap 的
  // outOfRange 统一着灰——"没记录"不是"用得少"，且两种格子走同一条绘制路径，
  // 尺寸天然一致（拆双序列会因渲染管线不同出现大小不一）。
  const points = useMemo<[string, number][]>(() => {
    const result: [string, number][] = [];
    let cursor = windowStart;
    while (cursor <= windowEnd) {
      const point = pointByDate.get(cursor);
      result.push([cursor, point === undefined ? -1 : amountOf(point)]);
      cursor = shiftDate(cursor, 1);
    }

    return result;
    // amountOf 是 config.metric 的纯函数，依赖里以 config.metric 表达。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pointByDate, windowStart, windowEnd, config.metric]);
  // 至少给 1，避免全空数据时 visualMap 的 max 为 0。
  const max = useMemo(() => Math.max(1, ...points.map(([, amount]) => amount)), [points]);
  // 窗口内一个有记录的日子都没有：不铺格子，整卡走空状态。
  const empty = points.every(([, amount]) => amount < 0);

  // 渐变色带：浅端取弱化底色混 30% 主题色，深端取主题色 1 号；分档时由 piecewise
  // 在色带上等分取色。两种画法共用这一条色带定义。
  const base = palette.series[0] ?? "";
  const ramp = useMemo((): [string, string] => [mixHex(palette.surfaceSubtle, base, 0.3), base], [
    palette,
    base,
  ]);

  const option = useMemo(() => {
    // 方形格子：列数随窗口长度变化（一年 ≈ 53 周，30 天 ≈ 5 周），宽预算
    // （减星期标签）÷ 列数与高预算（减月份标签、图例）÷ 7 取小者，夹在
    // [9, 22]（+1 列兜住窗口起点不落在周首的偏移）；日历在剩余空间里居中。
    const totalDays = points.length;
    const cols = Math.max(1, Math.ceil(totalDays / 7) + 1);
    const widthBudget = Math.max(box.w - 34, 0);
    const heightBudget = Math.max(box.h - 46, 0);
    const cell = Math.max(
      9,
      Math.min(Math.floor(widthBudget / cols), Math.floor(heightBudget / 7), 22),
    );
    const offsetX = 28 + Math.floor(Math.max(0, widthBudget - cols * cell) / 2);
    const offsetY = 24 + Math.floor(Math.max(0, heightBudget - 7 * cell) / 2);

    return {
      tooltip: {
        trigger: "item",
        backgroundColor: palette.surface,
        borderColor: palette.border,
        textStyle: { color: palette.ink, fontSize: 11 },
        formatter: (params: CalendarTip) => {
          const [date, amount] = params.value ?? ["", 0];
          // 无记录日（value = -1）只报日期，不摆一排 0。
          if (Number(amount) < 0) {
            return `${String(date)}<br />${strings.legendNone}`;
          }

          const point = pointByDate.get(String(date));

          return [
            String(date),
            `${strings.metricCalls}　${formatCount(point?.calls ?? 0)}`,
            `${strings.metricTokens}　${formatTokens(point?.tokens ?? 0, numberUnit)}`,
            `${strings.metricCost}　${formatCostMicros(point?.costMicros ?? 0)}`,
            `${strings.cardMetricLabel}　${format(Number(amount))}`,
          ].join("<br />");
        },
      },
      // heatmap 必须挂 visualMap（ECharts 硬约束）。无记录日（value = -1）低于
      // min，落到 outOfRange 的固定灰——与色带格同序列同尺寸。
      visualMap:
        steps === CALENDAR_STEPS_CONTINUOUS
          ? {
              type: "continuous",
              min: 0,
              max,
              show: false,
              inRange: { color: ramp },
              outOfRange: { color: palette.border },
            }
          : {
              type: "piecewise",
              splitNumber: steps,
              min: 0,
              max,
              show: false,
              inRange: { color: ramp },
              outOfRange: { color: palette.border },
            },
      calendar: {
        left: offsetX,
        top: offsetY,
        cellSize: [cell, cell],
        range: [windowStart, windowEnd],
        orient: "horizontal",
        dayLabel: {
          firstDay: 1,
          margin: 6,
          // nameMap 数组按星期几索引（0 = 周日），不是按显示行序——firstDay 只管网格对齐。
          nameMap: ["日", "一", "二", "三", "四", "五", "六"],
          color: palette.inkMuted,
          fontSize: 9,
        },
        monthLabel: {
          nameMap: ["1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月"],
          color: palette.inkMuted,
          fontSize: 10,
        },
        yearLabel: { show: false },
        // 没数据的格子露底色；卡片底色描边制造 GitHub 那种格间缝隙。
        itemStyle: { color: palette.surfaceSubtle, borderColor: palette.surface, borderWidth: 2 },
        splitLine: { show: false },
      },
      series: [
        {
          type: "heatmap",
          coordinateSystem: "calendar",
          data: points,
          ...barFocus(),
        },
      ],
    };
  }, [points, max, pointByDate, palette, box, steps, strings, numberUnit, windowStart, windowEnd]);

  // 图例色与 visualMap 的色带同源：连续 = 一条渐变，分档 = 等分取色。
  const rampBase = ramp[0];
  const legend =
    steps === CALENDAR_STEPS_CONTINUOUS ? (
      <span
        className="inline-block h-2.5 w-16 rounded-control"
        style={{ background: `linear-gradient(to right, ${rampBase}, ${base})` }}
      />
    ) : (
      <span className="inline-flex gap-0.5">
        {Array.from({ length: steps }, (_, index) => (
          <span
            key={index}
            className="inline-block size-2.5 rounded-control"
            style={{ background: mixHex(rampBase, base, (index + 1) / steps) }}
          />
        ))}
      </span>
    );

  return (
    <DashboardCard
      id={geo.id}
      x={geo.x}
      y={geo.y}
      w={geo.w}
      h={geo.h}
      title={strings.cardHeatmap}
      subtitle={strings.cardHeatmapSub}
      actions={
        <CardActions
          strings={strings}
          config={config}
          charts={[]}
          onConfigChange={onConfigChange}
          appearance={{ default: { gridLines: false, yAxisZero: false, legend: false } }}
          extraRows={[
            {
              label: strings.cardCalendarWindow,
              node: (
                <Segmented
                  label={strings.cardCalendarWindow}
                  size="sm"
                  options={[
                    { value: "30", label: strings.calendarWindow30 },
                    { value: "90", label: strings.calendarWindow90 },
                    { value: "183", label: strings.calendarWindow183 },
                    { value: "365", label: strings.calendarWindow365 },
                  ]}
                  value={String(windowDays)}
                  onChange={(value) =>
                    onConfigChange({ ...config, calendarWindow: Number(value) })
                  }
                />
              ),
            },
            {
              label: strings.cardCalendarSteps,
              node: (
                <Segmented
                  label={strings.cardCalendarSteps}
                  size="sm"
                  options={[
                    { value: "0", label: strings.calendarStepsContinuous },
                    { value: "4", label: strings.calendarStepsBands.replace("{n}", "4") },
                    { value: "5", label: strings.calendarStepsBands.replace("{n}", "5") },
                    { value: "6", label: strings.calendarStepsBands.replace("{n}", "6") },
                  ]}
                  value={String(steps)}
                  onChange={(value) => onConfigChange({ ...config, calendarSteps: Number(value) })}
                />
              ),
            },
          ]}
        />
      }
    >
      <div ref={bodyRef} className="flex h-full min-h-0 flex-col">
        {empty ? (
          <div className="min-h-0 flex-1">
            <EmptyState kind="grid" label={strings.cardEmpty} />
          </div>
        ) : (
          <>
            <EChart option={option} label={strings.cardHeatmap} className="tt-chart" />
            <p className="flex shrink-0 items-center justify-end gap-1.5 text-xs text-ink-muted">
              {strings.legendNone}
              <span
                className="inline-block size-2.5 rounded-control"
                style={{ background: palette.border }}
              />
              {strings.legendLess}
              {legend}
              {strings.legendMore}
            </p>
          </>
        )}
      </div>
    </DashboardCard>
  );
}
