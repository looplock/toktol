// 流向图：工具 → 模型 → 项目，带宽即用量。节点不另存一份，从链路两端推出来。

import { useMemo } from "react";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCostMicros, formatCount, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import type { CardConfig } from "../../lib/overview/layout";
import type { DashboardData, FlowLink } from "../../lib/overview/types";
import { CardActions } from "./CardActions";
import { EmptyState } from "./EmptyState";
import type { CardGeometry } from "./geometry";
import { flowFocus } from "./optionBase";

interface SankeyCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
}

interface SankeyTip {
  readonly dataType?: string;
  readonly name?: string;
  readonly value?: unknown;
  readonly data?: { readonly source?: string; readonly target?: string };
}

export function SankeyCard({ geo, strings, data, config, onConfigChange }: SankeyCardProps) {
  const palette = useChartPalette(config.palette);
  const numberUnit = useNumberUnit();
  // 当前指标下没有带宽大于 0 的链路即空。
  const empty = !data.flow.some((link) => sankeyAmount(link, config.metric) > 0);

  const option = useMemo(() => {
    const metric = config.metric;
    const amount = (link: FlowLink): number => sankeyAmount(link, metric);
    const format = (value: number): string =>
      metric === "cost"
        ? formatCostMicros(value * 1_000_000)
        : metric === "tokens"
          ? formatTokens(value, numberUnit)
          : formatCount(value);

    const nodes = new Map<string, number>();
    for (const link of data.flow) {
      nodes.set(link.source, 0);
      nodes.set(link.target, 0);
    }

    const links = data.flow
      .map((link) => ({ source: link.source, target: link.target, value: amount(link) }))
      .filter((link) => link.value > 0);

    return {
      tooltip: {
        trigger: "item",
        backgroundColor: palette.surface,
        borderColor: palette.border,
        textStyle: { color: palette.ink, fontSize: 11 },
        formatter: (params: SankeyTip) => {
          const value = format(Number(params.value ?? 0));

          return params.dataType === "edge"
            ? `${params.data?.source} → ${params.data?.target}　${value}`
            : `${params.name}　${value}`;
        },
      },
      series: [
        {
          type: "sankey",
          left: 4,
          right: 96,
          top: 8,
          bottom: 8,
          // 节点名按出现顺序取色，同一层不撞色。
          data: [...nodes.keys()].map((name, index) => ({
            name,
            itemStyle: { color: palette.series[index % palette.series.length], borderWidth: 0 },
          })),
          links,
          ...flowFocus(),
          nodeGap: 8,
          nodeWidth: 12,
          label: { color: palette.inkMuted, fontSize: 10 },
          lineStyle: { color: "gradient", opacity: 0.45, curveness: 0.5 },
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
      title={strings.cardSankey}
      subtitle={strings.cardSankeySub}
      actions={
        <CardActions strings={strings} config={config} charts={[]} onConfigChange={onConfigChange} />
      }
    >
      {empty ? (
        <EmptyState kind="flow" label={strings.cardEmpty} />
      ) : (
        <EChart option={option} label={strings.cardSankey} className="tt-chart" />
      )}
    </DashboardCard>
  );
}

function sankeyAmount(link: FlowLink, metric: CardConfig["metric"]): number {
  if (metric === "cost") return link.costMicros / 1_000_000;
  if (metric === "tokens") return link.tokens;

  return link.calls;
}
