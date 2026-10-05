/**
 * ScanButton 渲染断言（react-dom/server，与项目其余组件测试同一取舍）：
 * 空闲可点、扫描中禁点、提示与错误进 title。点击行为走的是
 * useScanScheduler.triggerManual，逻辑在 hook 侧。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { ScanButton } from "./ScanButton";

it("空闲：不禁点，提示为扫描标签", () => {
  const html = renderToStaticMarkup(
    <ScanButton scanning={false} label="重新扫描" scanningLabel="扫描中…" onClick={() => {}} />,
  );
  expect(html).toContain("重新扫描");
  expect(html).not.toContain("disabled=\"\""); // 类名里的 disabled:* 工具类不算禁点。
});

it("扫描中：禁点，title 换成扫描中提示", () => {
  const html = renderToStaticMarkup(
    <ScanButton scanning label="重新扫描" scanningLabel="扫描中…" onClick={() => {}} />,
  );
  expect(html).toContain("disabled");
  expect(html).toContain("扫描中…");
  expect(html).toContain("animate-spin");
});

it("失败信息进 title，用户能从悬停提示看到自动扫描出过事", () => {
  const html = renderToStaticMarkup(
    <ScanButton
      scanning={false}
      label="重新扫描"
      scanningLabel="扫描中…"
      error="db locked"
      onClick={() => {}}
    />,
  );
  expect(html).toContain("db locked");
});
