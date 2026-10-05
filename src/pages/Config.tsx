/**
 * 配置页：各工具本地配置的只读视图。左列表选工具，右卡片三个页签——
 * MCP（连接元数据，密钥已打码）、Skills 与文件和目录，后两者左列表右详情。
 * 数据现读磁盘（fetchToolConfig）；内容回读有凭据拒读与脱敏闸门，见
 * toktol-core 的 toolconfig 模块。
 */

import { useEffect, useMemo, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";

import { PageShell } from "../components/PageShell";
import {
  ChevronIcon,
  FileIcon,
  FolderIcon,
  FolderOpenIcon,
} from "../components/ui/icons";
import { Badge } from "../components/ui/Badge";
import { Card } from "../components/ui/Card";
import { Segmented } from "../components/ui/Segmented";
import { ToolIcon } from "../components/ui/ToolIcon";
import {
  fetchToolConfig,
  fetchToolConfigEntry,
  type FileContent,
  type FileEntry,
  type SkillView,
  type ToolConfigReport,
} from "../lib/api";
import { TOOL_ITEMS } from "../lib/tools";
import type { MessageKey, Strings } from "../i18n/strings";

type ConfigTab = "mcp" | "skills" | "files";

const TABS: readonly { value: ConfigTab; key: MessageKey }[] = [
  { value: "mcp", key: "configTabMcp" },
  { value: "skills", key: "configTabSkills" },
  { value: "files", key: "configTabFiles" },
];

interface ConfigPageProps {
  readonly strings: Strings;
  /** 测试缝隙：SSR 渲染不跑 effect，用注入的报告覆盖首屏。 */
  readonly initial?: ToolConfigReport;
  /** 测试缝隙：SSR 无法点击页签，直接指定初始页。 */
  readonly initialTab?: ConfigTab;
}

export function ConfigPage({ strings, initial, initialTab }: ConfigPageProps) {
  const [toolId, setToolId] = useState(TOOL_ITEMS[0]?.id ?? "claude-code");
  const [reports, setReports] = useState<Record<string, ToolConfigReport>>(
    initial === undefined ? {} : { [initial.tool]: initial },
  );
  const [tab, setTab] = useState<ConfigTab>(initialTab ?? "mcp");
  /** 技能详情"在文件中打开"的跳转焦点：置入后文件页签选中该条目，消费后清空。 */
  const [focusFile, setFocusFile] = useState<{ root: string; rel: string } | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const report = reports[toolId] ?? null;
  const loading = report === null && loadError === null;

  useEffect(() => {
    if (reports[toolId] !== undefined) return;
    let alive = true;
    fetchToolConfig(toolId)
      .then((next) => {
        if (!alive) return;
        setReports((prev) => ({ ...prev, [toolId]: next }));
        setLoadError(null);
      })
      .catch((err: unknown) => {
        if (alive) setLoadError(String(err));
      });
    return () => {
      alive = false;
    };
  }, [toolId, reports]);

  const toolLabel =
    TOOL_ITEMS.find((item) => item.id === toolId)?.label ?? toolId;

  return (
    /* 同设置页：无页面标题，两栏卡片顶格撑满（导航标签本身就是页名）。 */
    <PageShell fill>
      {/* 布局同设置页：固定侧栏列 + 内容列 minmax(0,1fr)，两卡撑满页高。 */}
      <div className="grid min-h-0 flex-1 grid-cols-[230px_minmax(0,1fr)] gap-4">
        <Card
          as="nav"
          aria-label={strings.configSidebarTitle}
          raised
          padding="p-2"
          className="self-stretch"
        >
          {/* 项间留 1 档空隙：相邻项的高亮/悬浮底色不能连成一片（同设置页导航）。 */}
          <div className="space-y-1">
            {TOOL_ITEMS.map((item) => (
              <button
                key={item.id}
                type="button"
                onClick={() => setToolId(item.id)}
                aria-current={item.id === toolId}
                className={`flex w-full items-center gap-2.5 rounded-control px-3 py-2 text-left text-sm transition-colors ${
                  item.id === toolId
                    ? "bg-accent/10 font-medium text-ink"
                    : "text-ink hover:bg-accent/5"
                }`}
              >
                <ToolIcon toolId={item.id} />
                <span className="truncate">{item.label}</span>
              </button>
            ))}
          </div>
        </Card>

      <Card padding="none" className="flex min-h-0 min-w-0 flex-col">
        <header className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-border px-5 py-3">
          <div className="flex min-w-0 items-center gap-2.5">
            <ToolIcon toolId={toolId} size={20} />
            <span className="truncate text-sm font-semibold">{toolLabel}</span>
            {report !== null && (
              <span className="truncate font-mono text-xs text-ink-muted">
                {report.homeLabel}
              </span>
            )}
          </div>
          <Segmented
            size="sm"
            label={strings.navConfig}
            options={TABS.map((t) => ({ value: t.value, label: strings[t.key] }))}
            value={tab}
            onChange={setTab}
          />
        </header>

        {loadError !== null && report === null ? (
          <div className="px-5 py-16 text-center text-sm text-ink-muted">
            {strings.configLoadError}
          </div>
        ) : report !== null && !report.supported ? (
          <div className="px-5 py-16 text-center text-sm text-ink-muted">
            {strings.configUnsupported}
          </div>
        ) : report === null ? null : tab === "mcp" ? (
          <McpPane strings={strings} report={report} />
        ) : tab === "skills" ? (
          <SkillsPane
            strings={strings}
            toolId={toolId}
            report={report}
            onOpenInFiles={(focus) => {
              setFocusFile(focus);
              setTab("files");
            }}
          />
        ) : (
          <FilesPane
            strings={strings}
            toolId={toolId}
            report={report}
            focusFile={focusFile}
            onFocusFileConsumed={() => setFocusFile(null)}
          />
        )}

        {loading && (
          <div className="px-5 py-16 text-center text-sm text-ink-muted" aria-live="polite">
            …
          </div>
        )}
      </Card>
      </div>
    </PageShell>
  );
}

/** `{count}` 模板的本地替换：i18n 表是纯字符串，不带插值机制。 */
function withCount(template: string, count: number): string {
  return template.replace("{count}", String(count));
}

function CountRow({ label, count }: { readonly label: string; readonly count: number }) {
  return (
    <div className="shrink-0 border-b border-border px-5 py-2.5 text-sm text-ink-muted">
      {withCount(label, count)}
    </div>
  );
}

function scopeLabel(strings: Strings, scope: string): string {
  if (scope === "user") return strings.configScopeUser;
  if (scope === "project") return strings.configScopeProject;
  if (scope === "plugin") return strings.configScopePlugin;
  return scope;
}

function McpPane({ strings, report }: { readonly strings: Strings; readonly report: ToolConfigReport }) {
  if (report.mcp.length === 0) {
    return <PaneEmpty title={strings.configEmptyMcpTitle} detail={strings.configEmptyMcpDetail} />;
  }
  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <CountRow label={strings.configMcpCount} count={report.mcp.length} />
      <div className="divide-y divide-border px-5">
        {report.mcp.map((server) => (
          <div
            key={`${server.project ?? ""}/${server.name}`}
            className="flex flex-col gap-1 py-3"
          >
            <div className="flex flex-wrap items-center gap-2">
              <span className="text-sm font-medium">{server.name}</span>
              <Badge>{server.transport}</Badge>
              <Badge>{scopeLabel(strings, server.scope)}</Badge>
            </div>
            {server.command !== null && (
              <p className="break-all font-mono text-xs text-ink-muted">{server.command}</p>
            )}
            {server.url !== null && (
              <p className="break-all font-mono text-xs text-ink-muted">{server.url}</p>
            )}
            {server.envKeys.length > 0 && (
              <p className="text-xs text-ink-muted">
                {strings.configEnvKeys}: {server.envKeys.join(", ")}
              </p>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

function SkillsPane({
  strings,
  toolId,
  report,
  onOpenInFiles,
}: {
  readonly strings: Strings;
  readonly toolId: string;
  readonly report: ToolConfigReport;
  readonly onOpenInFiles: (focus: { root: string; rel: string }) => void;
}) {
  // 缺省选中首项：主从布局让右侧常驻有内容比空态更有用；SSR 也因此能渲染详情。
  const [selected, setSelected] = useState<SkillView | null>(
    () => report.skills[0] ?? null,
  );
  const [filter, setFilter] = useState("");

  // 换工具/重拉后复位到首项：旧选择对新报告可能不存在。
  useEffect(() => {
    setSelected(report.skills[0] ?? null);
    setFilter("");
  }, [report]);

  const needle = filter.trim().toLowerCase();
  const visible = useMemo(() => {
    if (needle === "") return report.skills;
    return report.skills.filter(
      (s) =>
        s.name.toLowerCase().includes(needle) ||
        (s.description ?? "").toLowerCase().includes(needle),
    );
  }, [report.skills, needle]);

  // 过滤后选中项不在可见列表时跳到首项：右侧详情不能指向看不见的行。
  useEffect(() => {
    if (visible.length === 0) return;
    if (
      selected !== null &&
      visible.some((s) => s.path === selected.path && s.name === selected.name)
    ) {
      return;
    }
    setSelected(visible[0] ?? null);
  }, [visible, selected]);

  if (report.skills.length === 0) {
    return (
      <PaneEmpty title={strings.configEmptySkillsTitle} detail={strings.configEmptySkillsDetail} />
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <CountRow label={strings.configSkillCount} count={report.skills.length} />
      {/* 布局同文件页签：窄列表 + 常驻详情，取代原先的模态抽屉。 */}
      <div className="flex min-h-0 flex-1">
        <div className="flex w-72 shrink-0 flex-col border-r border-border">
          <div className="shrink-0 border-b border-border p-2.5">
            <input
              type="search"
              value={filter}
              onChange={(event) => setFilter(event.target.value)}
              placeholder={strings.configSkillsFilter}
              aria-label={strings.configSkillsFilter}
              className="w-full rounded-control border border-border bg-surface px-2.5 py-1.5 text-sm text-ink outline-none placeholder:text-ink-muted focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring"
            />
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto py-1">
            {visible.map((skill) => {
              const active =
                selected !== null &&
                selected.path === skill.path &&
                selected.name === skill.name;
              return (
                <button
                  key={`${skill.path}/${skill.name}`}
                  type="button"
                  onClick={() => setSelected(skill)}
                  onKeyDown={onSkillRowKeyDown}
                  aria-current={active}
                  className={`block w-full truncate px-3 py-1.5 text-left font-mono text-xs transition-colors ${
                    active
                      ? "bg-accent/10 font-medium text-ink"
                      : "text-ink-muted hover:bg-accent/5 hover:text-ink"
                  }`}
                >
                  {skill.name}
                </button>
              );
            })}
          </div>
        </div>

        {selected === null || visible.length === 0 ? (
          <div className="flex min-w-0 flex-1 items-center justify-center px-6 text-sm text-ink-muted">
            {strings.configSkillsHint}
          </div>
        ) : (
          <SkillDetail
            toolId={toolId}
            strings={strings}
            skill={selected}
            onOpenInFiles={onOpenInFiles}
          />
        )}
      </div>
    </div>
  );
}

/** ↑↓ 在相邻行间移动选中：聚焦相邻按钮并触发点击，选中态随之迁移。 */
function onSkillRowKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
  const next =
    event.key === "ArrowDown"
      ? event.currentTarget.nextElementSibling
      : event.key === "ArrowUp"
        ? event.currentTarget.previousElementSibling
        : null;
  if (next instanceof HTMLButtonElement) {
    event.preventDefault();
    next.focus();
    next.click();
  }
}

/**
 * 技能详情面板：主从布局的右栏，随左列表选中常驻刷新。内容经
 * fetchToolConfigEntry 现读 SKILL.md——凭据拒读与脱敏闸门在 Rust 侧。
 */
function SkillDetail({
  toolId,
  strings,
  skill,
  onOpenInFiles,
}: {
  readonly toolId: string;
  readonly strings: Strings;
  readonly skill: SkillView;
  readonly onOpenInFiles: (focus: { root: string; rel: string }) => void;
}) {
  const [content, setContent] = useState<FileContent | null>(null);
  const [contentError, setContentError] = useState<string | null>(null);

  useEffect(() => {
    if (skill.rel === "") return;
    let alive = true;
    setContent(null);
    setContentError(null);
    fetchToolConfigEntry(toolId, skill.root, skill.rel)
      .then((next) => {
        if (alive) setContent(next);
      })
      .catch((err: unknown) => {
        if (alive) setContentError(String(err));
      });
    return () => {
      alive = false;
    };
  }, [toolId, skill]);

  return (
    <div className="min-h-0 min-w-0 flex-1 overflow-y-auto p-5">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="break-all text-sm font-semibold">{skill.name}</h2>
        <Badge>{scopeLabel(strings, skill.scope)}</Badge>
      </div>
      {skill.description !== null && (
        <p className="mt-2 text-sm leading-6 text-ink">{skill.description}</p>
      )}
      <p className="mt-1 break-all font-mono text-xs text-ink-muted">{skill.path}</p>
      <button
        type="button"
        onClick={() => onOpenInFiles({ root: skill.root, rel: skill.rel })}
        className="mt-3 rounded-control border border-border px-3 py-1.5 text-sm text-ink transition-colors hover:bg-surface-subtle"
      >
        {strings.configSkillOpenFiles}
      </button>

      <div className="mt-4 border-t border-border pt-4">
        {skill.rel === "" || contentError !== null ? (
          <p className="text-sm text-ink-muted">{strings.configContentDenied}</p>
        ) : content === null ? (
          <p className="text-sm text-ink-muted">…</p>
        ) : (
          <>
            {content.truncated && (
              <p className="mb-2 text-xs text-ink-muted">{strings.configContentTruncated}</p>
            )}
            <pre className="overflow-x-auto whitespace-pre-wrap break-all font-mono text-xs leading-5 text-ink">
              {content.text}
            </pre>
          </>
        )}
      </div>
    </div>
  );
}

interface FileSelection {
  readonly root: string;
  readonly rel: string;
  readonly dir: boolean;
}

function FilesPane({
  strings,
  toolId,
  report,
  focusFile,
  onFocusFileConsumed,
}: {
  readonly strings: Strings;
  readonly toolId: string;
  readonly report: ToolConfigReport;
  readonly focusFile: { root: string; rel: string } | null;
  readonly onFocusFileConsumed: () => void;
}) {
  const [rootKey, setRootKey] = useState(report.roots[0]?.key ?? "");
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<FileSelection | null>(null);
  const [content, setContent] = useState<FileContent | null>(null);
  const [contentError, setContentError] = useState<string | null>(null);
  /** 已展开目录的 rel 集合；缺省 = 全折叠。 */
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(new Set());

  // 换工具/重拉后复位选择：旧 rel 对新报告可能不存在。
  useEffect(() => {
    setRootKey(report.roots[0]?.key ?? "");
    setSelected(null);
    setContent(null);
    setContentError(null);
    setExpanded(new Set());
  }, [report]);

  function toggleDir(rel: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(rel)) {
        next.delete(rel);
      } else {
        next.add(rel);
      }
      return next;
    });
  }

  // 技能详情"在文件中打开"：直接按 (root, rel) 选中并拉内容——rel 可能比
  // 文件树（深度 3）更深（如插件缓存里的 SKILL.md），高亮查不到但内容照常显示。
  useEffect(() => {
    if (focusFile === null) return;
    setSelected({ root: focusFile.root, rel: focusFile.rel, dir: false });
    onFocusFileConsumed();
  }, [focusFile, onFocusFileConsumed]);

  useEffect(() => {
    if (selected === null || selected.dir) return;
    let alive = true;
    setContent(null);
    setContentError(null);
    fetchToolConfigEntry(toolId, selected.root, selected.rel)
      .then((next) => {
        if (alive) setContent(next);
      })
      .catch((err: unknown) => {
        if (alive) setContentError(String(err));
      });
    return () => {
      alive = false;
    };
  }, [toolId, selected]);

  const root = report.roots.find((r) => r.key === rootKey) ?? report.roots[0] ?? null;
  const needle = filter.trim().toLowerCase();
  const entries = useMemo(() => {
    if (root === null) return [];
    if (needle === "") return root.entries;
    return root.entries.filter(
      (e) => e.rel.toLowerCase().includes(needle) || e.name.toLowerCase().includes(needle),
    );
  }, [root, needle]);

  // 树可见性：无过滤词时只展开已点开的目录（逐层递归）；有过滤词时忽略
  // 折叠平铺全部匹配项——否则折叠目录里的匹配文件永远看不见。
  const visible = useMemo(() => {
    if (needle !== "") return entries;
    const out: FileEntry[] = [];
    const walk = (parent: string) => {
      for (const entry of entries) {
        const entryParent = entry.rel.includes("/")
          ? entry.rel.slice(0, entry.rel.lastIndexOf("/"))
          : "";
        if (entryParent !== parent) continue;
        out.push(entry);
        if (entry.dir && expanded.has(entry.rel)) walk(entry.rel);
      }
    };
    walk("");
    return out;
  }, [entries, expanded, needle]);

  const childCount = useMemo(() => {
    if (selected === null || !selected.dir || root === null) return null;
    const prefix = `${selected.rel}/`;
    const depth = selected.rel.split("/").length + 1;
    return root.entries.filter(
      (e) => e.rel.startsWith(prefix) && e.rel.split("/").length === depth,
    ).length;
  }, [selected, root]);

  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex w-72 shrink-0 flex-col border-r border-border">
        <div className="shrink-0 border-b border-border p-2.5">
          <input
            type="search"
            value={filter}
            onChange={(event) => setFilter(event.target.value)}
            placeholder={strings.configFilesFilter}
            aria-label={strings.configFilesFilter}
            className="w-full rounded-control border border-border bg-surface px-2.5 py-1.5 text-sm text-ink outline-none placeholder:text-ink-muted focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring"
          />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto py-1">
          {report.roots.length > 1 && (
            <div className="px-3 pb-1 pt-2 font-mono text-xs text-ink-muted">
              {root?.label}
            </div>
          )}
          {visible.map((entry) => (
            <FileRow
              key={entry.rel}
              entry={entry}
              expanded={entry.dir && expanded.has(entry.rel)}
              selected={
                selected !== null && selected.root === rootKey && selected.rel === entry.rel
              }
              onSelect={() => {
                if (entry.dir) toggleDir(entry.rel);
                setSelected({ root: rootKey, rel: entry.rel, dir: entry.dir });
              }}
            />
          ))}
        </div>
      </div>

      <div className="min-w-0 flex-1 overflow-y-auto">
        {selected === null ? (
          <div className="flex h-full items-center justify-center px-6 text-sm text-ink-muted">
            {strings.configFilesHint}
          </div>
        ) : selected.dir ? (
          <div className="flex h-full flex-col items-center justify-center gap-1 px-6 text-center">
            <p className="font-mono text-xs text-ink-muted">{selected.rel}</p>
            <p className="text-sm text-ink-muted">{withCount(strings.configFileCount, childCount ?? 0)}</p>
          </div>
        ) : contentError !== null ? (
          <div className="px-5 py-10 text-sm text-ink-muted">{strings.configContentDenied}</div>
        ) : content === null ? (
          <div className="px-5 py-10 text-sm text-ink-muted">…</div>
        ) : (
          <div className="flex flex-col">
            {content.truncated && (
              <div className="border-b border-border px-5 py-2 text-xs text-ink-muted">
                {strings.configContentTruncated}
              </div>
            )}
            <pre className="overflow-x-auto p-5 font-mono text-xs leading-5 text-ink">
              {content.text}
            </pre>
          </div>
        )}
      </div>
    </div>
  );
}

function FileRow({
  entry,
  expanded,
  selected,
  onSelect,
}: {
  readonly entry: FileEntry;
  readonly expanded: boolean;
  readonly selected: boolean;
  readonly onSelect: () => void;
}) {
  const depth = entry.rel.split("/").length - 1;
  return (
    <button
      type="button"
      onClick={onSelect}
      aria-expanded={entry.dir ? expanded : undefined}
      style={{ paddingLeft: `${0.75 + depth * 0.875}rem` }}
      className={`flex w-full items-center gap-1.5 py-1 pr-3 text-left font-mono text-xs ${
        selected
          ? "bg-accent/10 font-medium text-ink"
          : "text-ink-muted hover:bg-accent/5 hover:text-ink"
      }`}
    >
      {/* 闭口/开口文件夹区分折叠态；文件用灰色文档：一眼区分类型与状态。 */}
      {/* 小尺寸下闭口/开口文件夹肉眼太接近：箭头方向（右=折叠、下=展开）才是
          决定性的状态信号；文件行留等宽空位，文件名与目录名对齐。 */}
      <span className="flex w-3 shrink-0 justify-center">
        {entry.dir && (
          <ChevronIcon right={!expanded} className="size-3 text-ink-muted" />
        )}
      </span>
      {entry.dir ? (
        expanded ? (
          <FolderOpenIcon className="size-3.5 shrink-0 text-accent" />
        ) : (
          <FolderIcon className="size-3.5 shrink-0 text-accent" />
        )
      ) : (
        <FileIcon className="size-3.5 shrink-0 text-ink-muted" />
      )}
      <span className="truncate">{entry.name}</span>
    </button>
  );
}

function PaneEmpty({ title, detail }: { readonly title: string; readonly detail: string }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-1 px-6 py-16 text-center">
      <p className="text-sm text-ink">{title}</p>
      <p className="text-xs text-ink-muted">{detail}</p>
    </div>
  );
}
