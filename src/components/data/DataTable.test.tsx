/**
 * 行高令牌回归测试：td 是行高的实际决定者（表格里 height 是最小值语义），
 * 必须随 compact 切换令牌，否则 compact 调什么都不生效——这是修过的 bug。
 */

import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { DataTable, type DataTableColumn } from "./DataTable";

interface Row {
  readonly id: string;
  readonly name: string;
}

const columns: readonly DataTableColumn<Row>[] = [
  { key: "name", header: "Name", render: (row) => row.name },
];

const rows: readonly Row[] = [{ id: "r1", name: "第一行" }];

it("普通模式 td 与 tr 都用普通行高令牌", () => {
  const html = renderToStaticMarkup(
    <DataTable columns={columns} rows={rows} rowKey={(row) => row.id} />,
  );

  expect(html).toContain("h-[var(--tt-row-h)]");
  expect(html).not.toContain("h-[var(--tt-row-h-compact)]");
});

it("compact 模式 td 与 tr 都用紧凑令牌，内容包裁剪层", () => {
  const html = renderToStaticMarkup(
    <DataTable columns={columns} rows={rows} rowKey={(row) => row.id} compact />,
  );

  expect(html).toContain("h-[var(--tt-row-h-compact)]");
  expect(html).toContain("max-h-[var(--tt-row-h-compact)] overflow-hidden");
});
