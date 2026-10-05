/**
 * 不支持态占位（SSR 直渲，与组件测试同一取舍）：镜片里必须是会话真实的
 * token 数、标题与副文案两行都在、标记属性供详情页测试定位。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { DEFAULT_NUMBER_UNIT } from "../../lib/numberUnit";
import { stringsFor } from "../../i18n/strings";
import { UnsupportedTranscript } from "./UnsupportedTranscript";

const strings = stringsFor("zh-CN");

it("镜片里是本会话真实的 token 数，主副文案齐全", () => {
  const html = renderToStaticMarkup(
    <UnsupportedTranscript
      tokens={143_600}
      numberUnit={DEFAULT_NUMBER_UNIT}
      strings={strings}
    />,
  );
  expect(html).toContain('data-transcript-unsupported');
  expect(html).toContain("143.6K");
  expect(html).toContain(strings.transcriptUnsupportedTitle);
  expect(html).toContain(strings.transcriptUnsupportedBody);
  expect(html.indexOf("143.6K")).toBeLessThan(html.indexOf(strings.transcriptUnsupportedTitle));
});
