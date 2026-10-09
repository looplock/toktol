import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { emptyData, geo, mockData, strings } from "./fixtures";
import { CostWaterfallCard } from "./CostWaterfallCard";

it("瀑布卡：标题与副标题；有未定价模型时副标题带注记", () => {
  const data = mockData();
  const html = renderToStaticMarkup(
    <CostWaterfallCard geo={geo("costWaterfall")} strings={strings} data={data} />,
  );

  expect(html).toContain(strings.cardCostWaterfall);
  // mock 数据含未定价模型：副标题拼上未知模型注记。
  expect(html).toContain(
    `${strings.cardCostWaterfallSub} · ${strings.unknownModelsNote.replace("{n}", String(data.unknownModelCount))}`,
  );
  expect(html).toContain("tt-chart");
});

it("零数据走空态", () => {
  const html = renderToStaticMarkup(
    <CostWaterfallCard geo={geo("costWaterfall")} strings={strings} data={emptyData()} />,
  );

  expect(html).toContain(strings.cardEmpty);
  expect(html).not.toContain("tt-chart");
});
