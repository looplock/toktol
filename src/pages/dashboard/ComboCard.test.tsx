import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { ComboCard } from "./ComboCard";

const noop = (): void => {};

it("组合卡默认档（用量与花费）：标题与副标题", () => {
  const html = renderToStaticMarkup(
    <ComboCard
      geo={geo("combo")}
      strings={strings}
      data={mockData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardCombo);
  expect(html).toContain(strings.cardComboSub);
  // 视图切换在设置抽屉里（收起态不渲染），SSR 不断言。
  expect(html).toContain("tt-chart");
});

it("费用档（费用与调用量）换标题；零数据走空态", () => {
  const costHtml = renderToStaticMarkup(
    <ComboCard
      geo={geo("combo")}
      strings={strings}
      data={mockData()}
      config={{ ...defaultConfig, comboView: "cost" }}
      onConfigChange={noop}
    />,
  );

  expect(costHtml).toContain(strings.cardCostCalls);
  expect(costHtml).toContain(strings.cardCostCallsSub);
  expect(costHtml).not.toContain(strings.cardComboSub);

  const emptyHtml = renderToStaticMarkup(
    <ComboCard
      geo={geo("combo")}
      strings={strings}
      data={emptyData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(emptyHtml).toContain(strings.cardEmpty);
  expect(emptyHtml).not.toContain("tt-chart");
});
