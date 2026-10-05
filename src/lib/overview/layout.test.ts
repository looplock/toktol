import { describe, expect, it } from "vitest";

import {
  DEFAULT_CARD_CONFIG,
  type LayoutWidget,
  readLayout,
  withDefaults,
  writeLayout,
} from "./layout";

const DEFAULTS: LayoutWidget[] = [
  { id: "usage", x: 0, y: 0, w: 8, h: 6 },
  { id: "totals", x: 8, y: 0, w: 4, h: 6 },
];

function fake(items: Record<string, string> = {}) {
  const map = new Map(Object.entries(items));

  return {
    storage: {
      getItem: (key: string) => map.get(key) ?? null,
      setItem: (key: string, value: string) => void map.set(key, value),
    },
    map,
  };
}

describe("readLayout", () => {
  it("键不存在时返回 null", () => {
    expect(readLayout(fake().storage)).toBeNull();
  });

  it("非 JSON 与未知版本号都当没存", () => {
    expect(readLayout(fake({ "toktol-overview-layout": "{oops" }).storage)).toBeNull();

    const wrongVersion = JSON.stringify({ version: 99, widgets: [], cards: {} });
    expect(readLayout(fake({ "toktol-overview-layout": wrongVersion }).storage)).toBeNull();
  });

  it("v1（12 列）布局迁移到 v2：x/w 放大 4 倍", () => {
    const raw = JSON.stringify({
      version: 1,
      widgets: [{ id: "usage", x: 2, y: 1, w: 8, h: 5 }],
      cards: { usage: { metric: "cost", chart: "line" } },
    });

    const doc = readLayout(fake({ "toktol-overview-layout": raw }).storage);

    expect(doc?.version).toBe(2);
    expect(doc?.widgets).toEqual([{ id: "usage", x: 8, y: 1, w: 32, h: 5 }]);
    expect(doc?.cards["usage"]).toEqual({ metric: "cost", chart: "line" });
  });

  it("丢掉形状不对的 widget 与卡片配置，保住剩下的", () => {
    const raw = JSON.stringify({
      version: 2,
      widgets: [{ id: "usage", x: 2, y: 0, w: 6, h: 5 }, { id: "bad" }, null],
      cards: { usage: { metric: "cost", chart: "line" }, bad: { metric: "nope" } },
    });

    const doc = readLayout(fake({ "toktol-overview-layout": raw }).storage);

    expect(doc?.widgets).toHaveLength(1);
    expect(doc?.cards["usage"]).toEqual({ metric: "cost", chart: "line" });
    expect(doc?.cards["bad"]).toBeUndefined();
  });

  it("尺寸不为正的 widget 丢弃，由默认表补回", () => {
    const raw = JSON.stringify({
      version: 2,
      widgets: [
        { id: "usage", x: 0, y: 0, w: 0, h: 6 },
        { id: "totals", x: 8, y: 0, w: 4, h: -1 },
        { id: "trend", x: 5, y: 6, w: 7, h: 5 },
      ],
      cards: {},
    });

    const doc = readLayout(fake({ "toktol-overview-layout": raw }).storage);

    expect(doc?.widgets.map((widget) => widget.id)).toEqual(["trend"]);
  });
});

describe("withDefaults", () => {
  it("没有存档时全用默认位置", () => {
    expect(withDefaults(DEFAULTS, null).widgets).toEqual(DEFAULTS);
  });

  it("存档里缺的卡片补默认，存档里已删的卡片不残留", () => {
    const doc = readLayout(
      fake({
        "toktol-overview-layout": JSON.stringify({
          version: 2,
          widgets: [{ id: "usage", x: 4, y: 1, w: 34, h: 7 }, { id: "gone", x: 0, y: 0, w: 1, h: 1 }],
          cards: {},
        }),
      }).storage,
    );

    const merged = withDefaults(DEFAULTS, doc);

    expect(merged.widgets).toEqual([
      { id: "usage", x: 4, y: 1, w: 34, h: 7 },
      { id: "totals", x: 8, y: 0, w: 4, h: 6 },
    ]);
  });
});

describe("writeLayout", () => {
  it("写进去再读出来", () => {
    const { storage } = fake();

    writeLayout({ version: 2, widgets: DEFAULTS, cards: { usage: DEFAULT_CARD_CONFIG } }, storage);
    expect(readLayout(storage)?.widgets).toEqual(DEFAULTS);
  });

  it("storage 抛异常时静默跳过", () => {
    const broken = {
      getItem: () => {
        throw new Error("disabled");
      },
      setItem: () => {
        throw new Error("disabled");
      },
    };

    expect(() => writeLayout({ version: 2, widgets: [], cards: {} }, broken)).not.toThrow();
  });
});
