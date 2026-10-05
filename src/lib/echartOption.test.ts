import { describe, expect, it } from "vitest";

import { chartStructureKey, optionFingerprint } from "./echartOption";

describe("chartStructureKey", () => {
  it("同结构（仅数值与文案不同）键相同", () => {
    const a = { xAxis: { data: ["1日"] }, series: [{ type: "bar", data: [1] }] };
    const b = { xAxis: { data: ["2日"] }, series: [{ type: "bar", data: [9, 8] }] };

    expect(chartStructureKey(a)).toBe(chartStructureKey(b));
  });

  it("顶层键顺序无关", () => {
    expect(chartStructureKey({ series: [], grid: {} })).toBe(
      chartStructureKey({ grid: {}, series: [] }),
    );
  });

  it("series 类型变化键变化", () => {
    expect(chartStructureKey({ series: [{ type: "bar" }] })).not.toBe(
      chartStructureKey({ series: [{ type: "line" }] }),
    );
  });

  it("series stack 变化键变化：百分比堆叠面积切普通折线必须整体重建", () => {
    expect(
      chartStructureKey({ series: [{ type: "line", stack: "total" }] }),
    ).not.toBe(chartStructureKey({ series: [{ type: "line" }] }));
  });

  it("yAxis.max 变化键变化：百分比轴切绝对量轴不许走合并（合并会残留旧上限压扁图）", () => {
    expect(
      chartStructureKey({ series: [{ type: "line" }], yAxis: { max: 100 } }),
    ).not.toBe(chartStructureKey({ series: [{ type: "line" }], yAxis: {} }));
  });

  it("顶层组件增减键变化", () => {
    expect(chartStructureKey({ series: [{ type: "bar" }] })).not.toBe(
      chartStructureKey({ series: [{ type: "bar" }], visualMap: {} }),
    );
  });

  it("series 为单对象而不是数组时不炸", () => {
    expect(() => chartStructureKey({ series: { type: "bar" } })).not.toThrow();
  });
});

describe("optionFingerprint", () => {
  it("值相同但对象身份不同时指纹相同", () => {
    const a = { series: [{ type: "bar", data: [1, 2] }] };
    const b = { series: [{ type: "bar", data: [1, 2] }] };

    expect(optionFingerprint(a)).toBe(optionFingerprint(b));
  });

  it("数据变化指纹变化", () => {
    const a = { series: [{ type: "bar", data: [1, 2] }] };
    const b = { series: [{ type: "bar", data: [1, 3] }] };

    expect(optionFingerprint(a)).not.toBe(optionFingerprint(b));
  });

  it("函数按源码文本比较：闭包重建但逻辑不变算同", () => {
    const make = () => ({ tooltip: { valueFormatter: (v: number) => v * 2 } });

    expect(optionFingerprint(make())).toBe(optionFingerprint(make()));
    expect(optionFingerprint(make())).not.toBe(
      optionFingerprint({ tooltip: { valueFormatter: (v: number) => v * 3 } }),
    );
  });

  it("嵌套对象键序无关", () => {
    expect(
      optionFingerprint({ tooltip: { trigger: "axis", order: "valueAsc" } }),
    ).toBe(optionFingerprint({ tooltip: { order: "valueAsc", trigger: "axis" } }));
  });
});
