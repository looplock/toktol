/**
 * ConfirmDialog 渲染断言（react-dom/server，与项目其余组件测试同一取舍）：
 * 关闭不挂任何节点；打开时 alertdialog 语义齐全，标题/说明/两键进 DOM，
 * 确认键是危险色实体按钮；busy 时取消与确认一起禁点（结果未定不撤）。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { ConfirmDialog } from "./ConfirmDialog";

const base = {
  title: "删除会话？",
  body: "日志文件将移入系统回收站（可恢复），用量统计保留。",
  confirmLabel: "删除",
  cancelLabel: "取消",
  onConfirm: () => {},
  onCancel: () => {},
};

it("关闭：不渲染任何节点", () => {
  const html = renderToStaticMarkup(<ConfirmDialog {...base} open={false} />);
  expect(html).toBe("");
});

it("打开：alertdialog 语义齐全，遮罩与危险色确认键在位", () => {
  const html = renderToStaticMarkup(<ConfirmDialog {...base} open />);
  expect(html).toContain('role="alertdialog"');
  expect(html).toContain('aria-modal="true"');
  expect(html).toContain("删除会话？");
  expect(html).toContain("日志文件将移入系统回收站（可恢复），用量统计保留。");
  expect(html).toContain("取消");
  expect(html).toContain("bg-danger");
  // 遮罩压暗是 bg-ink/40 在全应用唯一的豁免用途。
  expect(html).toContain("bg-ink/40");
  // 非 busy：两个键都可点。
  expect(html).not.toContain('disabled=""');
});

it("busy：取消与确认都禁点（恰好两处）", () => {
  const html = renderToStaticMarkup(<ConfirmDialog {...base} open busy />);
  expect(html.split('disabled=""').length - 1).toBe(2);
});
