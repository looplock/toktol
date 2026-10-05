/**
 * ScanToast 渲染断言（react-dom/server，与项目其余组件测试同一取舍）：
 * 三态图标与标题、fixed 贴右下角、只读不吃指针事件。显示时长逻辑在
 * useScanToast，不在组件里——这里的 props 就是它的全部输入。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { ScanToast } from "./ScanToast";

it("进行中：图标旋转，状态对读屏软件可见", () => {
  const html = renderToStaticMarkup(<ScanToast kind="running" title="扫描中…" />);
  expect(html).toContain('data-scan-toast="running"');
  expect(html).toContain("animate-spin");
  expect(html).toContain('role="status"');
  expect(html).toContain('aria-live="polite"');
  expect(html).toContain("扫描中…");
});

it("完成：钉在视口右下角，条数副标题跟在标题之后", () => {
  const html = renderToStaticMarkup(
    <ScanToast kind="done" title="扫描完成" detail="新增 12 条记录" />,
  );
  expect(html).toContain("fixed");
  expect(html).toContain("right-4");
  expect(html).toContain("bottom-4");
  expect(html.indexOf("扫描完成")).toBeLessThan(html.indexOf("新增 12 条记录"));
});

it("失败：不再转圈，换成失败语义色", () => {
  const html = renderToStaticMarkup(<ScanToast kind="failed" title="扫描失败" />);
  expect(html).toContain('data-scan-toast="failed"');
  expect(html).toContain("text-danger");
  expect(html).not.toContain("animate-spin");
  expect(html).not.toContain("text-ok");
});

it("气泡只读：不劫持指针事件，右下角内容照旧可点可滚", () => {
  const html = renderToStaticMarkup(
    <ScanToast kind="done" title="扫描完成" detail="没有新记录" />,
  );
  expect(html).toContain("pointer-events-none");
});
