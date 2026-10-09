/**
 * 明细页：逐条用量记录的请求级表格。数据走真实 IPC（usage_records_page）：
 * 筛选、排序、分页全部在服务端执行（行数无上界，不整表拉回），本页只拼查询、
 * 画状态。搜索词本地防抖 200ms，其余筛选即时生效。
 * 布局占满视口：表格区 flex 撑满、内部滚动，整页不滚。
 * 列定义与单元格渲染拆在 details/DetailsColumns.tsx。
 */

import { useEffect, useMemo, useState } from "react";
import {
  fetchUsageFilterOptions,
  fetchUsageRecords,
  type UsageFilters,
  type UsageRecordRow,
} from "../lib/api";
import {
  DataTable,
  type DataTableColumn,
  type DataTableSort,
} from "../components/data/DataTable";
import { type TimeSelection } from "../components/data/DateRangePicker";
import {
  FilterToolbar,
  type FilterDimension,
} from "../components/data/FilterToolbar";
import { type MultiSelectOption } from "../components/data/MultiSelect";
import { PageShell } from "../components/PageShell";
import { Pagination } from "../components/data/Pagination";
import type { Strings } from "../i18n/strings";
import type { InputScope } from "../lib/inputScope";
import { useInvokeQuery } from "../lib/hooks/useInvokeQuery";
import { modelIcon, toolIcon, type BrandIconAsset } from "../lib/brandIcons";
import { defaultTimeSelection, timePresetsOf, toggleValue, weekdaysOf } from "../lib/filters";
import type { SessionFocus } from "../lib/routes";
import { toolLabel } from "../lib/tools";
import { buildColumns } from "./details/DetailsColumns";

const PAGE_SIZE_OPTIONS = [20, 50, 100, 200] as const;
const DEFAULT_PAGE_SIZE = 50;
const SEARCH_DEBOUNCE_MS = 200;

interface DetailsPageProps {
  readonly strings: Strings;
  /** 被禁用的工具 id（设置页偏好）：查询排除、选项剔除、已选中项摘除。 */
  readonly disabledTools: string[];
  /** 自动/手动扫描带来新数据时递增：主查询与选项都跟着重拉。 */
  readonly scanVersion: number;
  /** Tokens 列的输入口径（设置页"显示"区）。 */
  readonly inputScope: InputScope;
  /** 点会话列：跳到会话页选中该会话（详情在会话页看，本页不再弹窗）。 */
  readonly onOpenSession: (focus: SessionFocus) => void;
}

export function DetailsPage({
  strings,
  disabledTools,
  scanVersion,
  inputScope,
  onOpenSession,
}: DetailsPageProps) {
  // buildColumns 在组件外：跳会话页的回调经参数传入（setState 引用稳定）。
  const columns = useMemo<DataTableColumn<UsageRecordRow>[]>(
    () => buildColumns(strings, inputScope, onOpenSession),
    [strings, inputScope, onOpenSession],
  );

  // 筛选状态是页面本地的：只在明细页生效，切走即重置（不做全局/跨页持久化）。
  const [time, setTime] = useState<TimeSelection>(defaultTimeSelection);
  const [toolSel, setToolSel] = useState<ReadonlySet<string>>(new Set());
  const [modelSel, setModelSel] = useState<ReadonlySet<string>>(new Set());
  const [projectSel, setProjectSel] = useState<ReadonlySet<string>>(new Set());
  const [query, setQuery] = useState("");
  const [search, setSearch] = useState("");

  const [sort, setSort] = useState<DataTableSort | null>({
    key: "ts",
    desc: true,
  });
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(DEFAULT_PAGE_SIZE);

  // 搜索词防抖：会话 id / 项目目录都是长串，逐键直查浪费查询。
  useEffect(() => {
    const timer = window.setTimeout(
      () => setSearch(query.trim()),
      SEARCH_DEBOUNCE_MS,
    );
    return () => window.clearTimeout(timer);
  }, [query]);

  const filters = useMemo<UsageFilters>(
    () => ({
      timeStart: time.range?.start ?? null,
      timeEnd: time.range?.end ?? null,
      tools: [...toolSel],
      models: [...modelSel],
      projects: [...projectSel],
      search: search === "" ? null : search,
    }),
    [time, toolSel, modelSel, projectSel, search],
  );

  // 服务端排序（sortMode="server"）：排序键白名单在 Rust 侧，白名单外退回时间排序。
  // 页码钳位在 fetch 闭包里做（经 ref 取最新渲染的 data，等价于原先 effect
  // 时读 state）：deps 用 page，渲染层的 currentPage 钳位不变。
  const records = useInvokeQuery({
    deps: [filters, disabledTools, sort, page, pageSize, scanVersion],
    fetch: () => {
      const total = records.data?.total ?? 0;
      const current = Math.min(page, Math.max(1, Math.ceil(total / pageSize)));
      return fetchUsageRecords({
        filters,
        disabled: disabledTools,
        sortKey: sort?.key ?? null,
        sortDesc: sort?.desc ?? true,
        offset: (current - 1) * pageSize,
        limit: pageSize,
      });
    },
  });
  const pageData = records.data ?? { rows: [], total: 0 };
  const loading = records.loading;
  const loadError = records.error;
  // 渲染层的页码钳位：total 缩水后停在越界页时归位显示。
  const pageCount = Math.max(1, Math.ceil(pageData.total / pageSize));
  const currentPage = Math.min(page, pageCount);

  // 选项计数各一个装载实例：错误不渲染（选项失败静默不阻塞页面，主查询
  // 的错误另有提示）；分面口径——每个维度用其余筛选条件计数（后端拆分）。
  const facetQuery = useInvokeQuery({
    deps: [filters, disabledTools, scanVersion],
    fetch: () => fetchUsageFilterOptions(filters, disabledTools),
  });
  const options = facetQuery.data;

  // 被禁用的工具要从选择里摘掉：选项已不可见，留着等于永远少一块、解释不清的数据。
  useEffect(() => {
    setToolSel((current) => dropDisabled(current, disabledTools));
  }, [disabledTools]);

  // 任一筛选变化回到第一页：不然可能停在已超出范围的空页上。
  useEffect(() => {
    setPage(1);
  }, [filters]);

  const timePresets = useMemo(() => timePresetsOf(strings), [strings]);
  const weekdays = useMemo(() => weekdaysOf(strings), [strings]);

  const toolOptions = useMemo(
    () =>
      (options?.tools ?? []).map((option) =>
        toMultiOption(
          option,
          undefined,
          toolIcon(option.value ?? ""),
          // 工具显示友好名（"Claude Code"），值仍是工具 id（筛选语义不变）。
          toolLabel(option.value ?? ""),
        ),
      ),
    [options],
  );
  const modelOptions = useMemo(
    () =>
      (options?.models ?? []).map((option) =>
        toMultiOption(option, undefined, modelIcon(option.value ?? "")),
      ),
    [options],
  );
  const projectOptions = useMemo(
    () =>
      (options?.projects ?? []).map((option) => toMultiOption(option, strings)),
    [options, strings],
  );

  const dimensions = useMemo<FilterDimension[]>(
    () => [
      {
        id: "tool",
        label: strings.detailsColTool,
        options: toolOptions,
        selected: toolSel,
        onToggle: (value) =>
          setToolSel((current) => toggleValue(current, value)),
        onClear: () => setToolSel(new Set()),
      },
      {
        id: "model",
        label: strings.detailsColModel,
        options: modelOptions,
        selected: modelSel,
        onToggle: (value) =>
          setModelSel((current) => toggleValue(current, value)),
        onClear: () => setModelSel(new Set()),
      },
      {
        id: "project",
        label: strings.detailsColProject,
        options: projectOptions,
        selected: projectSel,
        onToggle: (value) =>
          setProjectSel((current) => toggleValue(current, value)),
        onClear: () => setProjectSel(new Set()),
      },
    ],
    [
      strings,
      toolOptions,
      modelOptions,
      projectOptions,
      toolSel,
      modelSel,
      projectSel,
    ],
  );

  function resetFilters(): void {
    setTime(defaultTimeSelection());
    setToolSel(new Set());
    setModelSel(new Set());
    setProjectSel(new Set());
    setQuery("");
  }

  function changeSort(next: DataTableSort): void {
    setSort(next);
    setPage(1);
  }

  function changePageSize(size: number): void {
    setPageSize(size);
    setPage(1);
  }

  const tableEmpty = (
    <div className="px-6 py-12 text-center text-sm text-ink-muted">
      {loadError !== null ? (
        <p>
          {strings.detailsLoadError}
          <span className="ml-2 font-mono text-xs">{loadError}</span>
        </p>
      ) : loading ? (
        <p>{strings.detailsLoading}</p>
      ) : (
        <>
          <p>{strings.detailsEmpty}</p>
          <p className="mt-1 text-xs">{strings.detailsEmptyHint}</p>
        </>
      )}
    </div>
  );

  return (
    <PageShell fill>
      <div className="shrink-0">
        <FilterToolbar
          time={time}
          onTimeChange={setTime}
          timePresets={timePresets}
          allTimeLabel={strings.rangeAll}
          customLabel={strings.filterCustomRange}
          startLabel={strings.filterStart}
          endLabel={strings.filterEnd}
          weekdays={weekdays}
          dimensions={dimensions}
          query={query}
          onQueryChange={setQuery}
          searchHint={strings.filterSearchHint}
          searchPlaceholder={strings.filterSearchOptions}
          clearLabel={strings.filterClear}
          noMatchLabel={strings.filterNoMatch}
          totalLabel={strings.paginationTotal.replace(
            "{total}",
            String(pageData.total),
          )}
          resetLabel={strings.resetFilters}
          onReset={resetFilters}
        />
      </div>
      <DataTable
        className={`min-h-0 flex-1 transition-opacity ${
          loading ? "opacity-60" : ""
        }`}
        sortMode="server"
        columns={columns}
        rows={pageData.rows}
        rowKey={(row) => String(row.id)}
        caption={strings.detailsTableCaption}
        sort={sort}
        onSortChange={changeSort}
        empty={tableEmpty}
        footer={
          <Pagination
            page={currentPage}
            pageCount={pageCount}
            total={pageData.total}
            pageSize={pageSize}
            pageSizeOptions={PAGE_SIZE_OPTIONS}
            labels={{
              nav: strings.paginationNav,
              first: strings.paginationFirst,
              prev: strings.paginationPrev,
              next: strings.paginationNext,
              last: strings.paginationLast,
              total: strings.paginationTotal,
              pageSize: strings.paginationPageSize,
              pageSizeUnit: strings.paginationPageSizeUnit,
              jump: strings.paginationJump,
            }}
            onPageChange={setPage}
            onPageSizeChange={changePageSize}
          />
        }
      />
    </PageShell>
  );
}

function toMultiOption(
  option: { readonly value: string | null; readonly count: number },
  strings?: Strings,
  icon?: BrandIconAsset,
  /** 显示名覆盖（工具维度传友好名）；缺省用原始值。 */
  label?: string,
): MultiSelectOption {
  return {
    value: option.value ?? "",
    label: label ?? option.value ?? strings?.filterNoProject ?? "",
    count: option.count,
    ...(icon === undefined ? {} : { icon }),
  };
}

/** 从选中集合里摘掉被禁用的工具。 */
function dropDisabled(
  selected: ReadonlySet<string>,
  disabled: readonly string[],
): ReadonlySet<string> {
  if (disabled.length === 0) {
    return selected;
  }
  const next = new Set(selected);
  for (const id of disabled) {
    next.delete(id);
  }
  return next;
}
