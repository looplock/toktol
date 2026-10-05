/**
 * mock 数据。用定种子伪随机而不是 Math.random：同一个范围每次渲染必须出同一组数，
 * 否则改个筛选就让整页图表跳一次，看不出是布局错还是数据错。
 */

import {
  type ActivityCell,
  type BreakdownRow,
  type DashboardData,
  type DailyPoint,
  type FlowLink,
  type ModelSpread,
  type ModelTrendSeries,
  type SessionPoint,
  type TrendBucket,
  type TimeRange,
} from "./types";

function seeded(seed: number): () => number {
  let state = seed >>> 0;

  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let x = Math.imul(state ^ (state >>> 15), 1 | state);
    x ^= x + Math.imul(x ^ (x >>> 7), 61 | x);

    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

const HOUR_LABELS = Array.from(
  { length: 24 },
  (_, hour) => `${String(hour).padStart(2, "0")}:00`,
);

const MONTH_LABELS = [
  "1 月",
  "2 月",
  "3 月",
  "4 月",
  "5 月",
  "6 月",
  "7 月",
  "8 月",
  "9 月",
  "10 月",
  "11 月",
  "12 月",
];

const SEEDS: Record<TimeRange, number> = {
  today: 20260926,
  h24: 20260925,
  d7: 7,
  d30: 30,
  all: 12,
};

function dayLabels(count: number): string[] {
  return Array.from({ length: count }, (_, index) => `${index + 1} 日`);
}

function bucketsFor(range: TimeRange, random: () => number): TrendBucket[] {
  if (range === "today" || range === "h24") {
    // mock 简化：两种都铺当天 0 点起的 24 个小时桶。
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);

    return HOUR_LABELS.map((label, hour) =>
      bucketFrom(
        label,
        midnight.getTime() + hour * 3_600_000,
        random,
        workdayWeight(hour),
      ),
    );
  }

  if (range === "all") {
    // 月度有淡旺季：让 12 根柱子不是一条平线。桶起点 = 最近 12 个月的月初。
    const now = new Date();

    return MONTH_LABELS.map((label, index) => {
      const start = new Date(
        now.getFullYear(),
        now.getMonth() - (MONTH_LABELS.length - 1 - index),
        1,
      );

      return bucketFrom(label, start.getTime(), random, 0.7 + random() * 0.6);
    });
  }

  const count = range === "d7" ? 7 : 30;
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);

  return dayLabels(count).map((label, index) => {
    const start = new Date(midnight);
    start.setDate(midnight.getDate() - (count - 1 - index));
    // 周末回落 + 偶发尖峰 + 偶发近乎空白的低调日：日历和趋势要能对上"哪天不对劲"。
    const weekend = (index + 3) % 7 === 0 || (index + 4) % 7 === 0 ? 0.4 : 1;
    const spike = random() < 0.08 ? 1.9 : 1;
    const quiet = random() < 0.05 ? 0.08 : 1;

    return bucketFrom(
      label,
      start.getTime(),
      random,
      weekend * spike * quiet,
    );
  });
}

// 与 activityFor 共用：白天双峰 + 午饭回落 + 深夜底噪，两张图讲的是同一套作息。
function workdayWeight(hour: number): number {
  if (hour < 7 || hour > 23) return 0.12;
  if (hour <= 9) return 0.6;
  if (hour <= 12) return 1.15;
  if (hour === 13) return 0.75;
  if (hour <= 18) return 1.1;
  if (hour <= 22) return 0.55;

  return 0.25;
}

function bucketFrom(
  label: string,
  startMs: number,
  random: () => number,
  weight: number,
): TrendBucket {
  // 幅度抖动：真实用量逐桶差异是好几个数量级的，不是围绕均值的小噪声。
  const jitter = 0.25 + random() ** 1.5 * 2.2;
  const calls = Math.round((60 + random() * 340) * weight * jitter);
  const tokens = Math.round((12_000 + random() * 90_000) * weight * jitter);
  const costMicros = Math.round((180_000 + random() * 1_400_000) * weight * jitter);
  // 缓存读取单价按新生成的 1/10 估：mock 只求量级对，接 IPC 后换真实 cache_read_cost_micros。
  const cacheReadTokens = Math.round(tokens * (0.42 + random() * 0.3));
  const freshTokens = Math.max(tokens - cacheReadTokens, 0);
  const cacheWeight = cacheReadTokens * 0.1;

  return {
    label,
    startMs,
    calls,
    tokens,
    costMicros,
    cacheReadTokens,
    cacheReadCostMicros: Math.round(
      (costMicros * cacheWeight) / (cacheWeight + freshTokens),
    ),
  };
}

// costPerCall / tokensPerCall 是箱线图的基准：单次调用的量级，五数概括围绕它生成。
// 名单拉长是为了让图例翻页、构成表滚动这些"模型很多"的场景在 mock 阶段就可见。
const MOCK_MODELS: readonly {
  readonly model: string;
  readonly weight: number;
  readonly costPerCall: number;
  readonly tokensPerCall: number;
}[] = [
  {
    model: "claude-opus-4-6",
    weight: 0.205,
    costPerCall: 320_000,
    tokensPerCall: 4_200,
  },
  {
    model: "claude-sonnet-4-6",
    weight: 0.17,
    costPerCall: 120_000,
    tokensPerCall: 3_100,
  },
  {
    model: "gpt-5-codex",
    weight: 0.1,
    costPerCall: 90_000,
    tokensPerCall: 2_600,
  },
  {
    model: "gemini-3-pro",
    weight: 0.07,
    costPerCall: 85_000,
    tokensPerCall: 3_400,
  },
  {
    model: "claude-haiku-4-5",
    weight: 0.06,
    costPerCall: 28_000,
    tokensPerCall: 1_400,
  },
  {
    model: "grok-4",
    weight: 0.05,
    costPerCall: 75_000,
    tokensPerCall: 2_900,
  },
  {
    model: "glm-5",
    weight: 0.045,
    costPerCall: 18_000,
    tokensPerCall: 2_400,
  },
  {
    model: "kimi-k3",
    weight: 0.04,
    costPerCall: 14_000,
    tokensPerCall: 2_200,
  },
  {
    model: "qwen3-coder-plus",
    weight: 0.035,
    costPerCall: 22_000,
    tokensPerCall: 2_600,
  },
  {
    model: "deepseek-v4",
    weight: 0.03,
    costPerCall: 9_000,
    tokensPerCall: 2_200,
  },
  {
    model: "minimax-m2",
    weight: 0.025,
    costPerCall: 11_000,
    tokensPerCall: 2_000,
  },
  {
    model: "mistral-large-3",
    weight: 0.025,
    costPerCall: 16_000,
    tokensPerCall: 2_500,
  },
  {
    model: "doubao-seed-2",
    weight: 0.02,
    costPerCall: 12_000,
    tokensPerCall: 2_100,
  },
  {
    model: "phi-5-mini",
    weight: 0.02,
    costPerCall: 6_000,
    tokensPerCall: 1_800,
  },
  {
    model: "llama-4-scout",
    weight: 0.02,
    costPerCall: 8_000,
    tokensPerCall: 2_000,
  },
  {
    model: "command-a-2",
    weight: 0.015,
    costPerCall: 10_000,
    tokensPerCall: 1_900,
  },
  {
    model: "ernie-5",
    weight: 0.015,
    costPerCall: 9_500,
    tokensPerCall: 1_800,
  },
  {
    model: "hunyuan-turbo-s",
    weight: 0.015,
    costPerCall: 8_500,
    tokensPerCall: 1_700,
  },
];

const MOCK_PROJECTS: readonly {
  readonly project: string;
  readonly weight: number;
}[] = [
  { project: "toktol", weight: 0.28 },
  { project: "website", weight: 0.18 },
  { project: "internal-tools", weight: 0.14 },
  { project: "data-pipeline", weight: 0.12 },
  { project: "docs-site", weight: 0.1 },
  { project: "mobile-app", weight: 0.09 },
  { project: "ml-experiments", weight: 0.09 },
];

const MOCK_TOOLS: readonly {
  readonly tool: string;
  readonly weight: number;
}[] = [
  { tool: "Claude Code", weight: 0.4 },
  { tool: "Codex CLI", weight: 0.24 },
  { tool: "WorkBuddy", weight: 0.18 },
  { tool: "OpenCode", weight: 0.1 },
  { tool: "Grok CLI", weight: 0.08 },
];

/** 未定价模型在流向里照常出现，但费用恒为 0——桑基图也得能表达"这条流没有价格"。 */
const UNPRICED_WEIGHT = 0.04;

/** 未定价模型：占位用，界面必须把它显示成"未知"。 */
const UNPRICED_MODEL = "local/fine-tuned";

/** 含未定价模型的模型清单：构成表与筛选选项共用，未定价模型也能被筛。 */
const ALL_MOCK_MODELS: readonly {
  readonly label: string;
  readonly weight: number;
}[] = [
  ...MOCK_MODELS.map(({ model, weight }) => ({ label: model, weight })),
  { label: UNPRICED_MODEL, weight: UNPRICED_WEIGHT },
];

export const MODEL_OPTIONS: readonly string[] = ALL_MOCK_MODELS.map(
  (entry) => entry.label,
);

/** 筛选口径：三个维度都是"空集 = 不限"，query 按名字子串再收一层。 */
export interface MockFilters {
  readonly tools: ReadonlySet<string>;
  readonly models: ReadonlySet<string>;
  readonly projects: ReadonlySet<string>;
  readonly query: string;
}

const NO_FILTERS: MockFilters = {
  tools: new Set(),
  models: new Set(),
  projects: new Set(),
  query: "",
};

/** 一条"工具 × 模型 × 项目"组合及其份额；份额之和就是这一屏数据占全量的比例。 */
interface Combo {
  readonly tool: string;
  readonly model: string;
  readonly project: string;
  readonly share: number;
}

/**
 * 筛完剩下的组合。搜索按"任一维度命中即保留"（与明细页搜索会话/项目同义），
 * 否则搜一个项目名会把工具、模型维度一起清空，图直接归零。
 */
function combosFor(filter: MockFilters): Combo[] {
  const needle = filter.query.trim().toLowerCase();
  const out: Combo[] = [];

  for (const tool of MOCK_TOOLS) {
    if (filter.tools.size > 0 && !filter.tools.has(tool.tool)) continue;

    for (const entry of ALL_MOCK_MODELS) {
      if (filter.models.size > 0 && !filter.models.has(entry.label)) continue;

      for (const project of MOCK_PROJECTS) {
        if (filter.projects.size > 0 && !filter.projects.has(project.project))
          continue;

        const names = [tool.tool, entry.label, project.project];
        if (
          needle !== "" &&
          !names.some((name) => name.toLowerCase().includes(needle))
        )
          continue;

        out.push({
          tool: tool.tool,
          model: entry.label,
          project: project.project,
          share: tool.weight * entry.weight * project.weight,
        });
      }
    }
  }

  return out;
}

/** 按维度汇总份额：Σ 各维度 = 总比例，构成表据此分摊。 */
function shareBy(
  combos: readonly Combo[],
  pick: (combo: Combo) => string,
): Map<string, number> {
  const sums = new Map<string, number>();

  for (const combo of combos) {
    const key = pick(combo);
    sums.set(key, (sums.get(key) ?? 0) + combo.share);
  }

  return sums;
}

interface Amounts {
  costMicros: number;
  tokens: number;
  calls: number;
}

// 三层吞吐量必须守恒：模型节点的进（工具 × 模型）与出（模型 × 项目）都来自同一组
// 工具×模型×项目份额，所以选了任一层筛选，图上的带宽仍然读得通。
function flowFor(totals: Amounts, combos: readonly Combo[]): FlowLink[] {
  const links = new Map<string, { source: string; target: string } & Amounts>();

  function add(
    source: string,
    target: string,
    share: number,
    priced: boolean,
  ): void {
    const key = `${source} -> ${target}`;
    const link = links.get(key) ?? {
      source,
      target,
      costMicros: 0,
      tokens: 0,
      calls: 0,
    };

    link.tokens += Math.round(totals.tokens * share);
    link.calls += Math.round(totals.calls * share);
    if (priced) link.costMicros += Math.round(totals.costMicros * share);
    links.set(key, link);
  }

  for (const combo of combos) {
    const priced = combo.model !== UNPRICED_MODEL;

    add(combo.tool, combo.model, combo.share, priced);
    add(combo.model, combo.project, combo.share, priced);
  }

  return [...links.values()].map(
    ({ source, target, costMicros, tokens, calls }) => ({
      source,
      target,
      costMicros,
      tokens,
      calls,
    }),
  );
}

const DAYS_PER_RANGE: Record<TimeRange, number> = {
  today: 1,
  h24: 1,
  d7: 7,
  d30: 30,
  all: 365,
};

function scale(totals: Amounts, share: number): Amounts {
  return {
    costMicros: Math.round(totals.costMicros * share),
    tokens: Math.round(totals.tokens * share),
    calls: Math.round(totals.calls * share),
  };
}

// 与趋势共用同一套作息权重（workdayWeight），这样两张图讲的是同一件事。
function activityFor(totals: Amounts, random: () => number): ActivityCell[] {
  const cells: { day: number; hour: number; weight: number }[] = [];
  let sum = 0;

  for (let day = 0; day < 7; day += 1) {
    for (let hour = 0; hour < 24; hour += 1) {
      const weight =
        workdayWeight(hour) * (day >= 5 ? 0.45 : 1) * (0.55 + random() * 0.9);

      sum += weight;
      cells.push({ day, hour, weight });
    }
  }

  return cells.map(({ day, hour, weight }) => ({
    day,
    hour,
    ...scale(totals, weight / sum),
  }));
}

function dailyFor(
  range: TimeRange,
  totals: Amounts,
  random: () => number,
): DailyPoint[] {
  const count = DAYS_PER_RANGE[range];
  const last = new Date();
  const weights: number[] = [];
  let sum = 0;

  for (let offset = count - 1; offset >= 0; offset -= 1) {
    const date = new Date(last);
    date.setDate(last.getDate() - offset);
    const weekend = date.getDay() === 0 || date.getDay() === 6 ? 0.5 : 1;
    // 偶尔来一个尖峰：日历图的意义就是让你一眼看到"哪天不对劲"。
    const spike = random() < 0.07 ? 2.4 : 1;
    const weight = weekend * spike * (0.6 + random() * 0.8);

    sum += weight;
    weights.push(weight);
  }

  return weights.map((weight, index) => {
    const date = new Date(last);
    date.setDate(last.getDate() - (count - 1 - index));

    return { date: isoDate(date), ...scale(totals, weight / sum) };
  });
}

function isoDate(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");

  return `${date.getFullYear()}-${month}-${day}`;
}

// 只统计有价格的模型：未定价的"每调用开销"画不出来，硬画成 0 就是在替它估价。
function spreadFor(random: () => number): ModelSpread[] {
  const shape = [0.25, 0.7, 1, 1.55, 2.9];

  return MOCK_MODELS.map(({ model, costPerCall, tokensPerCall }) => {
    const jitter = 0.9 + random() * 0.2;

    return {
      label: model,
      costMicros: shape.map((factor) =>
        Math.round(costPerCall * factor * jitter),
      ),
      tokens: shape.map((factor) =>
        Math.round(tokensPerCall * factor * jitter),
      ),
    };
  });
}

/** 会话消耗气泡：一会话（× 模型）一点。量级跟单会话的真实形状对齐——
 * 调用次数长尾（多数会话十几次以内、少数上百），单价围绕模型基准抖动。
 * 时间跨度围绕调用次数展开（每次 1~3 分钟），与"使用时间"的口径对齐。 */
function sessionsFor(
  random: () => number,
  modelShares: Map<string, number>,
  baseMs: number,
): SessionPoint[] {
  const models = MOCK_MODELS.filter(({ model }) => modelShares.has(model));
  const tools = ["Claude Code", "Codex CLI", "WorkBuddy", "OpenCode"];
  const count = 40 + Math.floor(random() * 20);
  const points: SessionPoint[] = [];

  for (let index = 0; index < count; index += 1) {
    const pick = models[Math.floor(random() * models.length)];
    if (pick === undefined) break;

    // 长尾：平方根偏置让小会话占多数，偶尔冒出一个大会话。
    const calls = 1 + Math.floor(random() ** 2 * 120);
    const jitter = 0.7 + random() * 0.6;
    const startMs = baseMs + Math.floor(random() * 86_400_000 * 14);

    points.push({
      tool: tools[Math.floor(random() * tools.length)] ?? "Claude Code",
      externalId: `mock-session-${String(index).padStart(3, "0")}`,
      projectDir: random() < 0.12 ? "" : `/workspace/project-${1 + (index % 6)}`,
      model: pick.model,
      calls,
      tokens: Math.round(pick.tokensPerCall * calls * jitter),
      costMicros: Math.round(pick.costPerCall * calls * jitter),
      startMs,
      endMs: startMs + calls * (60_000 + Math.floor(random() * 120_000)),
    });
  }

  return points.sort((a, b) => b.costMicros - a.costMicros);
}

/** mock 也要对筛选有反应：否则用户点了筛选看不出区别，会以为是坏的。 */
export function buildMockDashboard(
  range: TimeRange,
  filters?: MockFilters,
): DashboardData {
  const filter = filters ?? NO_FILTERS;
  const combos = combosFor(filter);

  const random = seeded(SEEDS[range]);
  const trend = bucketsFor(range, random);

  // 留下的组合按份额求和：全选 = 1，多选 = 份额相加，所以"多选几个就变大"。
  const ratio = combos.reduce((sum, combo) => sum + combo.share, 0);
  const modelShares = shareBy(combos, (combo) => combo.model);
  const projectShares = shareBy(combos, (combo) => combo.project);

  const narrowed = trend.map((bucket) => ({
    ...bucket,
    calls: Math.round(bucket.calls * ratio),
    tokens: Math.round(bucket.tokens * ratio),
    costMicros: Math.round(bucket.costMicros * ratio),
    cacheReadTokens: Math.round(bucket.cacheReadTokens * ratio),
    cacheReadCostMicros: Math.round(bucket.cacheReadCostMicros * ratio),
  }));

  const costMicros = narrowed.reduce(
    (sum, bucket) => sum + bucket.costMicros,
    0,
  );
  const tokens = narrowed.reduce((sum, bucket) => sum + bucket.tokens, 0);
  const calls = narrowed.reduce((sum, bucket) => sum + bucket.calls, 0);

  // 模型趋势：每个模型一条"生命线"——活跃窗口（新模型中途出现、旧模型中途退场）、
  // 数量级差异（scale 跨一个数量级）、逐桶噪声、偶发爆发与静默日。全程在线的平滑
  // 正弦不像真实用量。份额逐桶归一化，接 IPC 后换真实按模型分桶。
  // 注意 shareBy 返回的是 Map，取键必须用 keys()——Object.keys 对 Map 恒为空。
  const modelNames = [...modelShares.keys()];
  const bucketCount = trend.length;
  interface Lifeline {
    readonly active: ReadonlySet<number>;
    readonly scale: number;
  }

  const lifelines: Lifeline[] = modelNames.map((_name, modelIndex) => {
    // 只有份额前二的模型全程在线；其余是碎片化活跃段——出现 → 活一段 → 消失一阵
    // → 可能再回来，像真实的新模型试用与弃用节奏。
    const always = modelIndex < 2;
    const active = new Set<number>();

    if (always) {
      for (let bucket = 0; bucket < bucketCount; bucket += 1) active.add(bucket);
    } else {
      let cursor = Math.floor(random() * bucketCount * 0.5);
      const end = bucketCount - Math.floor(random() * bucketCount * 0.25);

      while (cursor < end) {
        const length = 1 + Math.floor(random() * Math.max(bucketCount * 0.35, 1));

        for (let bucket = cursor; bucket < Math.min(cursor + length, end); bucket += 1) {
          active.add(bucket);
        }

        cursor += length + 1 + Math.floor(random() * bucketCount * 0.3);
      }
    }

    return { active, scale: 0.1 + random() ** 2 * 3.9 };
  });

  const sharesByBucket = trend.map((_, bucketIndex) => {
    const raw = modelNames.map((name, modelIndex) => {
      const line = lifelines[modelIndex];

      if (line === undefined || !line.active.has(bucketIndex)) {
        return 0;
      }

      let value =
        (modelShares.get(name) ?? 0) * line.scale * (0.15 + random() * 1.85);

      if (random() < 0.12) {
        value = 0; // 静默一天
      } else if (random() < 0.08) {
        value *= 4 + random() * 8; // 偶发大爆发
      }

      return value;
    });
    const sum = raw.reduce((acc, value) => acc + value, 0);

    if (sum === 0) {
      // 全员静默的桶退回基准占比：总量不为零时图上不能开天窗。
      const baseSum = modelNames.reduce(
        (acc, name) => acc + (modelShares.get(name) ?? 0),
        0,
      );

      return modelNames.map((name) => {
        const base = modelShares.get(name) ?? 0;

        return baseSum === 0 ? 0 : base / baseSum;
      });
    }

    return raw.map((value) => value / sum);
  });
  const modelTrend: ModelTrendSeries[] = modelNames.map((model, modelIndex) => ({
    model,
    costMicros: narrowed.map(
      (bucket, bucketIndex) =>
        Math.round(bucket.costMicros * (sharesByBucket[bucketIndex]?.[modelIndex] ?? 0)),
    ),
    tokens: narrowed.map(
      (bucket, bucketIndex) =>
        Math.round(bucket.tokens * (sharesByBucket[bucketIndex]?.[modelIndex] ?? 0)),
    ),
    calls: narrowed.map(
      (bucket, bucketIndex) =>
        Math.round(bucket.calls * (sharesByBucket[bucketIndex]?.[modelIndex] ?? 0)),
    ),
  }));

  // 明细行在"剩下的条目"之间按份额分摊：行之和等于上面的总量，选了单个就占满。
  // 费用不沿用同一份额：模型单价差异巨大（相差一个数量级很常见），按 tokens × 价格系数
  // 分摊再归一，总量守恒——否则所有模型的单次均价一模一样，效率散点成一条平线。
  const priceFactors = new Map(
    modelNames.map((name) => [name, 0.2 + random() ** 2 * 2.6]),
  );
  const compositionRows = ALL_MOCK_MODELS.filter((entry) =>
    modelShares.has(entry.label),
  ).map(({ label }) => {
    const share = ratio === 0 ? 0 : (modelShares.get(label) ?? 0) / ratio;
    const unpriced = label === UNPRICED_MODEL;
    const rowTokens = Math.round(tokens * share);

    return {
      label,
      tokens: rowTokens,
      calls: Math.round(calls * share),
      unknownCost: unpriced,
      weight: unpriced ? 0 : rowTokens * (priceFactors.get(label) ?? 1),
    };
  });
  const weightSum = compositionRows.reduce((sum, row) => sum + row.weight, 0);
  const composition: BreakdownRow[] = compositionRows.map(({ weight, ...row }) => ({
    ...row,
    costMicros: weightSum === 0 ? 0 : Math.round((costMicros * weight) / weightSum),
  }));

  const projects: BreakdownRow[] = MOCK_PROJECTS.filter((entry) =>
    projectShares.has(entry.project),
  ).map(({ project: name }) => {
    const share = ratio === 0 ? 0 : (projectShares.get(name) ?? 0) / ratio;

    return {
      label: name,
      tokens: Math.round(tokens * share),
      costMicros: Math.round(costMicros * share),
      calls: Math.round(calls * share),
      unknownCost: false,
    };
  });

  const flow = flowFor({ costMicros, tokens, calls }, combos);

  const activity = activityFor({ costMicros, tokens, calls }, random);
  // 日历卡固定看近一年，不跟随筛选范围：日数据永远生成 365 天，总量按天数比例
  // 放大，让"每天一格"的量级与其他卡的口径一致。颜色是窗口内相对深浅，不受影响。
  const daily = dailyFor(
    "all",
    scale({ costMicros, tokens, calls }, DAYS_PER_RANGE.all / DAYS_PER_RANGE[range]),
    random,
  );
  const spread = spreadFor(random).filter((row) => modelShares.has(row.label));
  // 会话时间以趋势窗口的起点为锚铺开，随筛选范围变化。
  const sessions = sessionsFor(random, modelShares, narrowed[0]?.startMs ?? Date.now());

  // 活跃工具数与总览页工具维度同口径：链路（工具 → 模型 → 项目）里只当源头的节点是工具。
  const flowTargets = new Set(flow.map((link) => link.target));
  const activeTools = new Set(
    flow.filter((link) => !flowTargets.has(link.source)).map((link) => link.source),
  ).size;

  // 费用构成：瀑布的分项之和必须精确等于总费用，四舍五入的残差都记到输出桶
  // （量级最大的分项）。分项比例取自典型编码用量的量级：输入与缓存读占大头，
  // 缓存写最小；mock 只求形状对，接 IPC 后换真实的 *_cost_micros 求和。
  const cacheReadCost = narrowed.reduce(
    (sum, bucket) => sum + bucket.cacheReadCostMicros,
    0,
  );
  const freshCost = Math.max(costMicros - cacheReadCost, 0);
  const inputCost = Math.round(freshCost * 0.3);
  const reasoningCost = Math.round(freshCost * 0.18);
  const cacheWriteCost = Math.round(freshCost * 0.06);
  const outputCost = Math.max(
    freshCost - inputCost - reasoningCost - cacheWriteCost,
    0,
  );

  return {
    trend: narrowed,
    modelTrend,
    composition,
    projects,
    flow,
    activity,
    daily,
    spread,
    sessions,
    costComposition: {
      inputCostMicros: inputCost,
      outputCostMicros: outputCost,
      reasoningCostMicros: reasoningCost,
      cacheReadCostMicros: cacheReadCost,
      cacheWriteCostMicros: cacheWriteCost,
    },
    unknownModelCount: composition.filter((row) => row.unknownCost).length,
    totals: {
      costMicros,
      tokens,
      calls,
      sessions: Math.round(calls / 8) + 1,
      projects: projects.length,
      cacheReadTokens: narrowed.reduce(
        (sum, bucket) => sum + bucket.cacheReadTokens,
        0,
      ),
      // mock 推算：单次调用 1–2.5 分钟活跃时长。
      activeDurationMs: Math.round(calls * (60_000 + random() * 90_000)),
      activeTools,
    },
  };
}
