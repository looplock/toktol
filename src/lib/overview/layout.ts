/**
 * 布局文档：卡片位置 + 卡片自身配置（看哪个指标、怎么画）存成一份。
 * 合成一份是为了"重置布局"能同时把两者恢复干净，不会留下位置复位、图表还停在饼图的半吊子状态。
 *
 * 持久化键暂不进 `src/constants.ts`：目前只有前端读它，还没成为跨语言契约。
 * 等后端或网关也要读时再上升——那时由 `verify:constants` 盯着两侧。
 */

import type { BreakdownDimension, ChartKind, Metric } from "./types";

const LS_KEY_LAYOUT = "toktol-overview-layout";

const LAYOUT_VERSION = 2;

/** 网格列数：决定横向吸附粒度（容器宽/列数），DashboardGrid 用同一份值初始化。 */
export const LAYOUT_COLUMNS = 48;

// v1 是 12 列坐标系；横向粒度改为 48 列后同一 x/w 表示的宽度变成 1/4，读入时等比放大。
const V1_COLUMNS = 12;

export interface CardConfig {
  readonly metric: Metric;
  readonly chart: ChartKind;
  /** 仅构成卡使用：拆分维度。旧存档没有这个键，读入时按缺省（模型）兜底。 */
  readonly dimension?: BreakdownDimension;
  /** 仅用量卡使用：模型用量 / 缓存用量 / 整体趋势。旧存档按缺省（模型用量）兜底。 */
  readonly view?: UsageView;
  /** 仅日历卡使用：时间窗口（天）。undefined = 365。 */
  readonly calendarWindow?: number;
  /** 仅日历卡使用：颜色分档。undefined = 5 档（GitHub 默认），0 = 连续渐变。 */
  readonly calendarSteps?: number;
  /** 仅组合卡使用：调用次数柱 + 累计费用线，或费用柱 + 调用次数线。undefined = calls。 */
  readonly comboView?: ComboView;
  /** 仅效率卡使用：模型效率 / 会话消耗。旧存档按缺省（模型效率）兜底。 */
  readonly efficiencyView?: EfficiencyView;
  /** 外观设置（抽屉"外观"组）：全部可选，缺省即默认外观，旧存档天然兼容。 */
  readonly palette?: CardPaletteScheme;
  readonly showLegend?: boolean;
  readonly showGridLines?: boolean;
  readonly yAxisFromZero?: boolean;
}

export const CARD_PALETTES = ["default", "indigo", "warm", "gray"] as const;

export type CardPaletteScheme = (typeof CARD_PALETTES)[number];

/** 合并后用量卡的视图：按模型拆分 / 缓存读取 vs 新生成 / 不分段的总柱。 */
export const USAGE_VIEWS = ["model", "cache", "total"] as const;

export type UsageView = (typeof USAGE_VIEWS)[number];

/** 日历卡的时间窗口（天）与颜色分档：分档 0 表示连续渐变，其余为离散档数。 */
export const CALENDAR_WINDOWS = [30, 90, 183, 365] as const;
export const CALENDAR_STEPS = [3, 4, 5, 6] as const;
export const CALENDAR_WINDOW_DEFAULT = 365;
export const CALENDAR_STEPS_DEFAULT = 5;
/** 分档 0 = 连续渐变，其余为离散档数。 */
export const CALENDAR_STEPS_CONTINUOUS = 0;

/** 组合卡的视图：calls = 调用次数柱 + 累计费用线；cost = 费用柱 + 调用次数线。 */
export const COMBO_VIEWS = ["calls", "cost"] as const;

export type ComboView = (typeof COMBO_VIEWS)[number];

/** 效率卡的视图：model = 模型效率气泡；session = 会话消耗气泡（一会话 × 模型一点）。 */
export const EFFICIENCY_VIEWS = ["model", "session"] as const;

export type EfficiencyView = (typeof EFFICIENCY_VIEWS)[number];

export interface LayoutWidget {
  readonly id: string;
  readonly x: number;
  readonly y: number;
  readonly w: number;
  readonly h: number;
}

export interface LayoutDoc {
  readonly version: number;
  readonly widgets: readonly LayoutWidget[];
  readonly cards: Readonly<Record<string, CardConfig>>;
}

export const DEFAULT_WIDGETS: readonly LayoutWidget[] = [
  { id: "usage", x: 0, y: 0, w: 32, h: 6 },
  { id: "totals", x: 32, y: 0, w: 16, h: 6 },
  { id: "composition", x: 0, y: 6, w: 20, h: 5 },
  // trend（花费趋势）与 cumulative（累计用量）已下线；cache 卡已并入用量卡（视图切换）、
  // projects 卡已并入构成卡（维度切换）、费用与调用量卡已并入组合卡（视图切换）、
  // scatter（会话开销散点）已整体移除——旧存档里的这些 id 由 withDefaults 丢弃。
  // 日历卡（GitHub 式活跃日历）曾整体移除又恢复，沿用 calendar 这个 id：
  // 旧存档的位置与配置直接被继承，heatmapView 键已从配置面删除（读入时随配置一起废弃）。
  { id: "calendar", x: 0, y: 11, w: 48, h: 9 },
  { id: "flow", x: 0, y: 20, w: 48, h: 8 },
  { id: "spread", x: 0, y: 28, w: 48, h: 6 },
  { id: "combo", x: 0, y: 34, w: 48, h: 6 },
  { id: "modelEfficiency", x: 0, y: 40, w: 48, h: 6 },
  { id: "costWaterfall", x: 0, y: 46, w: 48, h: 6 },
];

type Storage = Pick<globalThis.Storage, "getItem" | "setItem">;

export const DEFAULT_CARD_CONFIG: CardConfig = { metric: "tokens", chart: "bar" };

/** 按卡片 id 取默认布局。id 必须来自 DEFAULT_WIDGETS（卡片 id 全集）；未知 id
 *  说明调用方与默认表脱节，直接抛错暴露 bug——不再用 as 把 undefined 掩盖成
 *  运行时崩溃。生产路径（withDefaults）保证 doc.widgets 含全部默认卡片，
 *  这里只接住语义上不可能的兜底分支。 */
export function defaultWidgetFor(id: string): LayoutWidget {
  const found = DEFAULT_WIDGETS.find((widget) => widget.id === id);
  if (found === undefined) throw new Error(`unknown widget id: ${id}`);
  return found;
}

function isWidget(value: unknown): value is LayoutWidget {
  if (typeof value !== "object" || value === null) return false;
  const widget = value as Record<string, unknown>;

  return (
    typeof widget["id"] === "string" &&
    [widget["x"], widget["y"]].every((field) => typeof field === "number" && Number.isFinite(field)) &&
    // 尺寸必须为正：gridstack 偶发存出 0 尺寸，读回来会渲染成看不见也拖不到的卡片。
    [widget["w"], widget["h"]].every(
      (field) => typeof field === "number" && Number.isFinite(field) && field > 0,
    )
  );
}

function isCardConfig(value: unknown): value is CardConfig {
  if (typeof value !== "object" || value === null) return false;
  const config = value as Record<string, unknown>;
  const boolOrUndefined = (field: unknown): boolean =>
    field === undefined || typeof field === "boolean";

  return (
    (config["metric"] === "cost" ||
      config["metric"] === "tokens" ||
      config["metric"] === "calls") &&
    (config["chart"] === "bar" ||
      config["chart"] === "line" ||
      config["chart"] === "share" ||
      config["chart"] === "treemap" ||
      config["chart"] === "calendar") &&
    (config["dimension"] === undefined ||
      config["dimension"] === "model" ||
      config["dimension"] === "project" ||
      config["dimension"] === "tool") &&
    (config["view"] === undefined ||
      (USAGE_VIEWS as readonly unknown[]).includes(config["view"])) &&
    (config["calendarWindow"] === undefined ||
      (typeof config["calendarWindow"] === "number" &&
        (CALENDAR_WINDOWS as readonly unknown[]).includes(config["calendarWindow"]))) &&
    (config["calendarSteps"] === undefined ||
      (typeof config["calendarSteps"] === "number" &&
        (config["calendarSteps"] === CALENDAR_STEPS_CONTINUOUS ||
          (CALENDAR_STEPS as readonly unknown[]).includes(config["calendarSteps"])))) &&
    (config["comboView"] === undefined ||
      (COMBO_VIEWS as readonly unknown[]).includes(config["comboView"])) &&
    (config["efficiencyView"] === undefined ||
      (EFFICIENCY_VIEWS as readonly unknown[]).includes(config["efficiencyView"])) &&
    (config["palette"] === undefined ||
      (CARD_PALETTES as readonly unknown[]).includes(config["palette"])) &&
    boolOrUndefined(config["showLegend"]) &&
    boolOrUndefined(config["showGridLines"]) &&
    boolOrUndefined(config["yAxisFromZero"])
  );
}

function readCards(doc: Record<string, unknown>): Record<string, CardConfig> {
  const rawCards =
    typeof doc["cards"] === "object" && doc["cards"] !== null
      ? (doc["cards"] as Record<string, unknown>)
      : {};

  const cards: Record<string, CardConfig> = {};
  for (const [id, value] of Object.entries(rawCards)) {
    // 旧存档的 github 画法已删：校验前归位到 calendar，其余配置（指标/窗口/分档）原样保留。
    const candidate =
      typeof value === "object" &&
      value !== null &&
      (value as Record<string, unknown>)["chart"] === "github"
        ? { ...value, chart: "calendar" }
        : value;
    if (isCardConfig(candidate)) cards[id] = candidate;
  }

  return cards;
}

/** 读不到或读到脏数据就返回 null（不猜、不静默用半份）。SSR（node 测试环境）
 * 没有 localStorage：无存储即无布局。 */
export function readLayout(storage?: Storage): LayoutDoc | null {
  const ls = storage ?? (typeof localStorage === "undefined" ? null : localStorage);
  if (ls === null) return null;

  let parsed: unknown;

  try {
    const raw = ls.getItem(LS_KEY_LAYOUT);
    if (raw === null) return null;

    parsed = JSON.parse(raw);
  } catch {
    return null;
  }

  if (typeof parsed !== "object" || parsed === null) return null;
  const doc = parsed as Record<string, unknown>;
  // v1 只在读入时迁移，不原样回写——下次保存自然就是 v2。
  if (doc["version"] !== LAYOUT_VERSION && doc["version"] !== 1) return null;
  if (!Array.isArray(doc["widgets"])) return null;

  const widgets = doc["widgets"].filter(isWidget);
  if (doc["version"] !== 1) {
    return { version: LAYOUT_VERSION, widgets, cards: readCards(doc) };
  }

  const scale = LAYOUT_COLUMNS / V1_COLUMNS;
  return {
    version: LAYOUT_VERSION,
    widgets: widgets.map((widget) => ({ ...widget, x: widget.x * scale, w: widget.w * scale })),
    cards: readCards(doc),
  };
}

/** 用默认表补齐：新加的卡片要出现，删掉的卡片不能残留。 */
export function withDefaults(defaults: readonly LayoutWidget[], doc: LayoutDoc | null): LayoutDoc {
  const known = doc === null ? [] : doc.widgets;
  const cards = doc === null ? {} : doc.cards;

  const widgets = defaults.map(
    (fallback) => known.find((widget) => widget.id === fallback.id) ?? fallback,
  );

  return { version: LAYOUT_VERSION, widgets, cards };
}

export function writeLayout(doc: LayoutDoc, storage: Storage = localStorage): void {
  try {
    storage.setItem(LS_KEY_LAYOUT, JSON.stringify(doc));
  } catch {
    // 写不进去（隐私模式等）就静默跳过，本次会话照样能拖。
  }
}
