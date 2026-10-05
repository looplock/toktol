// 双轴组合卡，两种视图（⚙ 切换）：calls = 柱是这一段的调用量、线是累计花费
// （看累计斜率——量还在涨而线变陡才是真的"越花越快"）；cost = 柱是这一段的费用、
// 线是调用次数（看单段花销和调用节奏的对应）。

import { useMemo, useState } from "react";

import type { ECharts } from "echarts/core";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCostMicros, formatCount } from "../../lib/format";
import { type ComboView, type CardConfig } from "../../lib/overview/layout";
import type { DashboardData } from "../../lib/overview/types";
import type { CardGeometry } from "./geometry";
import { CardActions } from "./CardActions";
import { EmptyState } from "./EmptyState";
import { BAR_GAP, axisDefaults, barFocus, barStagger } from "./optionBase";
import { stackedTooltipFormatter, useStackedHover } from "./stackedHover";

interface ComboCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
}

export function ComboCard({ geo, strings, data, config, onConfigChange }: ComboCardProps) {
  const palette = useChartPalette(config.palette);
  const view: ComboView = config.comboView ?? "calls";
  const callsView = view === "calls";
  // 悬浮背景带要直接操作 zr，实例经 onReady 回传。
  const [chart, setChart] = useState<ECharts | null>(null);
  const hovered = useStackedHover({
    chart,
    bar: true,
    gap: BAR_GAP,
    count: data.trend.length,
    bandColor: palette.border,
  });
  // 两个视图共用调用与花费两份数据：双双为 0 才算空。
  const empty = data.totals.calls === 0 && data.totals.costMicros === 0;

  const option = useMemo(() => {
    const base = axisDefaults(palette);
    let running = 0;
    const cumulative = data.trend.map((bucket) => {
      running += bucket.costMicros / 1_000_000;

      return Number(running.toFixed(4));
    });
    const calls = data.trend.map((bucket) => bucket.calls);
    const dailyCost = data.trend.map((bucket) =>
      Number((bucket.costMicros / 1_000_000).toFixed(4)),
    );

    // 两种视图互为镜像：柱和线交换指标，轴的量纲跟着换。
    const barData = callsView ? calls : dailyCost;
    const barName = callsView ? strings.metricCalls : strings.metricCost;
    const lineData = callsView ? cumulative : calls;
    const lineName = callsView ? strings.legendCumulativeCost : strings.metricCalls;
    // 行格式按系列分流（seriesIndex：0 = 柱、1 = 线），量纲随视图换。
    const formatValue = (value: number, seriesIndex?: number): string => {
      const isCost = callsView ? seriesIndex === 1 : seriesIndex === 0;

      return isCost ? formatCostMicros(value * 1_000_000) : formatCount(value);
    };

    return {
      ...base,
      legend: {
        bottom: 0,
        itemWidth: 8,
        itemHeight: 8,
        icon: "circle",
        data: [barName, lineName],
        textStyle: { color: palette.inkMuted, fontSize: 10 },
      },
      grid: { ...(base["grid"] as Record<string, unknown>), bottom: 22 },
      xAxis: { ...(base["xAxis"] as Record<string, unknown>), data: data.trend.map((b) => b.label) },
      yAxis: [
        {
          ...(base["yAxis"] as Record<string, unknown>),
          axisLabel: {
            color: palette.inkMuted,
            fontSize: 10,
            formatter: (v: number) => (callsView ? formatCount(v) : formatCostMicros(v * 1_000_000)),
          },
        },
        {
          ...(base["yAxis"] as Record<string, unknown>),
          splitLine: { show: false },
          axisLabel: {
            color: palette.inkMuted,
            fontSize: 10,
            formatter: (v: number) => (callsView ? formatCostMicros(v * 1_000_000) : formatCount(v)),
          },
        },
      ],
      tooltip: {
        ...(base["tooltip"] as Record<string, unknown>),
        formatter: stackedTooltipFormatter(hovered, formatValue),
      },
      series: [
        {
          name: barName,
          type: "bar",
          data: barData,
          barCategoryGap: `${BAR_GAP * 100}%`,
          ...barStagger(),
          ...barFocus(),
          itemStyle: { color: palette.series[0], borderRadius: 0 },
        },
        {
          name: lineName,
          type: "line",
          yAxisIndex: 1,
          data: lineData,
          // 线的形态跟视图走：累计线折线带点（逐段可读），调用节奏线平滑无点（只看起伏）。
          smooth: !callsView,
          symbol: callsView ? "circle" : "none",
          symbolSize: 4,
          // triggerEvent 让线也进"压着谁"的判定：压线时 tooltip 淡化柱行。
          triggerEvent: true,
          emphasis: { focus: "series" },
          lineStyle: { width: 2, color: palette.series[2] },
          itemStyle: { color: palette.series[2] },
        },
      ],
    };
  }, [data, palette, strings, hovered, callsView]);

  const title = callsView ? strings.cardCombo : strings.cardCostCalls;
  const subtitle = callsView ? strings.cardComboSub : strings.cardCostCallsSub;

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
          charts={[]}
          // 指标由"视图"决定（柱与线各是什么），不单独暴露指标行。
          showMetric={false}
          onConfigChange={onConfigChange}
          appearance={{ default: { gridLines: false, yAxisZero: false, legend: false } }}
          views={[
            { value: "calls", label: strings.comboViewCalls },
            { value: "cost", label: strings.comboViewCost },
          ]}
          viewValue={view}
          onViewChange={(value: string) =>
            onConfigChange({ ...config, comboView: value as ComboView })
          }
        />
      }
    >
      {empty ? (
        <EmptyState kind="bars" label={strings.cardEmpty} />
      ) : (
        <EChart
          option={option}
          label={title}
          className="tt-chart"
          onReady={setChart}
        />
      )}
    </DashboardCard>
  );
}
