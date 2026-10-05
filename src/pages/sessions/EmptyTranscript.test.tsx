/**
 * 空转录占位（SSR 直渲）：空壳会话（0 请求 / 0 token）走「对话从未开始」
 * 光标插画；有用量但日志不带消息的（如 dsh）保持一行朴素说明。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import type { SessionRow } from "../../lib/api";
import { stringsFor } from "../../i18n/strings";
import { EmptyTranscript } from "./EmptyTranscript";

const strings = stringsFor("zh-CN");

const shell: SessionRow = {
  id: 1,
  tool: "opencode",
  externalId: "ses_x",
  title: "New session",
  projectDir: null,
  lastActivityAt: 1_789_200_645_293,
  requestCount: 0,
  inputTokens: 0,
  outputTokens: 0,
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
};

it("空壳会话：光标插画 + 主副文案，不出有量态文案", () => {
  const html = renderToStaticMarkup(
    <EmptyTranscript row={shell} strings={strings} />,
  );
  expect(html).toContain('data-transcript-empty');
  expect(html).toContain("tt-blink");
  expect(html).toContain(strings.transcriptEmptyTitle);
  expect(html).toContain(strings.transcriptEmptyBody);
  expect(html).not.toContain(strings.transcriptEmpty);
});

it("有用量但日志不带消息：保持一行朴素说明", () => {
  const html = renderToStaticMarkup(
    <EmptyTranscript
      row={{ ...shell, requestCount: 32, inputTokens: 5_000, outputTokens: 200 }}
      strings={strings}
    />,
  );
  expect(html).toContain(strings.transcriptEmpty);
  expect(html).not.toContain(strings.transcriptEmptyTitle);
});
