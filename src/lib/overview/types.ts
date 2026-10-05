/** 总览页的数据形状。字段语义对齐 `crates/toktol-core/src/storage` 的输出，mock 接进来时不用改结构。 */

export type TimeRange = "today" | "h24" | "d7" | "d30" | "all";

export type Metric = "cost" | "tokens" | "calls";

export type ChartKind = "bar" | "line" | "share" | "treemap" | "calendar";

/** 构成卡的拆分维度：模型 / 项目 / 工具。工具维度从流向链路的源头聚合而来。 */
export type BreakdownDimension = "model" | "project" | "tool";

export interface TrendBucket {
  readonly label: string;
  /** 本地时区对齐的桶起点（epoch ms）；label 由页面按粒度生成本地化文案。 */
  readonly startMs: number;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
  readonly cacheReadTokens: number;
  /** 缓存读取部分的费用；单价低于新生成 Token，不等于按 token 份额折算。 */
  readonly cacheReadCostMicros: number;
}

/** 构成类卡片的一行：模型、项目都用它，画法可以共用一套。 */
export interface BreakdownRow {
  readonly label: string;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
  readonly unknownCost: boolean;
}

/** 按模型拆分的趋势序列：数组与 trend 的桶一一对齐，百分比堆叠面积图的数据源。 */
export interface ModelTrendSeries {
  readonly model: string;
  readonly costMicros: readonly number[];
  readonly tokens: readonly number[];
  readonly calls: readonly number[];
}

export interface DashboardTotals {
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
  readonly sessions: number;
  readonly projects: number;
  readonly cacheReadTokens: number;
  /** 累计使用时长：各会话（首条 − 末条记录时间）之和；无会话的孤儿行不计。 */
  readonly activeDurationMs: number;
  /** 活跃工具个数：与总览页工具维度同口径——流向链路里只当源头的节点。 */
  readonly activeTools: number;
}

/** 桑基图的一条链路：工具 → 模型 → 项目。节点名由卡片从链路里取，不另存一份。 */
export interface FlowLink {
  readonly source: string;
  readonly target: string;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
}

/** 活跃热力图的一格：星期（0=周一）× 小时。 */
export interface ActivityCell {
  readonly day: number;
  readonly hour: number;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
}

/** 日历热力图的一天。 */
export interface DailyPoint {
  readonly date: string;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
}

/** 箱线图的一组五数概括：min / Q1 / 中位数 / Q3 / max。 */
export interface ModelSpread {
  readonly label: string;
  readonly costMicros: readonly number[];
  readonly tokens: readonly number[];
}

/** 会话消耗气泡：一会话（× 模型）一点；全部行未定价的会话不在切片里。 */
export interface SessionPoint {
  /** 会话所属工具；与 externalId 一起构成会话自然键。 */
  readonly tool: string;
  /** 会话在工具数据库里的原始 id（与会话页同一身份）。 */
  readonly externalId: string;
  /** 会话的项目目录；无项目为 ""（展示层换成哨兵文案）。 */
  readonly projectDir: string;
  /** 模型标准名；一会话跨多模型时按模型拆点。 */
  readonly model: string;
  readonly calls: number;
  readonly tokens: number;
  /** 该会话（该模型）的已知成本合计（微美元）。 */
  readonly costMicros: number;
  /** 该会话首条记录的时间（epoch ms）。 */
  readonly startMs: number;
  /** 该会话末条记录的时间（epoch ms）。 */
  readonly endMs: number;
}

/**
 * 费用构成瀑布的分项，与 UsageTotals 的量桶一一对应；分项之和 = 已知总费用
 * （knownCostMicros），未定价模型的费用不在其中，由 unknownModelCount 表达。
 */
export interface CostComposition {
  readonly inputCostMicros: number;
  readonly outputCostMicros: number;
  /** 推理量按输出价计费，但从输出桶里拆出来单列，瀑布上才看得见它占多少。 */
  readonly reasoningCostMicros: number;
  readonly cacheReadCostMicros: number;
  readonly cacheWriteCostMicros: number;
}

export interface DashboardData {
  readonly totals: DashboardTotals;
  readonly trend: readonly TrendBucket[];
  readonly modelTrend: readonly ModelTrendSeries[];
  readonly composition: readonly BreakdownRow[];
  readonly projects: readonly BreakdownRow[];
  readonly flow: readonly FlowLink[];
  readonly activity: readonly ActivityCell[];
  readonly daily: readonly DailyPoint[];
  readonly spread: readonly ModelSpread[];
  /** 会话消耗气泡切片，按已知成本降序。 */
  readonly sessions: readonly SessionPoint[];
  readonly costComposition: CostComposition;
  readonly unknownModelCount: number;
}

export function metricOf(bucket: TrendBucket, metric: Metric): number {
  if (metric === "cost") return bucket.costMicros / 1_000_000;
  if (metric === "tokens") return bucket.tokens;

  return bucket.calls;
}
