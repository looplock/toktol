import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../../i18n/strings";
import type { SessionRow, TranscriptPageEntry, TranscriptTurn } from "../../lib/api";
import { SessionDetailView } from "./SessionDetailView";

// SSR 直渲：经 initialData 缝隙注入首帧转录（跳过 IPC），覆盖加载态与
// 就绪态（信息头 + 目录 + 消息流）。滚动联动与补页都在 effect 里，SSR 不跑。

const strings = stringsFor();

const noop = (): void => {};

function rowFixture(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    id: 7,
    tool: "claude-code",
    externalId: "sess-abc",
    title: "修复构建失败",
    projectDir: "D:\\Work\\Demo\\App",
    lastActivityAt: 1_760_000_000_000,
    requestCount: 12,
    inputTokens: 1000,
    outputTokens: 500,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    ...overrides,
  };
}

const TURNS: readonly TranscriptTurn[] = [
  { seq: 0, tsMs: 1_760_000_000_000, snippet: "帮我修这个 bug", parts: [], isSummary: false },
  { seq: 2, tsMs: 1_760_000_060_000, snippet: "已修好", parts: [], isSummary: false },
];

const ENTRIES: readonly TranscriptPageEntry[] = [
  {
    seq: 0,
    entry: {
      role: "user",
      tsMs: 1_760_000_000_000,
      model: null,
      blocks: [{ kind: "text", text: "帮我修这个 bug" }],
    },
  },
  {
    seq: 1,
    entry: {
      role: "assistant",
      tsMs: 1_760_000_060_000,
      model: "claude-sonnet-4-5",
      blocks: [{ kind: "text", text: "已修好" }],
    },
  },
];

it("无注入时渲染加载态，不出消息流", () => {
  const html = renderToStaticMarkup(
    <SessionDetailView row={rowFixture()} strings={strings} onBack={noop} />,
  );

  expect(html).toContain(strings.transcriptLoading);
  expect(html).not.toContain("帮我修这个 bug");
});

it("注入就绪数据：信息头、目录与消息流齐现", () => {
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={noop}
      initialData={{ turns: TURNS, total: 2, entries: ENTRIES }}
    />,
  );

  // 信息头：标题、坐标、聚合。
  expect(html).toContain("修复构建失败");
  expect(html).toContain("sess-abc");
  expect(html).toContain(strings.sessionsRequestsUnit);
  expect(html).toContain(strings.detailsColTokens);
  // 目录（默认列表视图）：两条轮次快照。
  expect(html).toContain(strings.sessionsViewList);
  expect(html).toContain("帮我修这个 bug");
  expect(html).toContain("已修好");
  // 消息流：角色标签与正文。
  expect(html).toContain(strings.transcriptRoleUser);
  expect(html).toContain(strings.transcriptRoleAssistant);
  expect(html).toContain("claude-sonnet-4-5");
});

it("embedded 模式隐藏返回按钮；标题缺省时显示未命名", () => {
  const embeddedHtml = renderToStaticMarkup(
    <SessionDetailView row={rowFixture()} strings={strings} onBack={noop} embedded />,
  );

  expect(embeddedHtml).not.toContain(strings.sessionsBack);

  const untitledHtml = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ title: null })}
      strings={strings}
      onBack={noop}
      initialData={{ turns: [], total: 0, entries: [] }}
    />,
  );

  expect(untitledHtml).toContain(strings.sessionsUntitled);
  expect(untitledHtml).toContain(strings.transcriptEmpty);
});
