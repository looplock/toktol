/** 用量卡：一张卡三个视图——模型用量（按模型拆分）、缓存用量（读取 vs 新生成）、
 * 整体趋势（不分段的单色总量）。三个视图共用柱/线/占比三个画法：序列先归一成
 * `{name, values, color}` 的统一形状，画法分支完全不感知当前视图，只管画几条。 */

import { useCallback, useMemo, useState } from "react";

import type { ECharts } from "echarts/core";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCostMicros, formatCount, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import type { CardConfig, UsageView } from "../../lib/overview/layout";
import type { DashboardData, ChartKind, Metric } from "../../lib/overview/types";
import { CardActions } from "./CardActions";
import { EmptyState } from "./EmptyState";
import { playBarEntry } from "./barEntry";
import { stackedTooltipFormatter, useStackedHover } from "./stackedHover";
import { axisDefaults, BAR_GAP, barFocus } from "./optionBase";
import type { CardGeometry } from "./geometry";

interface UsageTrendCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
}

const CHARTS = ["bar", "line", "share"] as const;
// 整体趋势只有一条总量序列，桶内没有份额可看，画法列表里就不出现占比图。
const TOTAL_CHARTS: readonly ChartKind[] = ["bar", "line"];

const ALL_METRICS: readonly Metric[] = ["cost", "tokens", "calls"];
// 缓存没有"次数"口径；整体趋势有次数。
const CACHE_METRICS: readonly Metric[] = ["cost", "tokens"];

// 柱宽 = 类目槽宽 × (1 - GAP)。悬浮背景带的宽度和 barCategoryGap 必须同源；
// 三档视图统一 88%，与其他柱状卡共用 optionBase 的 BAR_GAP。
const GAP = BAR_GAP;

/** 画法分支看到的序列：视图差异（几条、叫什么、什么颜色）在构建处消化掉。 */
interface SeriesSpec {
  readonly name: string;
  readonly values: readonly number[];
  readonly color: string;
}

export function UsageTrendCard({
  geo,
  strings,
  data,
  config,
  onConfigChange,
}: UsageTrendCardProps) {
  const palette = useChartPalette(config.palette);
  const numberUnit = useNumberUnit();
  const view = config.view ?? "model";
  const bars = config.chart === "bar";
  const gap = GAP;
  // 三个视图都从同一份 filtered 聚合而来：总量为 0 则怎么切视图都是空的。
  // 画法图标跟档位走：柱→条形、占比→环形，其余（线/面积）→折线。
  const empty = totalsOf(data, config.metric) === 0;
  const emptyKind = config.chart === "bar" ? "bars" : config.chart === "share" ? "donut" : "line";

  // EChart init 后回传实例：柱状档的悬浮背景带要直接操作 zr。
  const [chart, setChart] = useState<ECharts | null>(null);
  // 压着的系列图形 + 柱状档背景带：mouseover 先于 tooltip 的 axis 路由写入，formatter 据此分流。
  const hoveredSegment = useStackedHover({
    chart,
    bar: bars,
    gap,
    count: data.trend.length,
    bandColor: palette.border,
  });

  // 配置写入的统一入口：视图切换可能让既有画法/指标变成无效组合，就地纠正。
  const handleConfigChange = useCallback(
    (next: CardConfig) => {
      const nextView = next.view ?? "model";
      // 缓存读取没有"次数"口径（省的是 Token，不是调用）。
      if (nextView === "cache" && next.metric === "calls") {
        next = { ...next, metric: "tokens" };
      }
      // 整体趋势只有一条总量序列，桶内没有份额可看。
      if (nextView === "total" && next.chart === "share") {
        next = { ...next, chart: "bar" };
      }

      onConfigChange(next);
    },
    [onConfigChange],
  );

  const option = useMemo(() => {
    const base = axisDefaults(palette, config);
    const colorOf = (index: number): string =>
      // 取模后仍是运行时索引，元组类型兜不住，回退到首色。
      palette.series[index % palette.series.length] ?? palette.series[0];

    // 三个视图各自产出统一形状的序列：画法分支（柱/线/占比）只认 SeriesSpec。
    const seriesSpec: SeriesSpec[] =
      view === "model"
        ? data.modelTrend.map((series, index) => ({
            name: series.model,
            values: data.trend.map((_, bucketIndex) => {
              if (config.metric === "cost") return series.costMicros[bucketIndex] ?? 0;
              if (config.metric === "tokens") return series.tokens[bucketIndex] ?? 0;

              return series.calls[bucketIndex] ?? 0;
            }),
            color: colorOf(index),
          }))
        : view === "cache"
          ? // 费用档取真实的缓存读取费用，新生成 = 总费用 − 缓存费用（各自按单价计，
            // 不做比例折算）。值统一存主单位，tooltip 再折回 micros 格式化。
            [
              {
                name: strings.legendCacheRead,
                values: data.trend.map((bucket) =>
                  config.metric === "cost"
                    ? bucket.cacheReadCostMicros / 1_000_000
                    : bucket.cacheReadTokens,
                ),
                color: palette.series[0] ?? "",
              },
              {
                name: strings.legendFresh,
                values: data.trend.map((bucket) =>
                  config.metric === "cost"
                    ? Math.max(
                        bucket.costMicros / 1_000_000 - bucket.cacheReadCostMicros / 1_000_000,
                        0,
                      )
                    : Math.max(bucket.tokens - bucket.cacheReadTokens, 0),
                ),
                color: palette.series[3] ?? palette.series[0] ?? "",
              },
            ]
          : // 整体趋势：不分段的单序列，每根柱/点就是当天总量。
            [
              {
                name: strings.legendTotal,
                values: data.trend.map((bucket) => {
                  if (config.metric === "cost") return bucket.costMicros;
                  if (config.metric === "tokens") return bucket.tokens;

                  return bucket.calls;
                }),
                color: palette.series[0] ?? "",
              },
            ];

    const metricFormatter = (value: number) =>
      view === "cache" && config.metric === "cost"
        ? formatCostMicros(value * 1_000_000)
        : config.metric === "cost"
          ? formatCostMicros(value)
          : config.metric === "tokens"
            ? formatTokens(value, numberUnit)
            : formatCount(value);
    // 整体趋势只有一条序列，图例没有信息量，直接不画——网格不用给它留位置。
    const legendShown = view !== "total" && (config.showLegend ?? true);
    const legend = {
      bottom: 0,
      type: "scroll",
      show: legendShown,
      itemWidth: 8,
      itemHeight: 8,
      icon: "circle",
      textStyle: { color: palette.inkMuted, fontSize: 10 },
      pageIconColor: palette.ink,
      pageIconInactiveColor: palette.border,
      pageTextStyle: { color: palette.inkMuted, fontSize: 10 },
    };
    const grid = { ...(base["grid"] as Record<string, unknown>), bottom: legendShown ? 26 : 22 };
    const xAxis = (boundaryGap: boolean) => ({
      ...(base["xAxis"] as Record<string, unknown>),
      boundaryGap,
      data: data.trend.map((b) => b.label),
    });
    // 三个画法都用 axis 触发：item 要求鼠标下的元素带 dataIndex（柱子是逐根的元素，行），
    // 折线/面积是整条序列一个元素、没有 dataIndex，item 悬浮永远走 hide。axis 按
    // 时间桶取整列，再过滤 0 值并按值降序，免得一串 0 顶走有用的行。
    const tooltipOf = (formatValue: (value: number) => string): Record<string, unknown> => ({
      ...(base["tooltip"] as Record<string, unknown>),
      trigger: "axis",
      formatter: stackedTooltipFormatter(hoveredSegment, formatValue),
    });
    // 柱/线档的值轴刻度与 tooltip 同一套格式化：tokens 跟随单位制设置（万/亿、K/M、无单位）。
    const yAxis = {
      ...(base["yAxis"] as Record<string, unknown>),
      axisLabel: {
        ...((base["yAxis"] as Record<string, unknown>)["axisLabel"] as Record<string, unknown>),
        formatter: metricFormatter,
      },
    };

    if (config.chart === "share") {
      // 百分比堆叠面积：桶内归一化到 100%，看的是份额格局而不是绝对量。
      const totals = data.trend.map((_, bucketIndex) =>
        seriesSpec.reduce((sum, series) => sum + (series.values[bucketIndex] ?? 0), 0),
      );

      return {
        ...base,
        grid,
        legend,
        xAxis: xAxis(false),
        yAxis: {
          ...(base["yAxis"] as Record<string, unknown>),
          max: 100,
          axisLabel: {
            color: palette.inkMuted,
            fontSize: 10,
            formatter: "{value}%",
          },
        },
        tooltip: { ...tooltipOf((value: number) => `${value.toFixed(1)}%`), axisPointer: { type: "line" } },
        series: seriesSpec.map((series) => ({
          name: series.name,
          type: "line",
          stack: "total",
          smooth: true,
          symbol: "none",
          // 面积要能派发图表事件（mouseover 记段用）：折线/面积默认不带 eventData。
          triggerEvent: true,
          data: series.values.map((value, bucketIndex) => {
            const total = totals[bucketIndex] ?? 0;

            return total === 0 ? 0 : (value / total) * 100;
          }),
          color: series.color,
          lineStyle: { width: 1, opacity: 0.6 },
          areaStyle: { opacity: 0.85 },
          emphasis: { focus: "series" },
        })),
      };
    }

    if (config.chart === "line") {
      return {
        ...base,
        grid,
        legend,
        xAxis: xAxis(false),
        yAxis,
        tooltip: { ...tooltipOf(metricFormatter), axisPointer: { type: "line" } },
        series: seriesSpec.map((series) => ({
          name: series.name,
          type: "line",
          smooth: true,
          symbol: "none",
          // 线要能派发图表事件（mouseover 记段用）：折线默认不带 eventData。
          triggerEvent: true,
          data: series.values,
          color: series.color,
          lineStyle: { width: 2 },
          areaStyle: { opacity: 0.14 },
          emphasis: { focus: "series" },
        })),
      };
    }

    // 柱状档就是堆叠柱：每根柱按序列分段摞起来（整体趋势只有一段，就是单色柱），
    // 看各部分的绝对量构成。
    // 不加轴指示器：柱子本身就是定位物，再画线只会把柱子劈成两半。
    // triggerEmphasis 关掉：axis 悬浮时 echarts 默认把整列段高亮发亮，柱状档不要；
    // 柱体段上的反馈由 barFocus 的 focus self 负责。
    // animationDuration 归零：原生进场是"锚在最终上边缘、向下展开"（BarView 建元素
    // 时 height=0），堆叠分段各自展开、交界处短暂脱开，做不出整柱锁死的生长；进场
    // 改由 zr 层的公共锚点 scaleY 接手（见下方 effect）。更新动画（450ms）不受影响。
    return {
      ...base,
      grid,
      legend,
      animationDuration: 0,
      xAxis: { ...xAxis(true), axisPointer: { triggerEmphasis: false } },
      yAxis,
      tooltip: tooltipOf(metricFormatter),
      series: seriesSpec.map((series) => ({
        name: series.name,
        type: "bar",
        stack: "total",
        data: series.values,
        color: series.color,
        barCategoryGap: `${gap * 100}%`,
        ...barFocus(),
        itemStyle: { borderRadius: 0 },
      })),
    };
  }, [data, config, palette, numberUnit, hoveredSegment, view, strings]);

  // rebuilt = 本次整体重建（首建/切档位）——元素是新的才值得播；数据/调色板变化
  // 走合并更新复用元素，重播会突兀。
  const handleApplied = useCallback(
    (instance: ECharts, rebuilt: boolean) => {
      if (rebuilt && bars) playBarEntry(instance);
    },
    [bars],
  );

  // 标题与副标题跟视图走：视图本身就是这张卡在看什么。
  const title =
    view === "cache"
      ? strings.cardCache
      : view === "total"
        ? strings.cardUsageTotal
        : strings.cardUsageTrend;
  const subtitle =
    view === "cache"
      ? strings.cardCacheSub
      : view === "total"
        ? strings.cardUsageTotalSub
        : strings.cardUsageTrendSub;

  return (
    <DashboardCard
      id={geo.id}
      x={geo.x}
      y={geo.y}
      w={geo.w}
      h={geo.h}
      title={title}
      subtitle={subtitle}
      actions={
        <CardActions
          strings={strings}
          config={config}
          charts={view === "total" ? TOTAL_CHARTS : CHARTS}
          metrics={view === "cache" ? CACHE_METRICS : ALL_METRICS}
          onConfigChange={handleConfigChange}
          views={[
            { value: "model", label: strings.usageViewModel },
            { value: "cache", label: strings.usageViewCache },
            { value: "total", label: strings.usageViewTotal },
          ]}
          viewValue={view}
          onViewChange={(value) => handleConfigChange({ ...config, view: value as UsageView })}
          appearance={{
            bar: { legend: true, gridLines: true, yAxisZero: true },
            line: { legend: true, gridLines: true, yAxisZero: true },
            share: { legend: true, gridLines: true },
          }}
        />
      }
    >
      {empty ? (
        <EmptyState kind={emptyKind} label={strings.cardEmpty} />
      ) : (
        <EChart
          option={option}
          label={title}
          className="tt-chart"
          onReady={setChart}
          onAfterApply={handleApplied}
        />
      )}
    </DashboardCard>
  );
}

function totalsOf(data: DashboardData, metric: Metric): number {
  if (metric === "cost") return data.totals.costMicros;
  if (metric === "tokens") return data.totals.tokens;

  return data.totals.calls;
}
