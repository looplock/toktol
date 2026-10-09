// 卡片测试共享夹具：mock 数据 + 卡片几何。只服务 SSR 直渲测试——
// 生产数据走 IPC 聚合，形状对齐 buildMockDashboard 的输出。

import { stringsFor } from "../../i18n/strings";
import type { Strings } from "../../i18n/strings";
import { DEFAULT_CARD_CONFIG, type CardConfig } from "../../lib/overview/layout";
import { buildMockDashboard } from "../../lib/overview/mock";
import type { DashboardData } from "../../lib/overview/types";
import type { CardGeometry } from "./geometry";

export const strings: Strings = stringsFor();

/** 默认配置（与首帧渲染一致：费用指标、柱状、模型视图）。 */
export const defaultConfig: CardConfig = { ...DEFAULT_CARD_CONFIG };

/** d7 模拟数据：五桶趋势 + 多模型构成 + 流向链路，覆盖非空分支。 */
export function mockData(): DashboardData {
  return buildMockDashboard("d7");
}

/** 全零数据：所有数组清空、总量清零，卡片全部落入空态分支。 */
export function emptyData(): DashboardData {
  const base = buildMockDashboard("d7");

  return {
    totals: {
      ...base.totals,
      costMicros: 0,
      tokens: 0,
      calls: 0,
      sessions: 0,
      projects: 0,
      cacheReadTokens: 0,
      activeDurationMs: 0,
      activeTools: 0,
    },
    trend: [],
    modelTrend: [],
    composition: [],
    projects: [],
    flow: [],
    activity: [],
    daily: [],
    spread: [],
    sessions: [],
    costComposition: {
      inputCostMicros: 0,
      outputCostMicros: 0,
      reasoningCostMicros: 0,
      cacheReadCostMicros: 0,
      cacheWriteCostMicros: 0,
    },
    unknownModelCount: 0,
  };
}

/** 卡片几何只需要形状合法，值无所谓。 */
export function geo(id: string): CardGeometry {
  return { id, x: 0, y: 0, w: 4, h: 4 };
}
