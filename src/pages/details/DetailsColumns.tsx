/**
 * 明细页表格的列定义与单元格渲染（时间/工具/模型/会话/tokens/费用）。
 * buildColumns 在组件外：跳会话页的回调经参数传入（setState 引用稳定）。
 */

import type { ReactNode } from "react";
import { Badge } from "../../components/ui/Badge";
import { BrandIcon } from "../../components/ui/BrandIcon";
import type { DataTableColumn } from "../../components/data/DataTable";
import { InfoTooltip } from "../../components/ui/InfoTooltip";
import { CompositionPanel } from "../../components/ui/CompositionPanel";
import type { Strings } from "../../i18n/strings";
import type { UsageRecordRow } from "../../lib/api";
import type { InputScope } from "../../lib/inputScope";
import { toolIcon, modelIcon, type BrandIconAsset } from "../../lib/brandIcons";
import { toolLabel } from "../../lib/tools";
import {
  formatCostMicros,
  formatCostMicrosExact,
  formatTimestamp,
  formatTokens,
} from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import type { SessionFocus } from "../../lib/routes";

export function buildColumns(
  strings: Strings,
  inputScope: InputScope,
  onOpenSession: (focus: SessionFocus) => void,
): DataTableColumn<UsageRecordRow>[] {
  // 自动布局：不钉任何宽度，列宽 = 内容宽（胶囊/时间戳/UUID 都不折行），
  // 窄窗口整体横向滚动、全屏按内容比例摊多余空间。
  // key 与服务端排序白名单一一对应（storage::usage_sort_expr）。
  return [
    {
      key: "ts",
      header: strings.detailsColTime,
      sortable: true,
      // 经典的 width:1% 技巧：auto 布局下配合 nowrap，这一列永远收缩到恰好内容宽
      // （不能更窄），全屏多出来的空间全让给其他列——时间列右侧不会再有空白。
      width: "1%",
      sticky: true,
      render: (row) => (
        // nowrap：时间戳一旦折行，固定列就变两行高，整行全被撑高。
        // 13px：会话列同尺寸；颜色用最强墨色，不加字重。
        <span className="whitespace-nowrap font-mono text-[13px] tabular-nums text-ink-strong">
          {formatTimestamp(row.ts)}
        </span>
      ),
    },
    {
      key: "tool",
      header: strings.detailsColTool,
      sortable: true,
      render: (row) => (
        // 显示友好名（"Claude Code"）；完整 id 留在 title 里（悬停可查，筛选值不变）。
        <Tag icon={toolIcon(row.tool)} title={row.tool}>
          {toolLabel(row.tool)}
        </Tag>
      ),
    },
    {
      key: "model",
      header: strings.detailsColModel,
      sortable: true,
      render: (row) => (
        <Tag icon={modelIcon(row.model)} title={row.model}>
          <span className="font-mono">{row.model}</span>
        </Tag>
      ),
    },
    {
      key: "project",
      header: strings.detailsColProject,
      sortable: true,
      render: (row) =>
        row.projectDir === null ? (
          <span className="text-ink-muted">—</span>
        ) : (
          <span
            className="block whitespace-nowrap text-ink-strong"
            title={row.projectDir}
          >
            {row.projectDir}
          </span>
        ),
    },
    {
      key: "session",
      header: strings.detailsColSession,
      sortable: true,
      render: (row) =>
        row.sessionExternalId === null ? (
          <span className="font-mono text-ink-muted">—</span>
        ) : (
          <button
            type="button"
            onClick={() =>
              onOpenSession({
                tool: row.tool,
                externalId: row.sessionExternalId ?? "",
              })
            }
            title={`${strings.transcriptOpenHint} · ${row.sessionExternalId}`}
            className="block max-w-60 truncate whitespace-nowrap font-mono text-[13px] text-ink-strong tabular-nums underline decoration-transparent underline-offset-2 transition-colors hover:decoration-current"
          >
            {row.sessionExternalId}
          </button>
        ),
    },
    {
      key: "tokens",
      header: strings.detailsColTokens,
      sortable: true,
      // 四个桶合成一个单元格，排序只剩一个维度：按四桶合计（量级）排。
      render: (row) => (
        <TokenBuckets row={row} strings={strings} inputScope={inputScope} />
      ),
    },
    {
      key: "duration",
      header: strings.detailsColDuration,
      sortable: true,
      render: (row) =>
        row.durationMs === null ? (
          <span className="font-mono text-ink-muted">–</span>
        ) : (
          <span className="font-mono tabular-nums">
            {Math.round(row.durationMs / 1000)}s
          </span>
        ),
    },
    {
      key: "cost",
      header: strings.detailsColCost,
      sortable: true,
      // 左对齐（用户要求），不做数字右对齐。
      render: (row) => {
        if (row.costMicros === null) {
          return <Badge tone="unknown">{strings.detailsCostUnknown}</Badge>;
        }
        return (
          <span className="inline-flex items-center gap-1.5">
            {/* 金额胶囊：绿色等宽全精度，无底色只留描边；ⓘ 悬停看四桶费用构成。 */}
            <span className="inline-flex items-center rounded-full border border-border px-2.5 py-0.5 font-mono text-sm text-down tabular-nums whitespace-nowrap">
              {formatCostMicrosExact(row.costMicros)}
            </span>
            <InfoTooltip
              label={strings.detailsCostHint}
              content={
                <CompositionPanel
                  parts={costParts(row, strings)}
                  total={row.costMicros}
                  totalLabel={strings.detailsCostTotal}
                  format={formatCostMicrosExact}
                  centerFormat={formatCostMicros}
                />
              }
            />
          </span>
        );
      },
    },
  ];
}

/**
 * 四桶费用构成（费用列 tooltip 用）。分桶费用由 IPC 直出（视图口径），不再由
 * 前端按单价推导；已知总分 ⇔ 所有有量桶已定价，`?? 0` 只是给类型一个交代
 * （零量桶费用恒为 0，会被构成环的 value>0 过滤掉）。
 */
function costParts(
  row: UsageRecordRow,
  strings: Strings,
): readonly { label: string; value: number; token: string }[] {
  return [
    {
      label: strings.detailsColInput,
      value: row.inputCostMicros ?? 0,
      token: "--tt-bucket-in",
    },
    {
      label: strings.detailsColOutput,
      value: row.outputCostMicros ?? 0,
      token: "--tt-bucket-out",
    },
    {
      label: strings.detailsColCacheRead,
      value: row.cacheReadCostMicros ?? 0,
      token: "--tt-bucket-read",
    },
    {
      label: strings.detailsColCacheWrite,
      value: row.cacheWriteCostMicros ?? 0,
      token: "--tt-bucket-write",
    },
  ];
}

/**
 * 胶囊标签：品牌图标 + 名字。字号固定 text-sm（14px）＝时间列（13px）+1px。
 * 列宽收紧后名字可能放不下：内容 truncate，完整文字进 title。
 * 图标取不到（未知厂商/工具）就只留文字，不占位留空洞。
 */
function Tag({
  icon,
  title,
  children,
}: {
  readonly icon: BrandIconAsset | undefined;
  readonly title: string;
  readonly children: ReactNode;
}) {
  return (
    <span
      title={title}
      className="inline-flex items-center gap-1.5 whitespace-nowrap rounded-full bg-surface-subtle px-2.5 py-1 text-sm text-ink-strong"
    >
      {icon === undefined ? null : <BrandIcon icon={icon} size={16} />}
      {children}
    </span>
  );
}

/** 裸箭头（↓/↑）：输入/输出桶专用。与缓存桶的落线箭头（ArrowToLineIcon）
 * 区分——带不带底杠就是"原始量 vs 缓存量"的形状编码，高对比模式下颜色
 * 全部退化成同色时，靠这个形状差异仍能读出桶的类别。 */
function ArrowIcon({
  up = false,
  className,
}: {
  up?: boolean;
  className: string;
}) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      {up ? <path d="M12 18V6" /> : <path d="M12 6v12" />}
      {up ? <path d="m6 12 6-6 6 6" /> : <path d="m6 12 6 6 6-6" />}
    </svg>
  );
}

/** 落线箭头（↓/↑ 带底杠，下载/上传隐喻）：缓存读/写专用，与首行的裸箭头区分。 */
function ArrowToLineIcon({
  up = false,
  className,
}: {
  up?: boolean;
  className: string;
}) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      {up ? <path d="m18 9-6-6-6 6" /> : <path d="M12 17V3" />}
      {up ? <path d="M12 3v18" /> : <path d="m6 11 6 6 6-6" />}
      <path d="M19 21H5" />
    </svg>
  );
}

/**
 * Tokens 单元格，两行排布（对齐样式参考）：
 *   首行 ↓输入 ↑输出 —— 裸箭头绿/紫；
 *   次行 缓存读 / 缓存写 —— 落线箭头（下载/上传隐喻）蓝/橙，弱化色。
 * 两行同为 13px，只靠颜色分层。箭头配色取自四桶专用令牌（冷热分组）：
 * 输入=橙、输出=玫红（暖，新生成要花钱），缓存读=灰蓝、缓存写=青（冷，缓存
 * 复用省钱），与涨跌色和分类色完全脱钩。右侧 info 图标悬停弹出构成环
 * tooltip（CompositionPanel），配色与这里的箭头一一对应。
 * 输入口径（设置页偏好）：含缓存读时输入行数值并入缓存读（色球拼色标记），
 * 缓存读行保持同级、灰蓝色球照常单列——它只是输入的明细回显，环里不再
 * 重复画弧（值已在输入弧内）。
 */
function TokenBuckets({
  row,
  strings,
  inputScope,
}: {
  readonly row: UsageRecordRow;
  readonly strings: Strings;
  readonly inputScope: InputScope;
}) {
  const withCacheRead = inputScope === "inputWithCacheRead";
  const shownInput = withCacheRead
    ? row.inputTokens + row.cacheReadTokens
    : row.inputTokens;
  const numberUnit = useNumberUnit();
  // 缓存率 = 缓存读 / 四桶总量；没有缓存活动就不显示这行小字（0% 没有信息量）。
  const totalTokens =
    row.inputTokens + row.outputTokens + row.cacheReadTokens + row.cacheWriteTokens;
  const ringCaption =
    row.cacheReadTokens > 0 && totalTokens > 0
      ? `${strings.detailsCacheRate} ${((row.cacheReadTokens / totalTokens) * 100).toFixed(1)}%`
      : undefined;

  return (
    <div className="flex items-center gap-2">
      <div className="flex flex-col gap-1 font-mono tabular-nums">
        {/* 两行同为 13px（与时间列一致），只靠颜色分层：首行 ink、次行弱化灰。
            箭头统一放 14px 定宽容器居中，两行的图标和数字才能纵向对齐。 */}
        <div className="flex items-center gap-4 text-[13px]">
          <span className="shrink-0 whitespace-nowrap tabular-nums">
            <span
              aria-hidden="true"
              className="mr-1 inline-flex w-3.5 items-center justify-center align-[-2px] text-bucket-in"
            >
              <ArrowIcon className="size-3.5" />
            </span>
            {formatTokens(shownInput, numberUnit)}
          </span>
          <span className="shrink-0 whitespace-nowrap tabular-nums">
            <span
              aria-hidden="true"
              className="mr-1 inline-flex w-3.5 items-center justify-center align-[-2px] text-bucket-out"
            >
              <ArrowIcon up className="size-3.5" />
            </span>
            {formatTokens(row.outputTokens, numberUnit)}
          </span>
        </div>
        {/* 缓存桶为零就不渲染：孤零零的"0"没有信息量，tooltip 的环与图例同步隐藏。
            含缓存读口径只改首行输入的数字，缓存读/写两桶照常单列。 */}
        {(row.cacheReadTokens > 0 || row.cacheWriteTokens > 0) && (
          <div className="flex items-center gap-4 text-[13px] text-ink-muted/70">
            {row.cacheReadTokens > 0 && (
              <span className="shrink-0 whitespace-nowrap tabular-nums">
                <span
                  aria-hidden="true"
                  className="mr-1 inline-flex w-3.5 items-center justify-center align-[-2px] text-bucket-read opacity-60"
                >
                  <ArrowToLineIcon className="size-3.5" />
                </span>
                {formatTokens(row.cacheReadTokens, numberUnit)}
              </span>
            )}
            {row.cacheWriteTokens > 0 && (
              <span className="shrink-0 whitespace-nowrap tabular-nums">
                <span
                  aria-hidden="true"
                  className="mr-1 inline-flex w-3.5 items-center justify-center align-[-2px] text-bucket-write opacity-60"
                >
                  <ArrowToLineIcon up className="size-3.5" />
                </span>
                {formatTokens(row.cacheWriteTokens, numberUnit)}
              </span>
            )}
          </div>
        )}
      </div>
      <InfoTooltip
        content={
          <CompositionPanel
            parts={[
              {
                label: strings.detailsColInput,
                // 含缓存读口径下与单元格首行同数字，色球拼进缓存读的颜色对上单元格。
                value: withCacheRead ? shownInput : row.inputTokens,
                token: "--tt-bucket-in",
                ...(withCacheRead ? { secondaryToken: "--tt-bucket-read" } : {}),
              },
              {
                label: strings.detailsColOutput,
                value: row.outputTokens,
                token: "--tt-bucket-out",
              },
              {
                label: strings.detailsColCacheRead,
                value: row.cacheReadTokens,
                token: "--tt-bucket-read",
                // 含缓存读口径下这行只是输入的明细回显，值已在输入弧里，
                // 不进环，否则同一份量画两遍弧。
                ...(withCacheRead ? { legendOnly: true } : {}),
              },
              {
                label: strings.detailsColCacheWrite,
                value: row.cacheWriteTokens,
                token: "--tt-bucket-write",
              },
            ]}
            total={totalTokens}
            totalLabel={strings.detailsTokensTotal}
            format={(value) => formatTokens(value, numberUnit)}
            {...(ringCaption === undefined ? {} : { ringCaption })}
          />
        }
        label={strings.detailsTokensHint}
      />
    </div>
  );
}
