import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { emptyData, geo, mockData, strings } from "./fixtures";
import { TotalsCard } from "./TotalsCard";

// SSR 直渲：数值由 AnimatedNumber 的 effect 写入，首帧为空 span——
// 断言标签结构与零值分支（命中率 "—"），数值语义归 lib/format 的单测。

it("统计卡：大数标签与六项指标齐全", () => {
  const html = renderToStaticMarkup(
    <TotalsCard geo={geo("totals")} strings={strings} data={mockData()} />,
  );

  expect(html).toContain(strings.cardTotals);
  expect(html).toContain(strings.overviewTotalTokens);
  expect(html).toContain(strings.overviewTotalCalls);
  expect(html).toContain(strings.overviewUsageTime);
  expect(html).toContain(strings.overviewSessionCount);
  expect(html).toContain(strings.overviewTotalProjects);
  expect(html).toContain(strings.overviewActiveTools);
  expect(html).toContain(strings.overviewCacheHitRate);
  // 日均注记在 AnimatedNumber 的 format 回调里拼出，SSR 首帧为空 span，不在此断言。
});

it("统计卡：零数据时缓存命中率显示为占位破折号", () => {
  const html = renderToStaticMarkup(
    <TotalsCard geo={geo("totals")} strings={strings} data={emptyData()} />,
  );

  expect(html).toContain("—");
});
