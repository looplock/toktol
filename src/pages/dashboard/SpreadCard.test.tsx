import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { SpreadCard } from "./SpreadCard";

const noop = (): void => {};

it("箱线卡：标题、副标题，模型与五数轴标签在 option 构建后正常渲染", () => {
  const html = renderToStaticMarkup(
    <SpreadCard
      geo={geo("spread")}
      strings={strings}
      data={mockData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardSpread);
  expect(html).toContain(strings.cardSpreadSub);
  expect(html).toContain("tt-chart");
});

it("没有分布数据时走空态", () => {
  const html = renderToStaticMarkup(
    <SpreadCard
      geo={geo("spread")}
      strings={strings}
      data={emptyData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardEmpty);
  expect(html).not.toContain("tt-chart");
});
