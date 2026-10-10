// 总览页：数据走 IPC 聚合（fetchOverview），mock 只留给 harness 回退与测试。
// 筛选语义与明细页一致：projects 里的 "" 是"无项目"哨兵，展示层换成文案。

import { useEffect, useMemo, useState } from "react";

import { DashboardGrid } from "./dashboard/DashboardGrid";
import {
  fetchOverview,
  isTauriRuntime,
  type DashboardPayload,
  type TrendGrain,
  type UsageFilters,
} from "../lib/api";
import { Button } from "../components/ui/Button";
import { presetRange, type TimeSelection } from "../components/data/DateRangePicker";
import {
  FilterToolbar,
  type FilterDimension,
} from "../components/data/FilterToolbar";
import { type MultiSelectOption } from "../components/data/MultiSelect";
import type { Strings } from "../i18n/strings";
import { useInvokeQuery } from "../lib/hooks/useInvokeQuery";
import { formatCount } from "../lib/format";
import { dropDisabled, filterDimensions, timePresetsOf, weekdaysOf } from "../lib/filters";
import { toolLabel } from "../lib/tools";
import {
  DEFAULT_CARD_CONFIG,
  DEFAULT_WIDGETS,
  defaultWidgetFor,
  readLayout,
  withDefaults,
  writeLayout,
  type CardConfig,
  type LayoutDoc,
  type LayoutWidget,
} from "../lib/overview/layout";
import { buildMockDashboard } from "../lib/overview/mock";
import {
  type BreakdownDimension,
  type BreakdownRow,
  type DashboardData,
  type FlowLink,
  type TimeRange,
} from "../lib/overview/types";
import { BreakdownCard } from "./dashboard/BreakdownCard";
import { CalendarCard } from "./dashboard/CalendarCard";
import { ComboCard } from "./dashboard/ComboCard";
import { CostWaterfallCard } from "./dashboard/CostWaterfallCard";
import { ModelEfficiencyCard } from "./dashboard/ModelEfficiencyCard";
import { SankeyCard } from "./dashboard/SankeyCard";
import { SpreadCard } from "./dashboard/SpreadCard";
import { TotalsCard } from "./dashboard/TotalsCard";
import { UsageTrendCard } from "./dashboard/UsageTrendCard";

const COMPOSITION_CHARTS = ["share", "bar", "treemap"] as const;

const DAY_MS = 86_400_000;

// mock 回退（纯浏览器 harness 没有壳能力）时的范围映射：自定义区间按跨度就近取一个种子。
function mockRangeOf(time: TimeSelection): TimeRange {
  if (time.preset === "custom" && time.range !== null) {
    const days = Math.round((time.range.end - time.range.start) / DAY_MS) + 1;
    if (days <= 1) return "today";
    if (days <= 7) return "d7";
    if (days <= 31) return "d30";

    return "all";
  }

  if (time.preset === "today") return "today";
  if (time.preset === "24h") return "h24";
  if (time.preset === "7d") return "d7";
  if (time.preset === "30d") return "d30";

  return "all";
}

/**
 * 载荷 → 卡片数据：趋势 label 按 grain 本地化，其余字段原样。
 * label 不在 Rust 侧生成——本地化文案是前端的事。
 */
function hydrate(payload: DashboardPayload): DashboardData {
  const labelOf = labelFormatter(payload.trendGrain);

  return {
    totals: payload.totals,
    trend: payload.trend.map((bucket) => ({
      ...bucket,
      label: labelOf(bucket.startMs),
    })),
    modelTrend: payload.modelTrend,
    composition: payload.composition,
    projects: payload.projects,
    flow: payload.flow,
    activity: payload.activity,
    daily: payload.daily,
    spread: payload.spread,
    sessions: payload.sessions,
    costComposition: payload.costComposition,
    unknownModelCount: payload.unknownModelCount,
  };
}

/** 小时桶的 label 手写（Intl 的小时带上午下午太长）；天/月按运行语言格式化。 */
function labelFormatter(grain: TrendGrain): (ms: number) => string {
  if (grain === "hour") {
    return (ms) => {
      const date = new Date(ms);
      return `${String(date.getHours()).padStart(2, "0")}:00`;
    };
  }
  const fmt = new Intl.DateTimeFormat(
    undefined,
    grain === "day"
      ? { month: "numeric", day: "numeric" }
      : { year: "numeric", month: "numeric" },
  );

  return (ms) => fmt.format(ms);
}

/** 顶部筛选 → IPC 查询。time.range 在切预设/自定义时总是同步成生效区间。 */
function filtersOf(
  time: TimeSelection,
  toolSel: ReadonlySet<string>,
  modelSel: ReadonlySet<string>,
  projectSel: ReadonlySet<string>,
  query: string,
): UsageFilters {
  const search = query.trim();

  return {
    timeStart: time.range?.start ?? null,
    timeEnd: time.range?.end ?? null,
    tools: [...toolSel],
    models: [...modelSel],
    projects: [...projectSel],
    search: search === "" ? null : search,
  };
}

/** 桑基图里以工具名为起点的链路按工具汇总：用它当下拉的调用计数。 */
function countsFromFlow(
  flow: readonly FlowLink[],
  tools: readonly string[],
): Map<string, number> {
  const counts = new Map<string, number>();

  for (const link of flow) {
    if (!tools.includes(link.source)) continue;
    counts.set(link.source, (counts.get(link.source) ?? 0) + link.calls);
  }

  return counts;
}

/** 构成表的一行就是一个可选项：条数取它的调用数，多的排前面。 */
function rowsToOptions(
  rows: readonly BreakdownRow[],
  noProjectLabel: string | undefined,
): MultiSelectOption[] {
  return rows
    .map((row) => ({
      value: row.label,
      label: row.label === "" ? (noProjectLabel ?? row.label) : row.label,
      count: row.calls,
    }))
    .sort((a, b) => b.count - a.count);
}

interface OverviewPageProps {
  readonly strings: Strings;
  /** 被禁用的工具 id：筛选条里不出现，数据也不参与聚合。 */
  readonly disabledTools: string[];
  /** 扫描带来新数据时递增：变化即重载仪表盘。 */
  readonly scanVersion: number;
  /** 测试缝隙：注入首帧仪表盘（当前筛选 + 未筛选基线），跳过 IPC 拉取。 */
  readonly initialDashboard?: [DashboardData, DashboardData];
}

export function OverviewPage({
  strings,
  disabledTools,
  scanVersion,
  initialDashboard,
}: OverviewPageProps) {
  const [doc, setDoc] = useState<LayoutDoc>(() =>
    withDefaults(DEFAULT_WIDGETS, readLayout()),
  );
  const [time, setTime] = useState<TimeSelection>(() => ({
    preset: "today",
    range: presetRange("today", Date.now()),
  }));
  const [toolSel, setToolSel] = useState<ReadonlySet<string>>(new Set());
  const [modelSel, setModelSel] = useState<ReadonlySet<string>>(new Set());
  const [projectSel, setProjectSel] = useState<ReadonlySet<string>>(new Set());
  const [query, setQuery] = useState("");
  // 搜索词防抖：本地聚合虽快，逐键发 IPC 也没必要。
  const [debouncedQuery, setDebouncedQuery] = useState("");

  useEffect(() => {
    const timer = setTimeout(() => setDebouncedQuery(query), 250);
    return () => clearTimeout(timer);
  }, [query]);

  // 顶部筛选 → IPC 查询。time.range 在切预设/自定义时总是同步成生效区间。
  const filters = useMemo(
    () => filtersOf(time, toolSel, modelSel, projectSel, debouncedQuery),
    [time, toolSel, modelSel, projectSel, debouncedQuery],
  );

  // 仪表盘装载：主查询与"未筛选基线"一并取回；scanVersion 只在扫描真正
  // 带来新记录/新会话时递增（与明细页同一信号），停在前台时扫到新数据
  // 仪表盘跟着自动更新。纯浏览器（harness）没有壳能力：回退 mock，布局
  // 调试不受影响。
  const dashboardQuery = useInvokeQuery({
    deps: [filters, disabledTools, scanVersion],
    ...(initialDashboard === undefined ? {} : { initialData: initialDashboard }),
    fetch: () => {
      // 选项与计数取自未筛选的数据（与明细页一致的静态全量口径），不随当前选择跳动。
      const baseFilters: UsageFilters = {
        timeStart: filters.timeStart,
        timeEnd: filters.timeEnd,
        tools: [],
        models: [],
        projects: [],
        search: null,
      };
      const request: Promise<[DashboardData, DashboardData]> = isTauriRuntime()
        ? Promise.all([
            fetchOverview(filters, disabledTools),
            fetchOverview(baseFilters, disabledTools),
          ]).then(([payload, basePayload]) => [hydrate(payload), hydrate(basePayload)])
        : Promise.all([
            buildMockDashboard(mockRangeOf(time), {
              tools: toolSel,
              models: modelSel,
              projects: projectSel,
              query: debouncedQuery,
            }),
            buildMockDashboard(mockRangeOf(time)),
          ]);
      return request;
    },
  });
  const loadError = dashboardQuery.error;
  const loaded = dashboardQuery.data;
  const data = loaded?.[0] ?? null;
  const base = loaded?.[1] ?? null;
  // 错误重试按钮的回调（hook 的 reload 即重拉当前键）。
  const load = dashboardQuery.reload;

  useEffect(() => {
    writeLayout(doc);
  }, [doc]);

  const timePresets = useMemo(() => timePresetsOf(strings), [strings]);
  const weekdays = useMemo(() => weekdaysOf(strings), [strings]);

  // 工具清单与计数都从流向派生：链路（工具 → 模型 → 项目）里只当源头的节点是工具。
  const toolNames = useMemo(() => {
    if (base === null) return [];
    const targets = new Set(base.flow.map((link) => link.target));
    const sources = new Set(
      base.flow
        .filter((link) => !targets.has(link.source))
        .map((link) => link.source),
    );

    return [...sources].sort((a, b) => a.localeCompare(b));
  }, [base]);
  const toolCounts = useMemo(
    () => countsFromFlow(base?.flow ?? [], toolNames),
    [base, toolNames],
  );
  const toolOptions = useMemo(
    () =>
      toolNames
        .filter((tool) => !disabledTools.includes(tool))
        .map((tool) => ({
          value: tool,
          // 筛选值仍是工具 id，显示用友好名（与明细页筛选一致）。
          label: toolLabel(tool),
          count: toolCounts.get(tool) ?? 0,
        })),
    [disabledTools, toolNames, toolCounts],
  );
  const modelOptions = useMemo(
    () => rowsToOptions(base?.composition ?? [], undefined),
    [base],
  );
  const projectOptions = useMemo(
    () => rowsToOptions(base?.projects ?? [], strings.filterNoProject),
    [base, strings],
  );

  function resetFilters(): void {
    setTime({ preset: "all", range: null });
    setToolSel(new Set());
    setModelSel(new Set());
    setProjectSel(new Set());
    setQuery("");
  }

  function widgetFor(id: string): LayoutWidget {
    // doc.widgets 由 withDefaults(DEFAULT_WIDGETS, …) 保证含全部默认卡片；
    // 缺失只能是代码 bug，defaultWidgetFor 直接抛错而不是返回 undefined。
    return doc.widgets.find((widget) => widget.id === id) ?? defaultWidgetFor(id);
  }

  function configFor(id: string): CardConfig {
    return doc.cards[id] ?? DEFAULT_CARD_CONFIG;
  }

  function setConfig(id: string, config: CardConfig) {
    setDoc((current) => ({
      ...current,
      cards: { ...current.cards, [id]: config },
    }));
  }

  // "" 是无项目哨兵：卡片展示层换成文案；筛选选项的 value 仍传 ""。
  const cardData = useMemo<DashboardData | null>(() => {
    if (data === null) return null;
    const displayLabel = (label: string): string =>
      label === "" ? strings.filterNoProject : label;

    return {
      ...data,
      projects: data.projects.map((row) => ({
        ...row,
        label: displayLabel(row.label),
      })),
      flow: data.flow.map((link) => ({
        ...link,
        source: displayLabel(link.source),
        target: displayLabel(link.target),
      })),
    };
  }, [data, strings]);

  // 工具维度没有现成的构成行，从流向聚合：按源头归并即各工具的用量。
  const toolRows = useMemo<BreakdownRow[]>(() => {
    if (cardData === null) return [];
    const targets = new Set(cardData.flow.map((link) => link.target));
    const sums = new Map<
      string,
      { costMicros: number; tokens: number; calls: number }
    >();

    for (const link of cardData.flow) {
      if (targets.has(link.source)) continue;

      const sum = sums.get(link.source) ?? {
        costMicros: 0,
        tokens: 0,
        calls: 0,
      };
      sum.costMicros += link.costMicros;
      sum.tokens += link.tokens;
      sum.calls += link.calls;
      sums.set(link.source, sum);
    }

    return [...sums.entries()].map(([label, sum]) => ({
      label,
      ...sum,
      unknownCost: false,
    }));
  }, [cardData]);

  const dimensions = useMemo<FilterDimension[]>(
    () =>
      filterDimensions({
        labels: {
          tool: strings.filterTool,
          model: strings.filterModel,
          project: strings.filterProject,
        },
        options: { tool: toolOptions, model: modelOptions, project: projectOptions },
        selection: { tool: toolSel, model: modelSel, project: projectSel },
        setters: { tool: setToolSel, model: setModelSel, project: setProjectSel },
      }),
    [
      strings,
      toolOptions,
      modelOptions,
      projectOptions,
      toolSel,
      modelSel,
      projectSel,
    ],
  );

  // 被禁用的工具要从选择里摘掉：选项已不可见，留着等于永远少一块、解释不清的数据。
  useEffect(() => {
    setToolSel((current) => dropDisabled(current, disabledTools));
  }, [disabledTools]);

  // 加载 / 错误态放在所有 hooks 之后：条件早退不能改变 hooks 的调用数量。
  if (loadError || cardData === null || base === null) {
    return (
      <div className="flex min-h-[40vh] flex-col items-center justify-center gap-3 text-sm text-ink-muted">
        <p>{loadError ? strings.overviewLoadError : strings.overviewLoading}</p>
        {loadError && (
          <Button variant="ghost" onClick={load}>
            {strings.retryLabel}
          </Button>
        )}
      </div>
    );
  }

  // key 必须稳定：拖拽结束后 doc.widgets 更新，若把几何编进 key，React 会重挂卡片，
  // 新节点 gridstack 不认识，布局信息全丢（卡片塌成 0 高）。
  const usage = widgetFor("usage");
  const totals = widgetFor("totals");
  const composition = widgetFor("composition");
  const flow = widgetFor("flow");
  const calendar = widgetFor("calendar");
  const spread = widgetFor("spread");
  const combo = widgetFor("combo");
  const modelEfficiency = widgetFor("modelEfficiency");
  const costWaterfall = widgetFor("costWaterfall");

  // 构成卡：模型 / 项目 / 工具三个维度共用一张卡，行、副标题与"未知"注记随维度切换。
  const compositionDimension: BreakdownDimension =
    configFor("composition").dimension ?? "model";
  const dimensionOptions: readonly {
    value: BreakdownDimension;
    label: string;
  }[] = [
    { value: "tool", label: strings.filterTool },
    { value: "project", label: strings.filterProject },
    { value: "model", label: strings.filterModel },
  ];
  const compositionRows =
    compositionDimension === "model"
      ? cardData.composition
      : compositionDimension === "project"
        ? cardData.projects
        : toolRows;
  const compositionSub =
    compositionDimension === "model"
      ? strings.cardCompositionSub
      : compositionDimension === "project"
        ? strings.cardProjectsSub
        : strings.cardCompositionSubTool;
  // "未知"是模型维度的概念（未定价模型），别的维度没有这个口径。
  const compositionNote =
    compositionDimension === "model" && cardData.unknownModelCount > 0
      ? strings.unknownModelsNote.replace(
          "{n}",
          String(cardData.unknownModelCount),
        )
      : undefined;

  return (
    <div>
      <div className="mb-4">
        <FilterToolbar
          time={time}
          onTimeChange={setTime}
          timePresets={timePresets}
          allTimeLabel={strings.rangeAll}
          customLabel={strings.filterCustomRange}
          startLabel={strings.filterStart}
          endLabel={strings.filterEnd}
          weekdays={weekdays}
          dimensions={dimensions}
          query={query}
          onQueryChange={setQuery}
          searchHint={strings.filterSearchHint}
          searchPlaceholder={strings.filterSearchOptions}
          clearLabel={strings.filterClear}
          noMatchLabel={strings.filterNoMatch}
          totalLabel={strings.paginationTotal.replace(
            "{total}",
            formatCount(cardData.totals.calls),
          )}
          resetLabel={strings.resetFilters}
          onReset={resetFilters}
        />
      </div>
      <DashboardGrid
        onWidgetsChange={(widgets) =>
          setDoc((current) => ({ ...current, widgets }))
        }
      >
          <UsageTrendCard
            key={usage.id}
            geo={usage}
            strings={strings}
            data={cardData}
            config={configFor("usage")}
            onConfigChange={(config) => setConfig("usage", config)}
          />
          <TotalsCard
            key={totals.id}
            geo={totals}
            strings={strings}
            data={cardData}
          />
          <BreakdownCard
            key={composition.id}
            geo={composition}
            strings={strings}
            title={strings.cardComposition}
            subtitle={compositionSub}
            rows={compositionRows}
            charts={COMPOSITION_CHARTS}
            note={compositionNote}
            dimensions={dimensionOptions}
            config={configFor("composition")}
            onConfigChange={(config) => setConfig("composition", config)}
          />
          <SankeyCard
            key={flow.id}
            geo={flow}
            strings={strings}
            data={cardData}
            config={configFor("flow")}
            onConfigChange={(config) => setConfig("flow", config)}
          />
          <CalendarCard
            key={calendar.id}
            geo={calendar}
            strings={strings}
            data={cardData}
            config={configFor("calendar")}
            onConfigChange={(config) => setConfig("calendar", config)}
          />
          <SpreadCard
            key={spread.id}
            geo={spread}
            strings={strings}
            data={cardData}
            config={configFor("spread")}
            onConfigChange={(config) => setConfig("spread", config)}
          />
          <ComboCard
            key={combo.id}
            geo={combo}
            strings={strings}
            data={cardData}
            config={configFor("combo")}
            onConfigChange={(config) => setConfig("combo", config)}
          />
          <ModelEfficiencyCard
            key={modelEfficiency.id}
            geo={modelEfficiency}
            strings={strings}
            data={cardData}
            config={configFor("modelEfficiency")}
            onConfigChange={(config) => setConfig("modelEfficiency", config)}
          />
          <CostWaterfallCard
            key={costWaterfall.id}
            geo={costWaterfall}
            strings={strings}
            data={cardData}
          />
        </DashboardGrid>
    </div>
  );
}
