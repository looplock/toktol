// 模型效率 / 会话消耗散点（右上角视图切换）：一个气泡一个模型或会话（× 模型），
// 横轴调用次数、纵轴单次均价、气泡大小为 Token 量。左上 = 少调用却贵，右下 =
// 高频且便宜。未定价模型没有"均价"可言（隐私红线：不估价），只能整点排除，卡片
// 底部注明；会话切片同样只含有价部分（全部行未定价的会话不在切片里）。

import { useMemo } from "react";

import { DashboardCard } from "./DashboardCard";
import { EChart } from "../../components/charts/EChart";
import type { Strings } from "../../i18n/strings";
import { useChartPalette } from "../../lib/chartTheme";
import { formatCount, formatCostMicros, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import { type CardConfig, type EfficiencyView } from "../../lib/overview/layout";
import type { DashboardData } from "../../lib/overview/types";
import type { CardGeometry } from "./geometry";
import { CardActions } from "./CardActions";
import { EmptyState } from "./EmptyState";
import { barFocus } from "./optionBase";

interface ModelEfficiencyCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
  readonly config: CardConfig;
  readonly onConfigChange: (config: CardConfig) => void;
}

/** 气泡数据项：value = [调用次数, 单次均价（USD）]，tokens/costMicros 给气泡大小和悬浮合计用。
 * session 只在会话视图出现：tooltip 靠它报出"这是哪次工作"（项目 + 时间范围）。 */
interface EfficiencyPoint {
  readonly name: string;
  readonly value: readonly [number, number];
  readonly tokens: number;
  readonly costMicros: number;
  readonly itemStyle: { readonly color: string };
  readonly session?:
    | {
        readonly projectDir: string;
        readonly startMs: number;
        readonly endMs: number;
      }
    | undefined;
}

interface EfficiencyTip {
  readonly data?: EfficiencyPoint;
}

const BUBBLE_MIN_PX = 9;
const BUBBLE_MAX_PX = 34;

/** 路径尾段：tooltip 头部显示项目名而非全路径（长路径会把 tooltip 撑爆）；
 * 空/根路径返回空串，由调用方落"无项目"哨兵。 */
function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);

  return parts[parts.length - 1] ?? "";
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function formatClock(ms: number): string {
  const d = new Date(ms);

  return `${pad2(d.getMonth() + 1)}-${pad2(d.getDate())} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

/** 会话时间跨度：同一天只补结束的时分，跨天两端都带日期。 */
function formatSpan(startMs: number, endMs: number): string {
  const start = formatClock(startMs);
  const startDay = start.slice(0, 5);
  const end = formatClock(endMs);

  return end.startsWith(startDay) ? `${start} → ${end.slice(6)}` : `${start} → ${end}`;
}

export function ModelEfficiencyCard({
  geo,
  strings,
  data,
  config,
  onConfigChange,
}: ModelEfficiencyCardProps) {
  const palette = useChartPalette(config.palette);
  const numberUnit = useNumberUnit();
  const view: EfficiencyView = config.efficiencyView ?? "model";

  const unpriced = data.composition.filter((row) => row.unknownCost).length;
  // 没有任何"已定价且有调用"的模型（会话视图：没有任何有价会话）就没有可画的气泡。
  const empty =
    view === "session"
      ? data.sessions.length === 0
      : data.composition.every((row) => row.unknownCost || row.calls <= 0);

  const option = useMemo(() => {
    // 气泡颜色与构成卡同源：同一模型在两张卡里同色。会话视图里部分未定价的
    // 模型可能不在构成表的着色路径上（unknownCost 行），落灰兜底。
    const colorByModel = new Map(
      data.composition.map((row, index) => [
        row.label,
        row.unknownCost
          ? palette.border
          : palette.series[index % palette.series.length],
      ]),
    );
    const points: EfficiencyPoint[] =
      view === "session"
        ? data.sessions.map((session) => ({
            name: session.model,
            value: [session.calls, session.costMicros / 1_000_000 / session.calls],
            tokens: session.tokens,
            costMicros: session.costMicros,
            itemStyle: {
              color: colorByModel.get(session.model) ?? palette.border,
            },
            session: {
              projectDir: session.projectDir,
              startMs: session.startMs,
              endMs: session.endMs,
            },
          }))
        : data.composition
            .filter((row) => !row.unknownCost && row.calls > 0)
            .map((row) => ({
              name: row.label,
              value: [row.calls, row.costMicros / 1_000_000 / row.calls],
              tokens: row.tokens,
              costMicros: row.costMicros,
              itemStyle: {
                color: colorByModel.get(row.label) ?? palette.series[0],
              },
            }));
    const maxTokens = Math.max(1, ...points.map((point) => point.tokens));

    return {
      grid: { left: 8, right: 24, top: 14, bottom: 12, containLabel: true },
      tooltip: {
        trigger: "item",
        // 挂到 body 上渲染：卡片 overflow 裁边会把 tooltip 上半截剪掉（见 optionBase 的同一注释）。
        appendToBody: true,
        // 十字准星穿过被悬浮的点画到两轴（虚线 + 轴端读数）：item 触发 + cross 指示器。
        axisPointer: {
          type: "cross",
          crossStyle: { color: palette.border },
          label: { backgroundColor: palette.border, color: palette.surface },
        },
        backgroundColor: palette.surface,
        borderColor: palette.border,
        textStyle: { color: palette.ink, fontSize: 11 },
        formatter: (params: EfficiencyTip) => {
          const point = params.data;
          const [calls, avg] = point?.value ?? [0, 0];

          // 会话视图：头部 = 项目，模型带色点成行，补时间范围——回答"这是哪次
          // 工作"；单次均价行不设（纵轴十字准星读数已覆盖）。
          if (point?.session !== undefined) {
            const { projectDir, startMs, endMs } = point.session;
            const dot =
              `<span style="display:inline-block;width:8px;height:8px;border-radius:9999px;` +
              `background-color:${point.itemStyle.color};margin-right:6px"></span>`;

            return [
              basename(projectDir) || strings.filterNoProject,
              `${dot}${point.name}`,
              `${strings.detailsColTime}　${formatSpan(startMs, endMs)}`,
              `${strings.metricCalls}　${formatCount(Number(calls))}`,
              `${strings.metricTokens}　${formatTokens(point.tokens, numberUnit)}`,
              `${strings.waterfallTotal}　${formatCostMicros(point.costMicros)}`,
            ].join("<br />");
          }

          return [
            point?.name ?? "",
            `${strings.metricCalls}　${formatCount(Number(calls))}`,
            `${strings.modelEfficiencyAvg}　${formatCostMicros(Number(avg) * 1_000_000)}`,
            `${strings.metricTokens}　${formatTokens(point?.tokens ?? 0, numberUnit)}`,
            `${strings.waterfallTotal}　${formatCostMicros(point?.costMicros ?? 0)}`,
          ].join("<br />");
        },
      },
      xAxis: {
        type: "value",
        name: strings.metricCalls,
        nameLocation: "middle",
        nameGap: 30,
        nameTextStyle: { color: palette.inkMuted, fontSize: 10, align: "left" },
        splitLine: { lineStyle: { color: palette.border } },
        axisLine: { show: false },
        axisLabel: { color: palette.inkMuted, fontSize: 9, formatter: (v: number) => formatCount(v) },
        axisPointer: { label: { formatter: ({ value }: { value: number }) => formatCount(value) } },
      },
      yAxis: {
        type: "value",
        name: strings.modelEfficiencyAvg,
        nameLocation: "middle",
        nameGap: 54,
        nameTextStyle: { color: palette.inkMuted, fontSize: 10 },
        splitLine: { lineStyle: { color: palette.border } },
        axisLine: { show: false },
        axisLabel: { color: palette.inkMuted, fontSize: 9, formatter: (v: number) => formatCostMicros(v * 1_000_000) },
        axisPointer: { label: { formatter: ({ value }: { value: number }) => formatCostMicros(value * 1_000_000) } },
      },
      series: [
        {
          name: strings.cardModelEfficiency,
          type: "scatter",
          data: points,
          // 气泡大小开根号缩放：面积才与 Token 量成正比，线性缩放会把小模型压成看不见。
          symbolSize: (_value: unknown, params: EfficiencyTip) => {
            const tokens = params.data?.tokens ?? 0;

            return BUBBLE_MIN_PX + (BUBBLE_MAX_PX - BUBBLE_MIN_PX) * Math.sqrt(tokens / maxTokens);
          },
          itemStyle: { opacity: 0.75 },
          ...barFocus(),
        },
      ],
    };
  }, [data, palette, numberUnit, strings, view]);

  const title = view === "session" ? strings.cardSessionSpend : strings.cardModelEfficiency;

  return (
    <DashboardCard
      id={geo.id}
      x={geo.x}
      y={geo.y}
      w={geo.w}
      h={geo.h}
      title={title}
      subtitle={strings.cardModelEfficiencySub}
      actions={
        <CardActions
          strings={strings}
          config={config}
          charts={[]}
          onConfigChange={onConfigChange}
          views={[
            { value: "model", label: strings.efficiencyViewModel },
            { value: "session", label: strings.efficiencyViewSession },
          ]}
          viewValue={view}
          onViewChange={(value) =>
            onConfigChange({ ...config, efficiencyView: value as EfficiencyView })
          }
        />
      }
    >
      <div className="flex h-full flex-col">
        {empty ? (
          <EmptyState kind="scatter" label={strings.cardEmpty} />
        ) : (
          <EChart option={option} label={title} className="tt-chart" />
        )}
        {unpriced === 0 ? null : (
          <p className="shrink-0 text-xs text-ink-muted">
            {strings.modelEfficiencyUnpriced.replace("{n}", String(unpriced))}
          </p>
        )}
      </div>
    </DashboardCard>
  );
}
