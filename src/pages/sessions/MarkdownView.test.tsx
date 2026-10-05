import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../../i18n/strings";
import { MarkdownView } from "./MarkdownView";

const strings = stringsFor();

it("MarkdownView：默认渲染 markdown，渲染/源码切换在位", () => {
  const html = renderToStaticMarkup(
    <MarkdownView text={"1. **加粗项**\n2. `code` 项"} strings={strings} />,
  );
  expect(html).toContain("<strong>加粗项</strong>");
  expect(html).toContain("<li");
  expect(html).toContain(strings.markdownRender);
  expect(html).toContain(strings.markdownSource);
  // 渲染口径下 markdown 标记不再以字面出现。
  expect(html).not.toContain("**加粗项**");
});
