import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import type {
  UsageFilterOptionsPayload,
  UsageRecordsPage,
} from "../lib/api";
import { DetailsPage } from "./Details";

// SSR 直渲：经 initialRecords / initialOptions 缝隙注入首帧，覆盖加载态、
// 空态与数据态；行内格式化细节归 lib/format 与 DetailsColumns 的单测。

const strings = stringsFor();

const noop = (): void => {};

function row(overrides: Partial<UsageRecordsPage["rows"][number]> = {}): UsageRecordsPage["rows"][number] {
  return {
    id: 1,
    ts: 1_760_000_000_000,
    tool: "claude-code",
    model: "claude-sonnet-4-5",
    sessionExternalId: "abc-123",
    projectDir: "D:\\Work\\Demo\\App",
    inputTokens: 1000,
    outputTokens: 500,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    reasoningTokens: null,
    durationMs: 1200,
    costMicros: 9000,
    inputCostMicros: 3000,
    outputCostMicros: 6000,
    cacheReadCostMicros: 0,
    cacheWriteCostMicros: 0,
    ...overrides,
  };
}

const RECORDS: UsageRecordsPage = { rows: [row()], total: 1 };

const OPTIONS: UsageFilterOptionsPayload = {
  tools: [{ value: "claude-code", count: 1 }],
  models: [{ value: "claude-sonnet-4-5", count: 1 }],
  projects: [{ value: "D:\\Work\\Demo\\App", count: 1 }],
};

it("无数据时渲染加载态；空表给出空态文案", () => {
  const html = renderToStaticMarkup(
    <DetailsPage
      strings={strings}
      disabledTools={[]}
      scanVersion={0}
      inputScope="inputWithCacheRead"
      onOpenSession={noop}
    />,
  );

  expect(html).toContain(strings.detailsLoading);
});

it("注入空行页：显示空态提示与 0 条总数", () => {
  const html = renderToStaticMarkup(
    <DetailsPage
      strings={strings}
      disabledTools={[]}
      scanVersion={0}
      inputScope="inputWithCacheRead"
      onOpenSession={noop}
      initialRecords={{ rows: [], total: 0 }}
      initialOptions={OPTIONS}
    />,
  );

  expect(html).toContain(strings.detailsEmpty);
  expect(html).toContain(strings.detailsEmptyHint);
  expect(html).toContain(strings.paginationTotal.replace("{total}", "0"));
});

it("注入数据行：行内容与总数渲染，工具列显示友好名", () => {
  const html = renderToStaticMarkup(
    <DetailsPage
      strings={strings}
      disabledTools={[]}
      scanVersion={0}
      inputScope="inputWithCacheRead"
      onOpenSession={noop}
      initialRecords={RECORDS}
      initialOptions={OPTIONS}
    />,
  );

  expect(html).toContain("abc-123");
  expect(html).toContain("claude-sonnet-4-5");
  expect(html).toContain(strings.paginationTotal.replace("{total}", "1"));
  expect(html).toContain(strings.detailsTableCaption);
  // 空态与加载态不再出现。
  expect(html).not.toContain(strings.detailsEmpty);
  expect(html).not.toContain(strings.detailsLoading);
});
