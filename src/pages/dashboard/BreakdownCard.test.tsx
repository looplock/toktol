import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { BreakdownCard } from "./BreakdownCard";

// 构成卡：标题/副标题/注记由调用方传入；三种画法（柱/环形/树图）的
// option 构建在渲染期执行——三种 chart 配置各渲一遍即全覆盖。

const noop = (): void => {};

const DIMENSIONS = [
  { value: "tool" as const, label: strings.filterTool },
  { value: "project" as const, label: strings.filterProject },
  { value: "model" as const, label: strings.filterModel },
];

function renderWith(overrides: Partial<Parameters<typeof BreakdownCard>[0]> = {}): string {
  return renderToStaticMarkup(
    <BreakdownCard
      geo={geo("composition")}
      strings={strings}
      title={strings.cardComposition}
      subtitle={strings.cardCompositionSub}
      rows={mockData().composition}
      charts={["bar", "share", "treemap"]}
      config={defaultConfig}
      onConfigChange={noop}
      dimensions={DIMENSIONS}
      {...overrides}
    />,
  );
}

it("柱状档：标题、副标题与图表容器", () => {
  const html = renderWith();

  expect(html).toContain(strings.cardComposition);
  expect(html).toContain(strings.cardCompositionSub);
  // 维度切换在设置抽屉里（收起态不渲染），SSR 不断言。
  expect(html).toContain("tt-chart");
});

it("环形与树图档的 option 都能构建", () => {
  expect(
    renderWith({ config: { ...defaultConfig, chart: "share" } }),
  ).toContain("tt-chart");
  expect(
    renderWith({ config: { ...defaultConfig, chart: "treemap" } }),
  ).toContain("tt-chart");
});

it("注记（未知模型数）原样透出；行全零走空态", () => {
  const note = strings.unknownModelsNote.replace("{n}", "3");
  expect(renderWith({ note })).toContain(note);

  const emptyHtml = renderWith({ rows: [] });
  expect(emptyHtml).toContain(strings.cardEmpty);
  expect(emptyHtml).not.toContain("tt-chart");

  // 全空数据下构成行也是空的，同样走空态（双保险口径）。
  expect(renderWith({ rows: emptyData().composition })).toContain(strings.cardEmpty);
});
