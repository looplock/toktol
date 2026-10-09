import { renderToStaticMarkup } from "react-dom/server";
import { expect, it, vi } from "vitest";

// gridstack 在 node 下无法解析/初始化，网格本身只有 effect 逻辑——
// mock 成透传容器，卡片作为 children 照常渲染。
vi.mock("./dashboard/DashboardGrid", () => ({
  DashboardGrid: ({ children }: { readonly children: React.ReactNode }) => (
    <div>{children}</div>
  ),
}));

import { stringsFor } from "../i18n/strings";
import { buildMockDashboard } from "../lib/overview/mock";
import { OverviewPage } from "./Overview";

const strings = stringsFor();
const data = buildMockDashboard("d7");

const SEAM: [typeof data, typeof data] = [data, buildMockDashboard("d30")];

it("无数据时渲染加载态，不出卡片", () => {
  const html = renderToStaticMarkup(
    <OverviewPage strings={strings} disabledTools={[]} scanVersion={0} />,
  );

  expect(html).toContain(strings.overviewLoading);
  expect(html).not.toContain(strings.cardTotals);
});

it("注入仪表盘后：筛选条与九张卡齐现", () => {
  const html = renderToStaticMarkup(
    <OverviewPage
      strings={strings}
      disabledTools={[]}
      scanVersion={0}
      initialDashboard={SEAM}
    />,
  );

  expect(html).toContain(strings.overviewTotalTokens);
  expect(html).toContain(strings.cardTotals);
  expect(html).toContain(strings.cardUsageTrend);
  expect(html).toContain(strings.cardComposition);
  expect(html).toContain(strings.cardSankey);
  expect(html).toContain(strings.cardHeatmap);
  expect(html).toContain(strings.cardSpread);
  expect(html).toContain(strings.cardCombo);
  expect(html).toContain(strings.cardModelEfficiency);
  expect(html).toContain(strings.cardCostWaterfall);
  // 未定价模型注记由构成卡透出（mock 数据带未定价模型）。
  expect(html).toContain(
    strings.unknownModelsNote.replace("{n}", String(data.unknownModelCount)),
  );
});
