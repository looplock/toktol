// 费用构成瀑布：透明辅助系列垫高起点 + 可见值系列堆叠出悬浮柱，虚线用 markLine
// 把每根柱顶连到下一根柱底。ECharts 没有原生瀑布系列，这是官方示例同款画法。
// 悬浮与其他柱状卡统一：同宽背景带 + "分项费用 / 累计"两行 tooltip。

import { useMemo, useState } from "react";

import type { ECharts } from "echarts/core";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCostMicros } from "../../lib/format";
import type { DashboardData } from "../../lib/overview/types";
import type { CardGeometry } from "./geometry";
import { EmptyState } from "./EmptyState";
import { BAR_GAP, axisDefaults, barFocus } from "./optionBase";
import { stackedTooltipFormatter, useStackedHover } from "./stackedHover";

interface CostWaterfallCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
}

interface TipParam {
  readonly seriesName?: string;
  readonly value?: number;
  readonly dataIndex?: number;
}

export function CostWaterfallCard({ geo, strings, data }: CostWaterfallCardProps) {
  const palette = useChartPalette();
  // 五个分项全 0 即空（未定价份额不进分项，也撑不起一张瀑布）。
  const c = data.costComposition;
  const empty =
    c.inputCostMicros +
      c.outputCostMicros +
      c.reasoningCostMicros +
      c.cacheReadCostMicros +
      c.cacheWriteCostMicros ===
    0;
  // 悬浮背景带要直接操作 zr，实例经 onReady 回传。柱宽 88% → 间隙 = BAR_GAP。
  const [chart, setChart] = useState<ECharts | null>(null);
  // 类目固定 6 个（五个分项 + 总计）。
  const hovered = useStackedHover({
    chart,
    bar: true,
    gap: BAR_GAP,
    count: 6,
    bandColor: palette.border,
  });

  const option = useMemo(() => {
    const composition = data.costComposition;
    const parts = [
      { label: strings.waterfallInput, micros: composition.inputCostMicros },
      { label: strings.waterfallOutput, micros: composition.outputCostMicros },
      {
        label: strings.waterfallReasoning,
        micros: composition.reasoningCostMicros,
      },
      {
        label: strings.waterfallCacheRead,
        micros: composition.cacheReadCostMicros,
      },
      {
        label: strings.waterfallCacheWrite,
        micros: composition.cacheWriteCostMicros,
      },
    ];
    // 垫高值 = 该柱之前的累计和（首根为 0，总计柱从 0 起）；单位统一换算成美元。
    const dollars = parts.map((part) => part.micros / 1_000_000);
    const cumulative: number[] = [];
    let run = 0;

    for (const value of dollars) {
      run += value;
      cumulative.push(run);
    }

    const values = [...dollars, run];
    const helpers = [0, ...cumulative.slice(0, -1), 0];
    const labels = [...parts.map((part) => part.label), strings.waterfallTotal];
    // 连接线第 i 段：第 i 根柱的顶（累计 i）连到第 i+1 根柱的底（同一高度）。
    const marks = cumulative.map((level, index) => [
      { coord: [index, level] },
      { coord: [index + 1, level] },
    ]);

    const base = axisDefaults(palette);
    const grid = base["grid"] as Record<string, number>;
    const tooltip = base["tooltip"] as Record<string, unknown>;

    return {
      ...base,
      grid,
      xAxis: { ...(base["xAxis"] as Record<string, unknown>), data: labels },
      yAxis: {
        ...(base["yAxis"] as Record<string, unknown>),
        // 顶部留白给数值标签，否则最高柱的标签会被裁掉。
        max: (value: { max: number }) => value.max * 1.18,
        axisLabel: {
          ...((base["yAxis"] as Record<string, unknown>)[
            "axisLabel"
          ] as Record<string, unknown>),
          formatter: (v: number) => formatCostMicros(v * 1_000_000),
        },
      },
      tooltip: {
        ...tooltip,
        trigger: "axis",
        // 与堆叠柱同一款 formatter（色点、压着淡化）；辅助系列对 tooltip 隐身
        // （series 级 tooltip.show=false），列表里只有真实值。分类名要按 dataIndex 取：
        // params 在轴触发下不保证与分类同序，只有它跟着柱子走。
        formatter: (params: TipParam[]) => {
          const item = params.find(
            (param) => param.seriesName === strings.metricCost,
          );
          const index = item?.dataIndex ?? -1;
          if (index < 0) return "";

          // 第二行是累计：瀑布的"看到哪一步、走到多高"就体现在这里。
          const accumulated = index < cumulative.length ? (cumulative[index] ?? 0) : run;
          const main = stackedTooltipFormatter(hovered, (value) =>
            formatCostMicros(value * 1_000_000),
          )([item]);
          const total =
            `<div style="display:flex;align-items:center;justify-content:space-between;gap:16px;min-width:170px;` +
            `border-top:1px solid ${palette.border};margin-top:4px;padding-top:4px">` +
            `<span style="color:${palette.inkMuted}">${strings.legendCumulativeCost}</span>` +
            `<span>${formatCostMicros(accumulated * 1_000_000)}</span></div>`;

          return main + total;
        },
      },
      series: [
        {
          name: "helper",
          type: "bar",
          stack: "waterfall",
          silent: true,
          barWidth: "88%",
          data: helpers,
          itemStyle: { color: "transparent" },
          emphasis: { itemStyle: { color: "transparent" } },
          tooltip: { show: false },
        },
        {
          name: strings.metricCost,
          type: "bar",
          stack: "waterfall",
          barWidth: "88%",
          data: values,
          ...barFocus(),
          itemStyle: {
            borderRadius: 0,
            // 分项逐个取系列色，总计压轴用第一色收尾，与分类色区分开。
            color: (param: { dataIndex: number }) =>
              palette.series[
                param.dataIndex < parts.length ? param.dataIndex : 0
              ],
          },
          label: {
            show: true,
            position: "top",
            color: palette.inkMuted,
            fontSize: 10,
            formatter: (param: { value: number }) =>
              formatCostMicros(param.value * 1_000_000),
          },
          markLine: {
            silent: true,
            animation: false,
            symbol: "none",
            label: { show: false },
            lineStyle: {
              type: "dashed",
              color: palette.inkMuted,
              width: 1,
              opacity: 0.6,
            },
            data: marks,
          },
        },
      ],
    };
  }, [data, palette, strings]);

  // 未定价模型的费用不进总数：有未知份额时在副标题里说出来，而不是让总数显得完整。
  const subtitle =
    data.unknownModelCount === 0
      ? strings.cardCostWaterfallSub
      : `${strings.cardCostWaterfallSub} · ${strings.unknownModelsNote.replace("{n}", String(data.unknownModelCount))}`;

  return (
    <DashboardCard
      id={geo.id}
      x={geo.x}
      y={geo.y}
      w={geo.w}
      h={geo.h}
      title={strings.cardCostWaterfall}
      subtitle={subtitle}
    >
      {empty ? (
        <EmptyState kind="stairs" label={strings.cardEmpty} />
      ) : (
        <EChart
          option={option}
          label={strings.cardCostWaterfall}
          className="tt-chart"
          onReady={setChart}
        />
      )}
    </DashboardCard>
  );
}
