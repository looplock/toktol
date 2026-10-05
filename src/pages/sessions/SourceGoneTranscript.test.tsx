/**
 * 源消失态占位（SSR 直渲）：撕剩的半页上必须是该会话真实的请求数与
 * token 数，"对话正文"删除线行与主副文案齐全。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import type { SessionRow } from "../../lib/api";
import { DEFAULT_NUMBER_UNIT } from "../../lib/numberUnit";
import { stringsFor } from "../../i18n/strings";
import { SourceGoneTranscript } from "./SourceGoneTranscript";

const strings = stringsFor("zh-CN");

const row: SessionRow = {
  id: 1,
  tool: "zcode",
  externalId: "sess_x",
  title: "查看项目内容",
  projectDir: null,
  lastActivityAt: 1_789_200_645_293,
  requestCount: 300,
  inputTokens: 61_000_000,
  outputTokens: 1_100_000,
  cacheReadTokens: 11_000_000,
  cacheWriteTokens: 90_000,
};

it("半页清单是真实用量，缺失行与主副文案齐全", () => {
  const html = renderToStaticMarkup(
    <SourceGoneTranscript row={row} numberUnit={DEFAULT_NUMBER_UNIT} strings={strings} />,
  );
  expect(html).toContain('data-transcript-source-gone');
  expect(html).toContain("300 次请求");
  expect(html).toContain("73.19M");
  expect(html).toContain(strings.transcriptSourceGoneMissing);
  expect(html).toContain(strings.transcriptSourceGoneTitle);
  expect(html).toContain(strings.transcriptSourceGoneHint);
  // 正文行是删除线的"缺失"语义，不能排在清单之前。
  expect(html.indexOf("73.19M")).toBeLessThan(
    html.indexOf(strings.transcriptSourceGoneMissing),
  );
});

it("零用量的空壳会话交给空转录占位，不出撕页账单", () => {
  const html = renderToStaticMarkup(
    <SourceGoneTranscript
      row={{ ...row, requestCount: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 }}
      numberUnit={DEFAULT_NUMBER_UNIT}
      strings={strings}
    />,
  );
  expect(html).toContain("tt-blink");
  expect(html).toContain(strings.transcriptEmptyTitle);
  expect(html).not.toContain(strings.transcriptSourceGoneLedger);
});
