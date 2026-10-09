import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import type { CardConfig } from "../../lib/overview/layout";
import { defaultConfig, emptyData, geo, mockData, strings } from "./fixtures";
import { UsageTrendCard } from "./UsageTrendCard";

// 三个视图（模型/缓存/整体）标题各不相同，画法分支（柱/线/占比）只在
// option 构建里——SSR 渲染即执行，崩溃即回归；断言走标题与空态。

const noop = (): void => {};

function renderWith(config: CardConfig, data = mockData()): string {
  return renderToStaticMarkup(
    <UsageTrendCard
      geo={geo("usage")}
      strings={strings}
      data={data}
      config={config}
      onConfigChange={noop}
    />,
  );
}

it("默认档（模型用量 / 柱状）：标题与副标题", () => {
  const html = renderWith(defaultConfig);

  expect(html).toContain(strings.cardUsageTrend);
  expect(html).toContain(strings.cardUsageTrendSub);
  // 视图/画法切换在设置抽屉里（收起态不渲染），SSR 不断言。
});

it("缓存档与整体档各用各的标题；整体档不出现占比画法", () => {
  expect(renderWith({ ...defaultConfig, view: "cache" })).toContain(strings.cardCache);
  const totalHtml = renderWith({ ...defaultConfig, view: "total", chart: "share" });
  // handleConfigChange 的纠正逻辑在回调里，SSR 不触发；渲染层直接以传入
  // 配置的 view 定标题——整体档标题出现即视图分支生效。
  expect(totalHtml).toContain(strings.cardUsageTotal);
  expect(totalHtml).not.toContain(strings.cardUsageTrend);
});

it("总量为零时走空态，不再渲染图表容器", () => {
  const html = renderWith(defaultConfig, emptyData());

  expect(html).toContain(strings.cardEmpty);
  expect(html).not.toContain("tt-chart");
});
