// 箱线图：单次调用开销的分布。中位数看量级，箱须和离群点看异常——
// 平均值会被几个超长会话带偏，这张图就是为了不给平均值骗。

import { useMemo } from "react";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCostMicros, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import type { CardConfig } from "../../lib/overview/layout";
import type { DashboardData } from "../../lib/overview/types";
import { CardActions } from "./CardActions";
import { EmptyState } from "./EmptyState";
import type { CardGeometry } from "./geometry";
import { barFocus } from "./optionBase";

interface SpreadCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
}

export function SpreadCard({ geo, strings, data, config, onConfigChange }: SpreadCardProps) {
  const palette = useChartPalette(config.palette);
  const numberUnit = useNumberUnit();

  const option = useMemo(() => {
    const cost = config.metric === "cost";
    const axisFormat = (value: number): string =>
      cost ? formatCostMicros(value) : formatTokens(value, numberUnit);

    return {
      grid: { left: 4, right: 12, top: 10, bottom: 2, containLabel: true },
      tooltip: {
        trigger: "item",
        backgroundColor: palette.surface,
        borderColor: palette.border,
        textStyle: { color: palette.ink, fontSize: 11 },
        // 五数与值轴同一格式化：tokens 跟随单位制设置（万/亿、K/M、无单位）。
        // 箱线系列的存储值被 ECharts 前置了一个类目序号（whiskerBox 初始化时
        // item.unshift(index)，绘制用的 dim 1-5 不受影响）：[序号, 最小, Q1,
        // 中位, Q3, 最大]，先剥掉序号再读五数，否则整体错位一格。
        formatter: (params: { name?: string; value?: unknown }) => {
          const stored = Array.isArray(params.value) ? (params.value as number[]) : [];
          const values = stored.length === 6 ? stored.slice(1) : stored;
          const rows: readonly (readonly [string, number])[] = [
            [strings.boxplotMin, values[0] ?? 0],
            [strings.boxplotQ1, values[1] ?? 0],
            [strings.boxplotMedian, values[2] ?? 0],
            [strings.boxplotQ3, values[3] ?? 0],
            [strings.boxplotMax, values[4] ?? 0],
          ];

          return `${params.name ?? ""}<br />${rows
            .map(([label, value]) => `${label}　${axisFormat(value)}`)
            .join("<br />")}`;
        },
      },
      xAxis: {
        type: "category",
        data: data.spread.map((row) => row.label),
        axisLine: { lineStyle: { color: palette.border } },
        axisTick: { show: false },
        axisLabel: { color: palette.inkMuted, fontSize: 9, interval: 0, rotate: 20 },
      },
      yAxis: {
        type: "value",
        splitLine: { show: config.showGridLines ?? true, lineStyle: { color: palette.border } },
        axisLine: { show: false },
        axisLabel: { color: palette.inkMuted, fontSize: 9, formatter: axisFormat },
        // 箱线图看分布，钉住零点会压扁箱体，所以零点档只在用户明确要"从 0 开始"时生效。
        ...(config.yAxisFromZero ? { min: 0 } : {}),
      },
      series: [
        {
          type: "boxplot",
          data: data.spread.map((row) => (cost ? [...row.costMicros] : [...row.tokens])),
          boxWidth: [8, 26],
          ...barFocus(),
          // 箱线系列原生悬浮自带"放大 + 投影"（ECharts 默认 emphasis），是全页唯一
          // 的例外——其他 item 悬浮卡都只有 focus self 淡化、无列带。压掉 scale 与
          // 阴影后，悬浮效果统一为"仅淡化"。
          emphasis: { focus: "self", scale: false, itemStyle: { borderWidth: 1, shadowBlur: 0 } },
          itemStyle: { color: palette.series[0], borderColor: palette.inkMuted, borderWidth: 1 },
        },
      ],
    };
  }, [data, config, palette, numberUnit]);

  return (
    <DashboardCard
      id={geo.id}
      x={geo.x}
      y={geo.y}
      w={geo.w}
      h={geo.h}
      title={strings.cardSpread}
      subtitle={strings.cardSpreadSub}
      actions={
        <CardActions
          strings={strings}
          config={config}
          charts={[]}
          onConfigChange={onConfigChange}
          appearance={{ default: { gridLines: true, yAxisZero: true } }}
        />
      }
    >
      {data.spread.length === 0 ? (
        <EmptyState kind="box" label={strings.cardEmpty} />
      ) : (
        <EChart option={option} label={strings.cardSpread} className="tt-chart" />
      )}
    </DashboardCard>
  );
}