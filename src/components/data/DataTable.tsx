/**
 * 数据表格。排序是表头按钮的受控状态，但比较规则由列自己提供（sortValue）——
 * 表格不该知道某一列是时间还是金额。数字列右对齐并用 tabular-nums。
 */

import { useState, type ReactNode } from "react";

import { Card } from "../ui/Card";

export interface DataTableColumn<T> {
  readonly key: string;
  readonly header: string;
  readonly numeric?: boolean;
  readonly width?: string;
  readonly sortValue?: (row: T) => number | string;
  /** 显式声明可排序：排序由服务端执行时没有 sortValue，靠它点亮表头排序按钮。 */
  readonly sortable?: boolean;
  readonly render: (row: T) => ReactNode;
  /** 横向滚动时钉在左侧（如时间列）。单元格需不透明底色，否则滚动内容会透出来。 */
  readonly sticky?: boolean;
  /** 附加到表头单元格：列宽用 ch 精确适配等宽内容时，靠它让 th 的字体与内容一致。 */
  readonly headerClassName?: string;
}

export interface DataTableSort {
  readonly key: string;
  readonly desc: boolean;
}

interface DataTableProps<T> {
  readonly columns: readonly DataTableColumn<T>[];
  readonly rows: readonly T[];
  readonly rowKey: (row: T) => string;
  readonly caption?: string;
  readonly empty?: ReactNode;
  readonly compact?: boolean;
  /** 附加到根容器，调用方做弹性布局时用。 */
  readonly className?: string;
  /** 表格最小宽度已不需要：自动布局下列宽下限 = 内容宽（不折行），窄窗口自动横向滚动。 */
  /** 卡片内、滚动区下方的页脚（如分页条），随卡片 padding 收边。 */
  readonly footer?: ReactNode;
  /** 传值即进入受控模式（排序要与分页配合、对全量数据生效时用）；不传则组件内部自管。 */
  readonly sort?: DataTableSort | null;
  readonly onSortChange?: (sort: DataTableSort) => void;
  /** 排序执行方：server = 排序已由数据层做完，本组件只画表头状态、不再重排当页
   * 行（分页在服务端时本地重排会把页内顺序打乱成假全序）。 */
  readonly sortMode?: "client" | "server";
}

export function DataTable<T>({
  columns,
  rows,
  rowKey,
  caption,
  empty,
  compact = false,
  className,
  footer,
  sort: controlledSort,
  onSortChange,
  sortMode = "client",
}: DataTableProps<T>) {
  const [internalSort, setInternalSort] = useState<DataTableSort | null>(null);
  const sort = controlledSort !== undefined ? controlledSort : internalSort;
  const serverSorted = sortMode === "server";

  if (rows.length === 0) {
    return <Card padding="none">{empty ?? null}</Card>;
  }

  const active =
    sort === null ? undefined : columns.find((column) => column.key === sort.key);

  const sorted =
    serverSorted || sort === null || active?.sortValue === undefined
      ? rows
      : [...rows].sort((a, b) => {
          const left = active.sortValue?.(a);
          const right = active.sortValue?.(b);

          if (left === undefined || right === undefined) return 0;

          const cmp =
            typeof left === "number" && typeof right === "number"
              ? left - right
              : String(left).localeCompare(String(right));

          return sort.desc ? -cmp : cmp;
        });

  function toggle(key: string): void {
    const next: DataTableSort =
      sort?.key === key ? { key, desc: !sort.desc } : { key, desc: true };

    if (onSortChange) {
      onSortChange(next);
    } else {
      setInternalSort(next);
    }
  }

  return (
    /* 卡片层带 padding、滚动在内层：留白圈在滚动区外，横向滚动时内容不会
     * 穿到 padding 里，固定列也始终钉在内层左缘（贴着留白内侧）。
     * 壳走 Card（raised），padding 特殊：底部由 footer 自带、没有 footer 的
     * 表格底边贴边，所以不用默认 p-5 而是自带 px-5 pt-5 pb-0。 */
    <Card raised className={`flex flex-col px-5 pt-5 pb-0 ${className ?? ""}`}>
      <div className="min-h-0 flex-1 overflow-auto">
        {/* 自动布局：列宽由内容决定（不折行的单元格 = 列宽下限），窗口变窄自动
         * 横向滚动、全屏多余空间按内容比例分摊——不需要手工钉宽度。 */}
        <table className="w-full border-collapse text-sm">
          {caption === undefined ? null : (
            <caption className="sr-only">{caption}</caption>
          )}

          <thead className="sticky top-0 z-30 bg-surface-raised">
            <tr>
              {columns.map((column) => {
                const sortable =
                  column.sortValue !== undefined || column.sortable === true;
                const isActive = sort?.key === column.key;

                return (
                  <th
                    key={column.key}
                    scope="col"
                    style={
                      column.width === undefined
                        ? undefined
                        : { width: column.width }
                    }
                    aria-sort={
                      !isActive
                        ? "none"
                        : sort?.desc === true
                          ? "descending"
                          : "ascending"
                    }
                    // 下缘分割线用内阴影：collapse 模式下 border 属于表格边框层，sticky
                    // 表头滚动时会把它丢在原地；box-shadow 跟着元素走。
                    // 右缘竖线只有固定列有（时间列），其余列之间不要线。
                    // 表头不折行：窄窗口下"耗时"这类两字表头会被压成竖排，
                    // nowrap 同时把列宽下限顶到表头文字宽。
                    className={`whitespace-nowrap px-3 py-2.5 text-left text-xs font-normal text-ink-muted ${
                      column.numeric === true ? "text-right" : ""
                    } ${
                      column.sticky === true
                        ? "sticky top-0 left-0 z-20 bg-surface-raised shadow-[inset_0_-1px_0_0_var(--tt-border),inset_-1px_0_0_0_var(--tt-border)]"
                        : "shadow-[inset_0_-1px_0_0_var(--tt-border)]"
                    }`}
                  >
                    {sortable ? (
                      <button
                        type="button"
                        onClick={() => toggle(column.key)}
                        className="inline-flex cursor-pointer items-center gap-1 hover:text-ink"
                      >
                        {column.header}
                        <span
                          aria-hidden="true"
                          // 激活的箭头带色（降序绿 / 升序红），未激活保持弱化灰。
                          className={`text-[10px] leading-none ${
                            !isActive
                              ? "text-ink-muted"
                              : sort?.desc === true
                                ? "text-down"
                                : "text-up"
                          }`}
                        >
                          {!isActive ? "↕" : sort?.desc === true ? "↓" : "↑"}
                        </span>
                      </button>
                    ) : (
                      column.header
                    )}
                  </th>
                );
              })}
            </tr>
          </thead>

          <tbody>
            {sorted.map((row) => (
              <tr
                key={rowKey(row)}
                // 行悬浮用不透明的 row-hover 令牌：半透明色会让固定列下面的滚动内容透出来。
                // transition-colors 让高亮淡入淡出，时长与曲线吃全局令牌（180ms）。
                className={`group border-b border-border/50 last:border-0 transition-colors hover:bg-row-hover ${
                  compact
                    ? "h-[var(--tt-row-h-compact)]"
                    : "h-[var(--tt-row-h)]"
                }`}
              >
                {columns.map((column) => (
                  <td
                    key={column.key}
                    // 行高同时在 tr 与 td 上钉一遍：个别表格实现里 tr 的 height 只当建议，
                    // td 兜底保证行高压得住内容。td 是行高的实际决定者（表格里 height 都是
                    // 最小值语义，行高被最高的 td 顶住），必须与 tr 用同一个变量，否则
                    // compact 令牌调什么都不生效。
                    className={`${
                      compact
                        ? "h-[var(--tt-row-h-compact)]"
                        : "h-[var(--tt-row-h)]"
                    } px-3 transition-colors ${column.numeric === true ? "text-right tabular-nums" : ""} ${
                      // 固定列盖在滚动内容之上，底色不透明；hover 用 group 跟着整行走。
                      column.sticky === true
                        ? "sticky left-0 z-10 bg-surface-raised shadow-[inset_-1px_0_0_0_var(--tt-border)] group-hover:bg-row-hover"
                        : ""
                    }`}
                  >
                    {compact ? (
                      // compact 是单行密度：超出令牌高度的内容（如双行 tokens 格）裁掉，
                      // 行高才压得到 32px——table cell 自己不吃 max-height，得包一层。
                      <div className="max-h-[var(--tt-row-h-compact)] overflow-hidden">
                        {column.render(row)}
                      </div>
                    ) : (
                      column.render(row)
                    )}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {footer === undefined ? null : (
        // footer 在滚动区外，border-t 天然常驻；与表头那根线形成首尾呼应。
        // 分割线必须贴住滚动区底缘：这里不能再留 margin——margin 在滚动区外面，
        // 露出的是卡片白底，内容在缘上被裁掉后看起来就像被白条盖住一层。
        // 呼吸感交给 py-2：只撑在分割线以下（分页控件与线之间）。
        <div className="shrink-0 border-t border-border py-2">
          {footer}
        </div>
      )}
    </Card>
  );
}
