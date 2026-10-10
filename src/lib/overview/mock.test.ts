import { describe, expect, it } from "vitest";

import { buildMockDashboard, MODEL_OPTIONS, type MockFilters } from "./mock";
import { type TimeRange } from "./types";

const NONE: MockFilters = {
  tools: new Set(),
  models: new Set(),
  projects: new Set(),
  query: "",
};

function withFilters(part: Partial<MockFilters>): MockFilters {
  return { ...NONE, ...part };
}

const UNPRICED = "local/fine-tuned";

describe("buildMockDashboard", () => {
  it("同范围同筛选必须出同一组数", () => {
    expect(buildMockDashboard("d7", NONE)).toEqual(
      buildMockDashboard("d7", NONE),
    );
  });

  it("每个范围的时间桶数固定", () => {
    const counts: Record<TimeRange, number> = {
      today: buildMockDashboard("today").trend.length,
      h24: buildMockDashboard("h24").trend.length,
      d7: buildMockDashboard("d7").trend.length,
      d30: buildMockDashboard("d30").trend.length,
      all: buildMockDashboard("all").trend.length,
    };

    expect(counts).toEqual({ today: 24, h24: 24, d7: 7, d30: 30, all: 12 });
  });

  it("选了模型就只剩它，总量也跟着变小", () => {
    const all = buildMockDashboard("d7", NONE);
    const one = buildMockDashboard(
      "d7",
      withFilters({ models: new Set(["gpt-5-codex"]) }),
    );

    expect(one.composition.map((row) => row.label)).toEqual(["gpt-5-codex"]);
    expect(one.totals.calls).toBeLessThan(all.totals.calls);
  });

  it("选了项目就只剩它，项目数也跟着变 1", () => {
    const one = buildMockDashboard(
      "d7",
      withFilters({ projects: new Set(["website"]) }),
    );

    expect(one.projects.map((row) => row.label)).toEqual(["website"]);
    expect(one.totals.projects).toBe(1);
  });

  it("多选是份额叠加，不是只取第一个", () => {
    const all = buildMockDashboard("d7", NONE);
    const one = buildMockDashboard(
      "d7",
      withFilters({ models: new Set(["gpt-5-codex"]) }),
    );
    const two = buildMockDashboard(
      "d7",
      withFilters({ models: new Set(["gpt-5-codex", "claude-haiku-4-5"]) }),
    );

    expect(two.composition.map((row) => row.label).sort()).toEqual([
      "claude-haiku-4-5",
      "gpt-5-codex",
    ]);
    expect(two.totals.calls).toBeGreaterThan(one.totals.calls);
    expect(two.totals.calls).toBeLessThan(all.totals.calls);
  });

  it("搜索按名字子串收窄，任一维度命中即保留", () => {
    const hit = buildMockDashboard("d7", withFilters({ query: "website" }));

    // 项目名命中 → 该项目留下，其余维度的组合也跟着留下（不是整屏归零）。
    expect(hit.projects.map((row) => row.label)).toEqual(["website"]);
    expect(hit.totals.calls).toBeGreaterThan(0);
    expect(hit.composition.length).toBeGreaterThan(0);
  });

  it("搜索没命中就归零，绝不退回全量", () => {
    const miss = buildMockDashboard(
      "d7",
      withFilters({ query: "没有这个名字" }),
    );

    expect(miss.totals.calls).toBe(0);
    expect(miss.composition).toHaveLength(0);
    expect(miss.projects).toHaveLength(0);
    expect(miss.flow).toHaveLength(0);
  });

  it("构成行的条数之和等于总量，不做二次折扣", () => {
    const data = buildMockDashboard(
      "d7",
      withFilters({ models: new Set(["gpt-5-codex"]) }),
    );
    const sum = data.composition.reduce((total, row) => total + row.calls, 0);

    expect(Math.abs(sum - data.totals.calls)).toBeLessThanOrEqual(
      data.composition.length,
    );
  });

  it("缓存读取量落在 0 与该桶 token 之间", () => {
    for (const bucket of buildMockDashboard("d7", NONE).trend) {
      expect(bucket.cacheReadTokens).toBeGreaterThanOrEqual(0);
      expect(bucket.cacheReadTokens).toBeLessThanOrEqual(bucket.tokens);
    }
  });

  it("费用构成分项之和精确等于总费用，各分项非负", () => {
    for (const range of ["today", "d7", "all"] as const) {
      const data = buildMockDashboard(range, NONE);
      const composition = data.costComposition;
      const parts = [
        composition.inputCostMicros,
        composition.outputCostMicros,
        composition.reasoningCostMicros,
        composition.cacheReadCostMicros,
        composition.cacheWriteCostMicros,
      ];

      for (const part of parts) {
        expect(part).toBeGreaterThanOrEqual(0);
      }
      // 瀑布的总计柱直接用分项求和，残差会让图和总量对不上。
      expect(parts.reduce((sum, part) => sum + part, 0)).toBe(
        data.totals.costMicros,
      );
    }
  });

  it("模型趋势序列与趋势桶对齐，各模型之和等于桶总量", () => {
    const data = buildMockDashboard("d7", NONE);

    // 堆叠图的数据源不能是空的：这里守住"生成了但恒为空"的回归。
    expect(data.modelTrend.length).toBeGreaterThan(1);
    for (const series of data.modelTrend) {
      expect(series.tokens).toHaveLength(data.trend.length);
    }

    const sumByBucket = data.trend.map((_, index) =>
      data.modelTrend.reduce((sum, series) => sum + (series.tokens[index] ?? 0), 0),
    );
    for (const [index, bucket] of data.trend.entries()) {
      // 各模型份额经过取整，允许每桶 ±模型数的误差。
      expect(Math.abs((sumByBucket[index] ?? 0) - bucket.tokens)).toBeLessThanOrEqual(
        data.modelTrend.length,
      );
    }
  });

  it("选了模型后，模型趋势只剩它的序列", () => {
    const one = buildMockDashboard(
      "d7",
      withFilters({ models: new Set(["gpt-5-codex"]) }),
    );

    expect(one.modelTrend.map((series) => series.model)).toEqual(["gpt-5-codex"]);
  });

  it("流向图的中层节点进出守恒，否则带宽读不通", () => {
    const { flow } = buildMockDashboard("d7", NONE);
    const inflow = new Map<string, number>();
    const outflow = new Map<string, number>();

    for (const link of flow) {
      inflow.set(link.target, (inflow.get(link.target) ?? 0) + link.tokens);
      outflow.set(link.source, (outflow.get(link.source) ?? 0) + link.tokens);
    }

    const middle = [...inflow.keys()].filter((name) => outflow.has(name));
    expect(middle.length).toBeGreaterThan(0);
    for (const name of middle) {
      expect(inflow.get(name)).toBe(outflow.get(name));
    }
  });

  it("选了工具后，流向里只剩它作起点", () => {
    const { flow } = buildMockDashboard(
      "d7",
      withFilters({ tools: new Set(["Codex CLI"]) }),
    );

    // 起点 = 被选工具本身 + 全部模型（中层节点也作为项目流的起点）。
    expect(new Set(flow.map((link) => link.source))).toEqual(
      new Set(["Codex CLI", ...MODEL_OPTIONS]),
    );
  });

  it("未定价模型的流入费用为 0，但 token 不为 0", () => {
    const { flow } = buildMockDashboard("d7", NONE);
    const into = flow.filter((link) => link.target === UNPRICED);

    expect(into.length).toBeGreaterThan(0);
    expect(into.every((link) => link.costMicros === 0 && link.tokens > 0)).toBe(
      true,
    );
  });

  it("活跃热力图是 7×24 格，周末白天比深夜有量", () => {
    const activity = buildMockDashboard("d7", NONE).activity;

    expect(activity).toHaveLength(7 * 24);
    const noon =
      activity.find((cell) => cell.day === 1 && cell.hour === 12)?.tokens ?? 0;
    const night =
      activity.find((cell) => cell.day === 1 && cell.hour === 3)?.tokens ?? 0;
    expect(noon).toBeGreaterThan(night);
  });

  it("日历固定生成近一年 365 天（不跟随筛选范围），日期是 ISO 形状且单调递增", () => {
    for (const range of ["today", "d7", "d30", "all"] as const) {
      const daily = buildMockDashboard(range, NONE).daily;

      expect(daily).toHaveLength(365);
      expect(
        daily.every((point) => /^\d{4}-\d{2}-\d{2}$/.test(point.date)),
      ).toBe(true);
      expect(daily.map((point) => point.date)).toEqual(
        daily.map((point) => point.date).sort(),
      );
    }
  });

  it("箱线图五数严格递增，选了模型后只剩它", () => {
    const all = buildMockDashboard("d7", NONE).spread;
    // 定价模型全员出场（未定价的不参与：没有价格就没有"每调用开销"）。
    expect(all.map((row) => row.label)).toEqual(
      MODEL_OPTIONS.filter((model) => model !== UNPRICED),
    );
    for (const row of all) {
      expect([...row.tokens]).toEqual([...row.tokens].sort((a, b) => a - b));
    }

    const one = buildMockDashboard(
      "d7",
      withFilters({ models: new Set(["claude-haiku-4-5"]) }),
    ).spread;
    expect(one.map((row) => row.label)).toEqual(["claude-haiku-4-5"]);
  });

  it("未定价模型始终在，且开销记成未知而不是 0", () => {
    const data = buildMockDashboard("d7", NONE);
    const unpriced = data.composition.filter((row) => row.unknownCost);

    expect(unpriced).toHaveLength(data.unknownModelCount);
    expect(unpriced.length).toBeGreaterThan(0);
    expect(
      unpriced.every((row) => row.costMicros === 0 && row.tokens > 0),
    ).toBe(true);
  });
});
