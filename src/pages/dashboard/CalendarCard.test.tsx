import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { CalendarCard } from "./CalendarCard";

const noop = (): void => {};

it("日历卡：标题与副标题", () => {
  const html = renderToStaticMarkup(
    <CalendarCard
      geo={geo("calendar")}
      strings={strings}
      data={mockData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardHeatmap);
  expect(html).toContain(strings.cardHeatmapSub);
  // 窗口/分档设置在设置抽屉里（收起态不渲染），SSR 不断言。
  expect(html).toContain("tt-chart");
});

it("窗口内一个有记录的日子都没有时走空态", () => {
  const html = renderToStaticMarkup(
    <CalendarCard
      geo={geo("calendar")}
      strings={strings}
      data={emptyData()}
      config={defaultConfig}
      onConfigChange={noop}
    />,
  );

  expect(html).toContain(strings.cardEmpty);
  expect(html).not.toContain("tt-chart");
});
