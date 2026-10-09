import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { ModelEfficiencyCard } from "./ModelEfficiencyCard";

const noop = (): void => {};

it("效率卡默认档：标题、副标题与未定价注记", () => {
  const html = renderToStaticMarkup(
    <ModelEfficiencyCard
      geo={geo("modelEfficiency")}
      strings={strings}
      data={mockData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardModelEfficiency);
  expect(html).toContain(strings.cardModelEfficiencySub);
  // 视图切换在设置抽屉里（收起态不渲染），SSR 不断言。
  // mock 数据含未定价模型，注记随数据出现（n 替换自 unknownModelCount）。
  expect(html).toContain(
    strings.modelEfficiencyUnpriced.replace("{n}", String(mockData().unknownModelCount)),
  );
  expect(html).toContain("tt-chart");
});

it("会话消耗档换标题；零数据走空态", () => {
  const sessionHtml = renderToStaticMarkup(
    <ModelEfficiencyCard
      geo={geo("modelEfficiency")}
      strings={strings}
      data={mockData()}
      config={{ ...defaultConfig, efficiencyView: "session" }}
      onConfigChange={noop}
    />,
  );

  expect(sessionHtml).toContain(strings.cardSessionSpend);
  expect(sessionHtml).not.toContain(strings.cardModelEfficiency);

  const emptyHtml = renderToStaticMarkup(
    <ModelEfficiencyCard
      geo={geo("modelEfficiency")}
      strings={strings}
      data={emptyData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(emptyHtml).toContain(strings.cardEmpty);
  expect(emptyHtml).not.toContain("tt-chart");
});
