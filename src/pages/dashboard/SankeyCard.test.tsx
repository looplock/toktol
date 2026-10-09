import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { SankeyCard } from "./SankeyCard";

const noop = (): void => {};

it("流向卡：标题、副标题与链路节点名", () => {
  const html = renderToStaticMarkup(
    <SankeyCard
      geo={geo("flow")}
      strings={strings}
      data={mockData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardSankey);
  expect(html).toContain(strings.cardSankeySub);
  expect(html).toContain("tt-chart");
});

it("没有正带宽链路时走空态", () => {
  const html = renderToStaticMarkup(
    <SankeyCard
      geo={geo("flow")}
      strings={strings}
      data={emptyData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardEmpty);
  expect(html).not.toContain("tt-chart");
});
