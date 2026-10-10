/**
 * 会话页：左列表卡 + 右详情卡的双卡布局（同网关页骨架）。数据走真实
 * IPC（sessions_page）：分页在服务端执行；点列表项在右卡切换详情
 * （时间线消息流，session_transcript 现读源日志），列表常驻不换页。
 * 列表卡头部是筛选条（搜索防抖 + 工具 chips，下推 filters）；行复选框
 * 悬停浮现、勾选行常驻，有勾选即出批量删除栏；行悬停另有单删垃圾桶
 * 按钮。两类删除都经同一轻确认弹层（ConfirmDialog，
 * delete_session / delete_sessions：文件进回收站，统计保留，部分失败只报
 * 计数），全部取消勾选后操作栏随之消失。
 * 详情视图与转录渲染拆在 sessions/ 子目录（SessionDetailView /
 * OutlineList / TranscriptStream / OriginalTextView）。
 */

import { useEffect, useMemo, useState } from "react";
import { Card } from "../components/ui/Card";
import { ConfirmDialog } from "../components/ui/ConfirmDialog";
import { PageShell } from "../components/PageShell";
import { Pagination } from "../components/data/Pagination";
import { MultiSelect } from "../components/data/MultiSelect";
import type { Strings } from "../i18n/strings";
import { ToolIcon } from "../components/ui/ToolIcon";
import { SearchIcon, TrashIcon } from "../components/ui/icons";
import { TOOL_ITEMS } from "../lib/tools";
import { toolIcon } from "../lib/brandIcons";
import {
  deleteSession,
  deleteSessions,
  fetchSessionsPage,
  type SessionFilters,
  type SessionRow,
  type SessionsPagePayload,
} from "../lib/api";
import { useInvokeQuery } from "../lib/hooks/useInvokeQuery";
import { formatTimestamp } from "../lib/format";
import { setPresence, toggleValue } from "../lib/sets";
import type { SessionFocus } from "../lib/routes";
import { SessionDetailView } from "./sessions/SessionDetailView";

/** 空筛选：全量会话按最近活跃排序（跨页焦点定位复用）。 */
const NO_FILTERS: SessionFilters = {
  timeStart: null,
  timeEnd: null,
  tools: [],
  search: null,
};

const DEFAULT_PAGE_SIZE = 24;

/** 搜索防抖：停顿后并入查询，避免每键一次 IPC。 */
const SEARCH_DEBOUNCE_MS = 300;

interface SessionsPageProps {
  readonly strings: Strings;
  /** 被禁用的工具 id（设置页偏好）：查询排除、选项剔除。 */
  readonly disabledTools: string[];
  /** 跨页焦点（明细页点会话跳过来）：按会话 id 搜索定位并选中。 */
  readonly focus?: SessionFocus | undefined;
  /** 焦点消费完置空（App 持有，避免再次进入时重复定位）。 */
  readonly onFocusConsumed?: (() => void) | undefined;
  /** 测试缝隙：注入首帧数据（SSR 不跑 effect，IPC 拉取自然被跳过）。 */
  readonly initialPageData?: SessionsPagePayload;
  /** 测试缝隙：预勾选的会话 id（交互态无法从 SSR 外部触达）。 */
  readonly initialCheckedIds?: readonly number[];
  /** 测试缝隙：预打开的删除确认弹层（单删指向会话 id，批删不指向）。 */
  readonly initialConfirm?:
    | { readonly kind: "one"; readonly sessionId: number }
    | { readonly kind: "batch" };
}

export function SessionsPage({
  strings,
  disabledTools,
  focus,
  onFocusConsumed,
  initialPageData,
  initialCheckedIds,
  initialConfirm,
}: SessionsPageProps) {
  // 详情视图是页面级状态：选中即"进入"，返回即置空，不进路由。
  const [selected, setSelected] = useState<SessionRow | null>(null);

  const [page, setPage] = useState(1);

  // 筛选态：工具集合 + 生效搜索词（searchInput 防抖后落入 search）。
  const [toolFilter, setToolFilter] = useState<ReadonlySet<string>>(new Set());
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState<string | null>(null);

  // 勾选集合 + 批量删除的进行中与结果反馈；无显式"多选模式"，
  // 有勾选即视为多选中（操作栏出现），清空即退出。
  const [checkedIds, setCheckedIds] = useState<ReadonlySet<number>>(
    new Set(initialCheckedIds ?? []),
  );
  const [deleting, setDeleting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  // 部分失败计数（数据库来源的会话不可删）；下次成功拉取时清掉（onSuccess）。
  const [deleteFailedCount, setDeleteFailedCount] = useState<number | null>(null);
  // 待确认的删除：单删指向会话 id，批删不指向；null = 没有弹层。
  // 删除语义（文件进回收站、统计保留）在弹层里说明，确认后才执行。
  const [confirm, setConfirm] = useState<
    | { readonly kind: "one"; readonly sessionId: number }
    | { readonly kind: "batch" }
    | null
  >(initialConfirm ?? null);

  useEffect(() => {
    const timer = setTimeout(() => {
      const trimmed = searchInput.trim();
      setSearch(trimmed === "" ? null : trimmed);
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchInput]);

  // 筛选变化回到第 1 页：结果集变了，旧页码多半越界。
  useEffect(() => {
    setPage(1);
  }, [toolFilter, search]);

  const filters: SessionFilters = useMemo(
    () => ({ timeStart: null, timeEnd: null, tools: [...toolFilter], search }),
    [toolFilter, search],
  );

  // 页码钳位在 fetch 闭包里做（经 ref 取最新渲染的 data，与 Details 同一套
  // 模式）：删除会话后 total 缩水，停在越界页会拉回空列表。渲染层的
  // currentPage 钳位不变。
  const sessionsQuery = useInvokeQuery({
    deps: [filters, disabledTools, page],
    fetch: () => {
      const total = sessionsQuery.data?.total ?? 0;
      const current = Math.min(page, Math.max(1, Math.ceil(total / DEFAULT_PAGE_SIZE)));
      return fetchSessionsPage({
        filters,
        disabled: disabledTools,
        sortKey: null,
        sortDesc: true,
        offset: (current - 1) * DEFAULT_PAGE_SIZE,
        limit: DEFAULT_PAGE_SIZE,
      });
    },
    initialData: initialPageData,
    onSuccess: () => setDeleteFailedCount(null),
  });
  const pageData = sessionsQuery.data ?? { rows: [], total: 0 };
  const loading = sessionsQuery.loading;
  const loadError = sessionsQuery.error;
  // 删除后手动刷新一次列表（翻页/筛选之外的失效源）。
  const refreshList = sessionsQuery.reload;

  // 跨页焦点：按会话 id 搜索定位（sessions_page 的 search 覆盖 external_id），
  // 找到即选中——目标不一定在当前分页页上，选中态不依赖列表可见。无论命中
  // 与否都消费掉焦点，失败时静默回退为"未选中"。
  useEffect(() => {
    if (focus === undefined) {
      return undefined;
    }
    let cancelled = false;
    fetchSessionsPage({
      filters: { ...NO_FILTERS, search: focus.externalId },
      disabled: disabledTools,
      sortKey: null,
      sortDesc: true,
      offset: 0,
      limit: DEFAULT_PAGE_SIZE,
    })
      .then((next) => {
        if (cancelled) return;
        const row = next.rows.find(
          (candidate) =>
            candidate.externalId === focus.externalId &&
            candidate.tool === focus.tool,
        );
        if (row !== undefined) {
          setSelected(row);
        }
      })
      .catch(() => {
        // 定位失败不报错：跳转本身已成功，只是没选中。
      })
      .finally(() => {
        if (!cancelled) onFocusConsumed?.();
      });
    return () => {
      cancelled = true;
    };
  }, [focus, disabledTools, onFocusConsumed]);

  const pageCount = Math.max(1, Math.ceil(pageData.total / DEFAULT_PAGE_SIZE));
  const currentPage = Math.min(page, pageCount);

  const chipTools = TOOL_ITEMS.filter((tool) => !disabledTools.includes(tool.id));
  const filterActive = toolFilter.size > 0 || search !== null;
  const allPageChecked =
    pageData.rows.length > 0 &&
    pageData.rows.every((row) => checkedIds.has(row.id));

  const toggleTool = (toolId: string) => {
    setToolFilter((prev) => toggleValue(prev, toolId));
  };

  const toggleChecked = (id: number) => {
    setCheckedIds((prev) => toggleValue(prev, id));
  };

  const togglePage = () => {
    setCheckedIds((prev) =>
      setPresence(
        prev,
        pageData.rows.map((row) => row.id),
        !allPageChecked,
      ),
    );
  };

  const clearChecked = () => {
    setCheckedIds(new Set());
    setDeleteError(null);
  };

  // 批量删除的执行体：确认弹层的确认键触发。部分失败只报计数；
  // 详情正展示被删会话时清掉（转录现读源日志，文件已进回收站）。
  const runBatchDelete = () => {
    if (deleting || checkedIds.size === 0) {
      return;
    }
    setDeleting(true);
    setDeleteError(null);
    deleteSessions([...checkedIds])
      .then((report) => {
        if (report.failed.length > 0) {
          setDeleteFailedCount(report.failed.length);
        }
        setSelected((prev) =>
          prev !== null && checkedIds.has(prev.id) ? null : prev,
        );
        clearChecked();
        refreshList();
      })
      .catch((err: unknown) => {
        setDeleteError(String(err));
      })
      .finally(() => {
        setDeleting(false);
        setConfirm(null);
      });
  };

  // 单会话删除：语义同批量（文件进回收站、统计保留），失败直接报错——
  // 单删没有"部分失败"形态。该行的勾选态一并摘除，弹层随结果关闭。
  const runOneDelete = (sessionId: number) => {
    if (deleting) {
      return;
    }
    setDeleting(true);
    setDeleteError(null);
    deleteSession(sessionId)
      .then(() => {
        setSelected((prev) =>
          prev !== null && prev.id === sessionId ? null : prev,
        );
        setCheckedIds((prev) =>
          prev.has(sessionId) ? toggleValue(prev, sessionId) : prev,
        );
        refreshList();
      })
      .catch((err: unknown) => {
        setDeleteError(String(err));
      })
      .finally(() => {
        setDeleting(false);
        setConfirm(null);
      });
  };

  return (
    <PageShell fill>
      <div className="grid min-h-0 flex-1 grid-cols-[340px_minmax(0,1fr)] gap-4">
        <Card raised padding="none" className="flex min-h-0 flex-col overflow-hidden">
            <div className="shrink-0 border-b border-border px-3 py-2">
              <div className="flex items-center gap-2">
                <div className="flex h-8 min-w-0 flex-1 items-center gap-1.5 rounded-control border border-border px-2 focus-within:border-ink-muted/60">
                  <SearchIcon className="size-3.5 shrink-0 text-ink-muted" />
                  <input
                    value={searchInput}
                    onChange={(event) => setSearchInput(event.target.value)}
                    placeholder={strings.sessionsSearchHint}
                    aria-label={strings.sessionsSearchHint}
                    className="h-full w-full min-w-0 bg-transparent text-xs text-ink placeholder:text-ink-muted/60 focus:outline-none focus-visible:outline-none"
                  />
                </div>
                <MultiSelect
                  bordered
                  label={strings.sessionsFilterTools}
                  options={chipTools.map((tool) => ({
                    value: tool.id,
                    label: tool.label,
                    icon: toolIcon(tool.id),
                  }))}
                  selected={toolFilter}
                  onToggle={toggleTool}
                  onClear={() => setToolFilter(new Set())}
                  searchPlaceholder={strings.filterSearchOptions}
                  clearLabel={strings.filterClear}
                  noMatchLabel={strings.filterNoMatch}
                />
              </div>
            </div>
          {deleteFailedCount !== null && (
            <p className="shrink-0 px-3 py-1.5 text-xs text-danger">
              {strings.sessionsDeleteFailed.replace("{n}", String(deleteFailedCount))}
            </p>
          )}
          {deleteError !== null && (
            <p className="shrink-0 truncate px-3 py-1.5 font-mono text-xs text-danger">
              {deleteError}
            </p>
          )}
          <div
            className={`min-h-0 flex-1 overflow-y-auto transition-opacity ${
              loading ? "opacity-60" : ""
            }`}
          >
            {loadError !== null ? (
              <p className="py-12 px-4 text-center text-sm text-ink-muted">
                {strings.sessionsLoadError}
                <span className="mt-1 block font-mono text-xs">{loadError}</span>
              </p>
            ) : loading && pageData.rows.length === 0 ? (
              <p className="py-12 text-center text-sm text-ink-muted">
                {strings.detailsLoading}
              </p>
            ) : pageData.rows.length === 0 ? (
              <div className="py-12 px-4 text-center text-sm text-ink-muted">
                <p>{filterActive ? strings.sessionsFilterEmpty : strings.sessionsEmpty}</p>
                {!filterActive && (
                  <p className="mt-1 text-xs">{strings.sessionsEmptyHint}</p>
                )}
              </div>
            ) : (
              pageData.rows.map((row) => (
                <SessionListItem
                  key={row.id}
                  row={row}
                  strings={strings}
                  active={selected?.id === row.id}
                  checked={checkedIds.has(row.id)}
                  onOpen={() => setSelected(row)}
                  onToggleCheck={() => toggleChecked(row.id)}
                  onDelete={() => setConfirm({ kind: "one", sessionId: row.id })}
                />
              ))
            )}
          </div>
          <div className="shrink-0 border-t border-border px-3 py-2">
            {checkedIds.size > 0 ? (
              <div className="flex min-h-[32px] items-center gap-2">
                <span className="shrink-0 text-sm font-medium text-ink">
                  {strings.sessionsSelectedCount.replace("{n}", String(checkedIds.size))}
                </span>
                <button
                  type="button"
                  onClick={togglePage}
                  disabled={pageData.rows.length === 0}
                  className="shrink-0 rounded-full border border-border px-2.5 py-1 text-xs text-ink transition-colors hover:bg-surface-muted disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {strings.sessionsSelectPage}
                </button>
                <button
                  type="button"
                  onClick={clearChecked}
                  className="shrink-0 rounded-full border border-border px-2.5 py-1 text-xs text-ink transition-colors hover:bg-surface-muted"
                >
                  {strings.sessionsClearPage}
                </button>
                <button
                  type="button"
                  onClick={() => setConfirm({ kind: "batch" })}
                  disabled={deleting || checkedIds.size === 0}
                  className="ml-auto inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border px-2.5 py-1 text-xs text-ink transition-colors hover:bg-surface-muted disabled:cursor-not-allowed disabled:opacity-50"
                >
                  <TrashIcon className="size-3.5 shrink-0" />
                  {deleting ? strings.sessionsDeleting : strings.sessionsDeleteSelected}
                </button>
              </div>
            ) : (
              <Pagination
                compact
                page={currentPage}
                pageCount={pageCount}
                total={pageData.total}
                pageSize={DEFAULT_PAGE_SIZE}
                pageSizeOptions={[DEFAULT_PAGE_SIZE]}
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
                onPageSizeChange={() => {}}
              />
            )}
          </div>
        </Card>
        <Card raised padding="none" className="flex min-h-0 flex-col overflow-hidden">
          {selected === null ? (
            <div className="flex h-full items-center justify-center px-6">
              <p className="text-center text-sm text-ink-muted">
                {strings.sessionsPickHint}
              </p>
            </div>
          ) : (
            <SessionDetailView
              row={selected}
              strings={strings}
              embedded
              onBack={() => setSelected(null)}
            />
          )}
        </Card>
      </div>
      <ConfirmDialog
        open={confirm !== null}
        title={
          confirm?.kind === "batch"
            ? strings.sessionsDeleteBatchTitle.replace("{n}", String(checkedIds.size))
            : strings.sessionsDeleteOneTitle
        }
        body={strings.sessionsDeleteConfirmBody}
        confirmLabel={deleting ? strings.sessionsDeleting : strings.sessionsDeleteConfirm}
        cancelLabel={strings.confirmDialogCancel}
        busy={deleting}
        onConfirm={() => {
          if (confirm?.kind === "batch") {
            runBatchDelete();
          } else if (confirm?.kind === "one") {
            runOneDelete(confirm.sessionId);
          }
        }}
        onCancel={() => {
          // 删除在途不允许关弹层（确认键此时也不可点）：结果未定，
          // 半路消失的弹层会让"删没删成"无从判断。
          if (!deleting) {
            setConfirm(null);
          }
        }}
      />
    </PageShell>
  );
}

/**
 * 会话列表项：左侧常驻空白槽位 + 工具图标 + 标题（无标题退回未命名），
 * 元信息行左端时间、右端请求次数（两端对齐；工具身份由图标承担，
 * 不再重复工具名）。选中高亮，整行可点。复选框与删除按钮都在悬停
 * （或键盘聚焦）时以 opacity 浮现——不用宽度动画，避免逐帧重排；
 * 槽位常驻留白。两者的点击与键盘事件都不冒泡成"打开详情"。
 */
function SessionListItem({
  row,
  strings,
  active,
  checked,
  onOpen,
  onToggleCheck,
  onDelete,
}: {
  readonly row: SessionRow;
  readonly strings: Strings;
  readonly active: boolean;
  readonly checked: boolean;
  readonly onOpen: () => void;
  readonly onToggleCheck: () => void;
  readonly onDelete: () => void;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onOpen();
        }
      }}
      aria-current={active ? "true" : undefined}
      className={`group flex w-full cursor-default items-center gap-2.5 border-b border-border/60 px-3 py-2.5 text-left transition-colors last:border-0 hover:bg-row-hover focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent ${
        active || checked ? "bg-accent/10" : ""
      }`}
    >
      {/* 左侧槽位常驻留白，复选框悬停/勾选/键盘聚焦时在此浮现：
          行级 items-center 让槽位控件落在行的几何中线上；动画只走
          opacity（纯合成器，零重排）；隐藏态 pointer-events-none，
          点击落回行（打开详情）。 */}
      <span className="flex w-5 shrink-0 items-center justify-center">
        <span
          className={`transition-opacity duration-150 ${
            checked
              ? "opacity-100"
              : "pointer-events-none opacity-0 group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100"
          }`}
        >
          <RowCheckbox checked={checked} label={strings.sessionsSelectRow} onToggle={onToggleCheck} />
        </span>
      </span>
      <span className="shrink-0">
        <ToolIcon toolId={row.tool} />
      </span>
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium text-ink-strong">
          {row.title ?? strings.sessionsUntitled}
        </span>
        <span className="mt-0.5 flex items-center justify-between gap-2 text-xs text-ink-muted">
          <span className="font-mono tabular-nums">
            {formatTimestamp(row.lastActivityAt)}
          </span>
          <span className="shrink-0 tabular-nums">
            <span className="font-mono text-ink-strong">{row.requestCount}</span>{" "}
            {strings.sessionsRequestsUnit}
          </span>
        </span>
      </span>
      {/* 行级删除：与复选框同一套浮现逻辑（悬停/键盘聚焦，纯 opacity）。
          槽位常驻留白、items-center 落中线。 */}
      <span className="flex w-6 shrink-0 items-center justify-end">
        <span className="pointer-events-none opacity-0 transition-opacity duration-150 group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100">
          <RowDeleteButton label={strings.sessionsDeleteRow} onDelete={onDelete} />
        </span>
      </span>
    </div>
  );
}

/**
 * 行级删除按钮：危险色调的垃圾桶（悬停加深）。点击不冒泡、键盘操作
 * 也不冒泡——行容器的 Enter/Space 会打开详情，这里必须就地拦住。
 */
function RowDeleteButton({
  label,
  onDelete,
}: {
  readonly label: string;
  readonly onDelete: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={(event) => {
        event.stopPropagation();
        onDelete();
      }}
      onKeyDown={(event) => event.stopPropagation()}
      className="rounded-full p-0.5 text-ink-muted transition-colors hover:bg-danger/10 hover:text-danger focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
    >
      <TrashIcon className="block size-3.5" />
    </button>
  );
}

/**
 * 行复选框：白底方框，勾选态描边加粗换强调色 + 强调色勾（勾线收进框内）。
 * role=checkbox 承载语义；点击 stopPropagation，防止冒泡成"打开详情"。
 */
function RowCheckbox({
  checked,
  label,
  onToggle,
}: {
  readonly checked: boolean;
  readonly label: string;
  readonly onToggle: () => void;
}) {
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={checked}
      aria-label={label}
      onClick={(event) => {
        event.stopPropagation();
        onToggle();
      }}
      className="shrink-0 rounded-control focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
    >
      <svg viewBox="0 0 20 20" className="block size-[18px]" aria-hidden="true">
        <rect
          x="2"
          y="2"
          width="16"
          height="16"
          rx="4.5"
          strokeWidth="2"
          className={checked ? "fill-surface stroke-accent" : "fill-surface stroke-ink-muted/50"}
        />
        {checked && (
          <path
            d="M5.5 10.5l3.5 3.5L15 6.5"
            fill="none"
            strokeWidth="2.2"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="stroke-accent"
          />
        )}
      </svg>
    </button>
  );
}
