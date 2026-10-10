// 构成类卡片：按某一维度（模型 / 项目）拆分用量。未定价的必须显示成"未知"而不是 0——
// 本产品的隐私红线之一就是不为未定价的东西估价。

import { useCallback, useMemo } from "react";

import type { ECharts } from "echarts/core";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCount, formatCostMicros, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import type { CardConfig } from "../../lib/overview/layout";
import type { BreakdownRow, BreakdownDimension, ChartKind } from "../../lib/overview/types";
import { CardActions, DonutIcon } from "./CardActions";
import { EmptyState } from "./EmptyState";
import { playTreemapEntry } from "./treemapEntry";
import type { CardGeometry } from "./geometry";
import { barFocus, barStagger } from "./optionBase";

interface TreemapTip {
  readonly name?: string;
  readonly value?: unknown;
}

interface BreakdownCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly title: string;
  readonly subtitle: string;
  readonly rows: readonly BreakdownRow[];
  readonly charts: readonly ChartKind[];
  readonly note?: string | undefined;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
  /** 维度切换的选项集：给了就在操作区最左侧渲染"模型/项目/工具"切换。 */
  readonly dimensions?: readonly {
    readonly value: BreakdownDimension;
    readonly label: string;
  }[];
}

export function BreakdownCard({
  geo,
  strings,
  title,
  subtitle,
  rows,
  charts,
  note,
  config,
  onConfigChange,
  dimensions,
}: BreakdownCardProps) {
  const palette = useChartPalette(config.palette);
  const numberUnit = useNumberUnit();

  // 当前指标下全 0 即空：条形/树图用条形图标，环形占比用圆环图标。
  // every 对空数组返回 true，length 检查是多余的。
  const empty = rows.every((row) => value(row, config.metric) === 0);
  const emptyKind = config.chart === "share" ? "donut" : "bars";

  const option = useMemo(() => {
    const sorted = [...rows].sort((a, b) => value(b, config.metric) - value(a, config.metric));
    const items = sorted.map((row, index) => ({
      name: row.label,
      value: config.metric === "cost" ? row.costMicros / 1_000_000 : value(row, config.metric),
      itemStyle: {
        color: row.unknownCost ? palette.border : palette.series[index % palette.series.length],
      },
    }));
    const formatValue = (amount: number): string =>
      config.metric === "cost"
        ? formatCostMicros(amount * 1_000_000)
        : config.metric === "tokens"
          ? formatTokens(amount, numberUnit)
          : formatCount(amount);

    if (config.chart === "treemap") {
      return {
        tooltip: {
          backgroundColor: palette.surface,
          borderColor: palette.border,
          textStyle: { color: palette.ink, fontSize: 11 },
          formatter: (params: TreemapTip) => `${params.name}　${formatValue(Number(params.value ?? 0))}`,
        },
        series: [
          {
            type: "treemap",
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
            roam: false,
            nodeClick: false,
            breadcrumb: { show: false },
            ...barFocus(),
            itemStyle: { borderColor: palette.surface, borderWidth: 1, gapWidth: 1 },
            data: items,
            label: { color: palette.ink, fontSize: 10, overflow: "truncate" },
          },
        ],
      };
    }

    if (config.chart === "share") {
      return {
        tooltip: {
          trigger: "item",
          backgroundColor: palette.surface,
          borderColor: palette.border,
          textStyle: { color: palette.ink, fontSize: 11 },
          formatter: (params: { name?: string; value?: number }) =>
            `${params.name ?? ""}　${formatValue(Number(params.value ?? 0))}`,
        },
        series: [
          {
            type: "pie",
            radius: ["45%", "72%"],
            data: items,
            // 与柱状图同一套：悬浮一块扇区，其余变淡。
            ...barFocus(),
            itemStyle: { borderColor: palette.surface, borderWidth: 2 },
            label: { color: palette.inkMuted, fontSize: 10 },
          },
        ],
      };
    }

    return {
      grid: { left: 4, right: 12, top: 8, bottom: 2, containLabel: true },
      tooltip: {
        trigger: "axis",
        axisPointer: { type: "shadow" },
        formatter: (raw: unknown) => {
          const params = (Array.isArray(raw) ? raw[0] : raw) as
            | { name?: string; value?: number }
            | undefined;

          return `${params?.name ?? ""}　${formatValue(Number(params?.value ?? 0))}`;
        },
      },
      xAxis: {
        type: "value",
        // 横向条的网格线画在值轴（x）上，跟随网格线开关。刻度与 tooltip 同一
        // 格式化：tokens 跟随单位制设置（万/亿、K/M、无单位）。
        splitLine: { show: config.showGridLines ?? true },
        axisLabel: { color: palette.inkMuted, fontSize: 10, formatter: formatValue },
      },
      yAxis: {
        type: "category",
        data: items.map((item) => item.name),
        axisLabel: { color: palette.inkMuted, fontSize: 10 },
        axisLine: { lineStyle: { color: palette.border } },
      },
      series: [
        {
          type: "bar",
          data: items,
          barCategoryGap: "20%",
          ...barStagger(),
          ...barFocus(),
          itemStyle: { borderRadius: 0 },
        },
      ],
    };
  }, [rows, config, palette, numberUnit]);

  // rebuilt = 本次整体重建（首建/切档位）——元素是新的才值得播；数据/调色板变化
  // 走合并更新复用元素（ treemap 原生的块面挪位动画照常播），重播会突兀。
  const handleApplied = useCallback(
    (instance: ECharts, rebuilt: boolean) => {
      if (rebuilt && config.chart === "treemap") playTreemapEntry(instance);
    },
    [config.chart],
  );

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
          charts={charts}
          onConfigChange={onConfigChange}
          dimensions={dimensions}
          // 构成卡的占比档是环形 pie，与面积卡共用 "share" 档位，图标单独覆盖。
          chartIcons={{ share: DonutIcon }}
          appearance={{ bar: { gridLines: true } }}
        />
      }
    >
      <div className="flex h-full flex-col">
        {empty ? (
          <EmptyState kind={emptyKind} label={strings.cardEmpty} />
        ) : (
          <EChart
            option={option}
            label={title}
            className="tt-chart"
            onAfterApply={handleApplied}
          />
        )}
        {note === undefined ? null : <p className="shrink-0 text-xs text-ink-muted">{note}</p>}
      </div>
    </DashboardCard>
  );
}

function value(row: BreakdownRow, metric: CardConfig["metric"]): number {
  if (metric === "cost") return row.costMicros;
  if (metric === "tokens") return row.tokens;

  return row.calls;
}
