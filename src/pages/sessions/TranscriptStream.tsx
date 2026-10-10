/**
 * 转录时间线的消息渲染：单条消息按内容形态分派（用户气泡 / 事件卡 /
 * 注入区 / 中断分隔线 / 各内容块）。块类型由核心层归一（adapter 产出统一
 * 的 TranscriptBlock 词汇表），本文件按内容形态分支，不按工具分支；工具
 * 名只作为各形态的出身记录出现在注释里。
 */

import { Fragment, memo, useState } from "react";
import { ChevronIcon } from "../../components/ui/icons";
import { ImageAttachment } from "./ImageAttachment";
import { MarkdownView } from "./MarkdownView";
import type { Strings } from "../../i18n/strings";
import type { TranscriptBlock, TranscriptEntry } from "../../lib/api";
import { formatTimestamp } from "../../lib/format";
import { buildOriginalTree } from "../../lib/transcriptOriginal";
import { renderOriginalTree } from "./OriginalTextView";

const ROLE_LABEL: Record<TranscriptEntry["role"], keyof Strings> = {
  user: "transcriptRoleUser",
  assistant: "transcriptRoleAssistant",
  system: "transcriptRoleSystem",
  tool: "transcriptRoleTool",
};

/** 长文本折叠阈值：超过任一即给「展开/收起」。 */
const COLLAPSE_CHARS = 400;
const COLLAPSE_LINES = 8;
const RESULT_MAX_LINES = 500;
/** 工具卡收起态的行数阈值：超过即默认折叠（配合字符阈值双保险）。 */
const RESULT_PREVIEW_LINES = 12;

/**
 * 时间线单条：角色标签（用户右对齐、其余左对齐）+ 内容。
 * 用户 = 强调色实底气泡；助手/工具 = 各内容块平铺（工具调用/结果卡）；
 * 系统 = 居中弱化说明。注入块（harness 的 system-reminder 等包裹段）随
 * 所属消息进气泡：占比条 + 纯净/完整切换都是**每条消息自带**的，完整
 * 口径在气泡内展开注入原文。
 * memo：entry/strings 引用稳定，滚动联动（activeIndex）高频重渲染时
 * 几百条气泡全部跳过 diff——不 memo 的话大会话滚动必卡。
 */
export const StreamEntry = memo(function StreamEntry({
  entry,
  strings,
}: {
  readonly entry: TranscriptEntry;
  readonly strings: Strings;
}) {
  // 只有带正文的用户消息才算「一次询问」（Claude Code 的工具结果也是
  // user 角色行，但没有 text 块）——它们归入上一轮，不套强调色气泡。
  // 正文含图片块：只贴图不打字的用户消息（opencode 常见）同样是一轮，
  // 不然会落进"工具结果"分支平铺到左侧。注入块不算正文：纯注入消息
  // 不开启轮次，落一条中性气泡承载注入区。
  const isUserTurn =
    entry.role === "user" &&
    entry.blocks.some((block) => block.kind === "text" || block.kind === "image");

  // 系统生成的 user 角色消息（任务通知 / 压缩摘要 / 继续指令）：不套
  // 用户气泡、不带角色标签行，渲染成系统事件卡。
  if (entry.blocks.length > 0 && entry.blocks.every(isMetaBlock)) {
    return (
      <div className="min-w-0 flex-1 pr-6">
        <MetaEntryCard entry={entry} strings={strings} />
      </div>
    );
  }

  // 纯注入消息（模式切换提醒等）：中性气泡承载注入区（占比条 + 完整原文）。
  const hasBody = entry.blocks.some((block) => block.kind !== "injected");
  if (!hasBody) {
    if (entry.blocks.length === 0) return null;
    return (
      <div className="min-w-0 flex-1 pr-6">
        <UserTurnBubble blocks={entry.blocks} strings={strings} tone="neutral" />
      </div>
    );
  }

  // 工具注入的会话上下文（codex 的 environment_context / skills_instructions
  // 等包裹消息）：**一条源消息一张卡**，卡内 XML 元素默认折叠——不占用户
  // 气泡、不锚定轮次。
  if (
    entry.blocks.length > 0 &&
    entry.blocks.every((block) => block.kind === "harnessContext")
  ) {
    return (
      <div className="min-w-0 flex-1 pr-6">
        <HarnessCard blocks={entry.blocks} strings={strings} />
      </div>
    );
  }

  // 用户中断标记（codex 的 <turn_aborted>）：红色分隔线徽章——"上一轮到此
  // 为止"。事件而非内容，不占气泡、不锚定轮次。
  if (
    entry.blocks.length > 0 &&
    entry.blocks.every((block) => block.kind === "turnAborted")
  ) {
    return (
      <div className="min-w-0 flex-1 pr-6">
        <TurnAbortedDivider strings={strings} />
      </div>
    );
  }

  const label = (
    <p
      className={`mb-1.5 flex items-center gap-2 text-xs text-ink-muted ${
        isUserTurn ? "justify-end" : ""
      }`}
    >
      <span>{strings[ROLE_LABEL[entry.role]]}</span>
      {entry.model !== null && (
        // min-w-0：flex 子项的 truncate 靠它生效——模型名是无空格长 token，
        // 缺了它 min-width:auto 顶住不缩，整行撑出视口造成横向滚动条。
        <span className="min-w-0 truncate font-mono" title={entry.model}>
          {entry.model}
        </span>
      )}
      {entry.tsMs !== null && (
        <span className="shrink-0 font-mono tabular-nums">
          {formatTimestamp(entry.tsMs)}
        </span>
      )}
    </p>
  );

  if (entry.role === "system") {
    const text = entry.blocks
      .filter((b) => b.kind === "text")
      .map((b) => (b.kind === "text" ? b.text : ""))
      .join("\n");
    return (
      <p className="min-w-0 flex-1 text-center text-xs leading-relaxed break-words whitespace-pre-wrap text-ink-muted">
        {text}
      </p>
    );
  }

  return (
    <div className="min-w-0 flex-1 pr-6">
      {label}
      {isUserTurn ? (
        <UserTurnBubble blocks={entry.blocks} strings={strings} />
      ) : (
        <div className="space-y-2.5">
          {entry.blocks.map((block, index) => (
            <BlockView key={index} block={block} strings={strings} />
          ))}
        </div>
      )}
    </div>
  );
});

/**
 * 气泡内的注入呈现口径：纯净=正文 + 占比条；完整=整条原文 + 图片按提及
 * 插入；纯文本=整条原文但不渲染任何图片（提及标记保留为文字）。
 */
type BubbleViewMode = "pure" | "full" | "text";

/**
 * 用户气泡：整条消息一个折叠单元——任一文本段超阈值即整条可折叠。
 * 收起时只显示首段文本（钳制行数），图片与其余文本随「展开」一起出现，
 * 避免展开按钮切在文字和图片中间。注入块随消息进气泡：气泡顶部的占比条
 * 与 纯净/完整/纯文本 切换是**每条消息自带**的——纯净显示正文，完整把正文
 * 替换成注入块携带的整条原文（提问内容就在 `<user_query>` 标签内，不拆
 * 两截，图片按提及插入），纯文本同样渲染原文但不渲染任何图片。
 * 独立导出供测试（SSR 直渲）。
 */
export function UserTurnBubble({
  blocks,
  strings,
  tone = "accent",
  initialMode = "pure",
  initialFoldOverrides,
}: {
  readonly blocks: readonly TranscriptBlock[];
  readonly strings: Strings;
  /** accent = 用户强调色气泡（右对齐）；neutral = 纯注入消息的中性气泡。 */
  readonly tone?: "accent" | "neutral";
  /** 测试缝隙：初始呈现口径（默认纯净）。 */
  readonly initialMode?: BubbleViewMode;
  /** 测试缝隙：初始折叠覆盖表（元素默认全折叠，路径 → 是否折叠）。 */
  readonly initialFoldOverrides?: Readonly<Record<string, boolean>>;
}) {
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<BubbleViewMode>(initialMode);
  // 元素级折叠：路径 → 是否收起。默认全折叠，覆盖项记用户动过的元素。
  const [foldOverride, setFoldOverride] = useState<Record<string, boolean>>(
    () => ({ ...(initialFoldOverrides ?? {}) }),
  );
  const toggleFold = (path: string, next: boolean) =>
    setFoldOverride((prev) => ({ ...prev, [path]: next }));
  const isAccent = tone === "accent";
  const injectedBlocks = blocks.filter((block) => block.kind === "injected");
  // grok 的 <user_query> 信封行即使无注入也产 0 字符 Injected 块（原文树
  // 素材）：占比条按注入字符总数显隐——0 字符（裸提问）不显条，树随完整/
  // 纯文本口径照常出现。
  const injectedCharsTotal = injectedBlocks.reduce(
    (sum, block) => sum + block.injectedChars,
    0,
  );
  const bodyBlocks = blocks.filter((block) => block.kind !== "injected");
  const bodyChars = bodyBlocks.reduce(
    (count, block) => count + (block.kind === "text" ? block.text.length : 0),
    0,
  );
  const long = bodyBlocks.some(
    (block) =>
      block.kind === "text" &&
      (block.text.length > COLLAPSE_CHARS ||
        block.text.split("\n").length > COLLAPSE_LINES),
  );
  const firstText = bodyBlocks.find((block) => block.kind === "text");

  const content = (() => {
    // 完整/纯文本口径优先于正文折叠：切档后原文按 XML 元素逐个折叠，
    // 否则长消息收起时连口径切换都看不见。
    if (injectedBlocks.length > 0 && injectedCharsTotal > 0 && mode !== "pure") {
      return (
        <>
          <InjectedBar
            blocks={injectedBlocks}
            bodyChars={bodyChars}
            mode={mode}
            onToggle={setMode}
            tone={tone}
            strings={strings}
          />
          {/* pl-4 给 chevron 的悬挂空间：滚动容器会裁掉内容盒左侧的
              绝对定位元素，留出内边距后 -left-4 正好落在可视区内。 */}
          <div className="overflow-x-auto pl-4">
            {renderOriginalTree(
              buildOriginalTree(
                injectedBlocks,
                mode === "full"
                  ? bodyBlocks.filter((block) => block.kind === "image")
                  : [],
              ),
              foldOverride,
              "0",
              isAccent,
              strings,
              toggleFold,
            )}
          </div>
          {mode === "full" &&
            bodyBlocks
              .filter((block) => {
                if (block.kind === "image") return false;
                // 正文若已被注入原文覆盖（workbuddy 的 Injected 块携带整条
                // 消息原文，<user_query> 就在树里）不重复渲染；grok 吸收的
                // 独立 reminder 行不含提问，完整口径下补渲染正文。
                if (block.kind === "text")
                  return !injectedBlocks.some((injected) =>
                    injected.text.includes(block.text),
                  );
                return true;
              })
              .map((block, index) => (
                <BlockView
                  key={index}
                  block={block}
                  strings={strings}
                  onAccent={isAccent}
                />
              ))}
        </>
      );
    }
    if (long && !open) {
      return (
        <>
          <p className="text-sm leading-relaxed break-words whitespace-pre-wrap line-clamp-6">
            {firstText?.kind === "text" ? firstText.text : ""}
          </p>
          <CollapseToggle
            open={open}
            onToggle={() => setOpen(true)}
            strings={strings}
          />
        </>
      );
    }
    return (
      <>
        {injectedCharsTotal > 0 && (
          <InjectedBar
            blocks={injectedBlocks}
            bodyChars={bodyChars}
            mode={mode}
            onToggle={setMode}
            tone={tone}
            strings={strings}
          />
        )}
        {bodyBlocks.map((block, index) =>
          block.kind === "text" ? (
            <p
              key={index}
              className="text-sm leading-relaxed break-words whitespace-pre-wrap"
            >
              {block.text}
            </p>
          ) : (
            <BlockView
              key={index}
              block={block}
              strings={strings}
              onAccent={isAccent}
            />
          ),
        )}
      </>
    );
  })();

  return (
    <div className={`flex ${isAccent ? "justify-end" : ""}`}>
      <div
        className={
          isAccent
            ? "w-fit max-w-[75%] rounded-card rounded-br-sm bg-accent px-4 py-3 text-accent-ink"
            : "w-full max-w-[75%] rounded-card bg-surface-subtle px-4 py-3"
        }
      >
        {content}
      </div>
    </div>
  );
}

/**
 * 气泡内的注入区（每条含注入的消息自带）：占比条（注入字符 / 全消息字符）
 * + 纯净/完整/纯文本三档切换。切换只管口径，正文/原文的取舍由气泡（调用
 * 方）决定。
 */
function InjectedBar({
  blocks,
  bodyChars,
  mode,
  onToggle,
  tone,
  strings,
}: {
  readonly blocks: readonly TranscriptBlock[];
  readonly bodyChars: number;
  readonly mode: BubbleViewMode;
  readonly onToggle: (mode: BubbleViewMode) => void;
  readonly tone: "accent" | "neutral";
  readonly strings: Strings;
}) {
  const injectedChars = blocks.reduce(
    (count, block) =>
      count + (block.kind === "injected" ? block.injectedChars : 0),
    0,
  );
  const totalChars = injectedChars + bodyChars;
  const percent =
    totalChars === 0 ? 100 : Math.round((injectedChars / totalChars) * 100);
  const caption = strings.transcriptInjectedRatio
    .replace("{percent}", String(percent))
    .replace("{injected}", injectedChars.toLocaleString("en-US"))
    .replace("{total}", totalChars.toLocaleString("en-US"));
  const onAccent = tone === "accent";
  const chip = (selected: boolean) =>
    `rounded-control px-2 py-0.5 text-xs transition-colors ${
      selected
        ? onAccent
          ? "bg-accent-ink text-accent"
          : "bg-accent text-accent-ink"
        : onAccent
          ? "text-accent-ink/75 hover:text-accent-ink"
          : "text-ink-muted hover:text-ink"
    }`;
  const modes: readonly { readonly value: BubbleViewMode; readonly label: string }[] = [
    { value: "pure", label: strings.transcriptViewPure },
    { value: "full", label: strings.transcriptViewFull },
    { value: "text", label: strings.transcriptViewText },
  ];
  return (
    <div className="mb-2">
      <div className="flex items-center gap-2.5">
        <div
          className={`h-1 min-w-0 flex-1 overflow-hidden rounded-full ${
            onAccent ? "bg-accent-ink/25" : "bg-surface-muted"
          }`}
          role="img"
          aria-label={caption}
        >
          <div
            className={`h-full rounded-full ${onAccent ? "bg-accent-ink" : "bg-ink-muted/60"}`}
            style={{ width: `${percent}%` }}
          />
        </div>
        <div
          className={`inline-flex shrink-0 items-center gap-0.5 rounded-control p-0.5 ${
            onAccent ? "bg-accent-ink/15" : "bg-surface-muted"
          }`}
          role="group"
          aria-label={strings.transcriptViewToggle}
        >
          {modes.map((option) => (
            <button
              key={option.value}
              type="button"
              onClick={() => onToggle(option.value)}
              className={chip(mode === option.value)}
            >
              {option.label}
            </button>
          ))}
        </div>
      </div>
      <p
        className={`mt-1 text-right font-mono text-xs tabular-nums ${
          onAccent ? "text-accent-ink/80" : "text-ink-muted"
        }`}
      >
        {caption}
      </p>
    </div>
  );
}

/** 长文本折叠展示：默认 line-clamp，点「展开」放开。 */
function CollapsibleText({
  text,
  strings,
}: {
  readonly text: string;
  readonly strings: Strings;
}) {
  const [open, setOpen] = useState(false);
  const long = text.length > COLLAPSE_CHARS || text.split("\n").length > COLLAPSE_LINES;
  return (
    <div>
      <p
        className={`text-sm leading-relaxed break-words whitespace-pre-wrap ${
          long && !open ? "line-clamp-6" : ""
        }`}
      >
        {text}
      </p>
      {long ? (
        <CollapseToggle
          open={open}
          onToggle={() => setOpen(!open)}
          strings={strings}
        />
      ) : null}
    </div>
  );
}

/** 折叠单元共用的展开/收起按钮。 */
function CollapseToggle({
  open,
  onToggle,
  strings,
}: {
  readonly open: boolean;
  readonly onToggle: () => void;
  readonly strings: Strings;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      className="mt-1.5 inline-flex cursor-pointer items-center gap-1 text-xs opacity-75 transition-opacity hover:opacity-100"
    >
      <ChevronIcon
        className={`size-3 transition-transform ${open ? "rotate-180" : ""}`}
      />
      {open ? strings.transcriptCollapse : strings.transcriptExpand}
    </button>
  );
}

/** 系统生成的 user 角色消息块：渲染为事件卡而非用户气泡。 */
function isMetaBlock(block: TranscriptBlock): boolean {
  return (
    block.kind === "taskNotification" ||
    block.kind === "historySummary" ||
    block.kind === "continueNotice"
  );
}

/**
 * 用户中断标记的分隔线（codex 的 <turn_aborted>）：红色系——"上一轮到此
 * 为止"。中断意味着上一轮的回答戛然而止，运行中的命令可能仍在后台、
 * 已执行的工具可能只执行了一部分。
 */
function TurnAbortedDivider({ strings }: { readonly strings: Strings }) {
  // 样式对齐「上下文已压缩」分隔线（bg-border 细线 + ink-muted 裸文字）：
  // 同结构、仅配色换成红色系。
  return (
    <div className="flex items-center gap-2" role="note">
      <span className="h-px flex-1 bg-danger/25" />
      <span className="text-xs text-danger">{strings.transcriptTurnAborted}</span>
      <span className="h-px flex-1 bg-danger/25" />
    </div>
  );
}

/**
 * 工具注入的会话上下文卡（codex 的 environment_context / skills_instructions
 * 等包裹消息）：**一条源消息一张卡**——同一条消息的多个注入段共用一张卡，
 * 与其他消息之间留时间线间距。内容是给模型的 XML 指令，用与 workbuddy
 * 用户消息原文同一套元素折叠树呈现，元素默认全部折叠（chip 显示隐藏行数），
 * 点开看细节；卡头一行弱化文案 + 字符数。
 */
function HarnessCard({
  blocks,
  strings,
}: {
  readonly blocks: readonly TranscriptBlock[];
  readonly strings: Strings;
}) {
  // 元素级折叠：路径 → 是否收起。默认全折叠，覆盖项记用户动过的元素。
  const [foldOverride, setFoldOverride] = useState<Record<string, boolean>>({});
  const toggleFold = (path: string, next: boolean) =>
    setFoldOverride((prev) => ({ ...prev, [path]: next }));
  const tree = buildOriginalTree(blocks, []);
  const totalChars = blocks.reduce(
    (count, block) =>
      count + (block.kind === "harnessContext" ? block.text.length : 0),
    0,
  );
  return (
    <div className="rounded-card bg-surface-subtle px-4 py-3">
      <div className="mb-2 flex items-center gap-2 text-xs text-ink-muted">
        <span>{strings.transcriptHarnessContext}</span>
        <span className="ml-auto shrink-0 font-mono tabular-nums">
          {totalChars.toLocaleString("en-US")}
        </span>
      </div>
      {renderOriginalTree(tree, foldOverride, "0", false, strings, toggleFold)}
    </div>
  );
}

/**
 * 系统生成消息的事件卡：任务通知 = 状态徽章 + 摘要一行 + 可展开原文；
 * 压缩摘要 = 分隔线 + 可展开的摘要原文卡（后续"继续对话"指令已由核心层
 * 并入摘要，这里只在标题上提示）；继续指令（未并上时的兜底）= 居中弱化
 * 说明。
 */
function MetaEntryCard({
  entry,
  strings,
}: {
  readonly entry: TranscriptEntry;
  readonly strings: Strings;
}) {
  return (
    <div className="space-y-2.5">
      {entry.blocks.map((block, index) => {
        if (block.kind === "taskNotification") {
          return (
            <TaskNotificationCard
              key={index}
              block={block}
              tsMs={entry.tsMs}
              strings={strings}
            />
          );
        }
        if (block.kind === "historySummary") {
          return (
            <HistorySummaryCard
              key={index}
              block={block}
              tsMs={entry.tsMs}
              strings={strings}
            />
          );
        }
        if (block.kind === "continueNotice") {
          return (
            <p
              key={index}
              className="text-center text-xs leading-relaxed text-ink-muted"
            >
              {strings.transcriptContinueNotice}
            </p>
          );
        }
        return null;
      })}
    </div>
  );
}

/** 任务状态徽章配色：completed 绿、failed 红，其余状态不给色。 */
function statusTone(status: string): string {
  if (status === "completed") return "text-ok";
  if (status === "failed") return "text-danger";
  return "text-ink-muted";
}

function TaskNotificationCard({
  block,
  tsMs,
  strings,
}: {
  readonly block: Extract<TranscriptBlock, { kind: "taskNotification" }>;
  readonly tsMs: number | null;
  readonly strings: Strings;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="relative rounded-card border border-border bg-surface-subtle px-4 py-3">
      {/* 右下角骑缝的状态圆徽：completed 绿勾 / failed 红叉，其余不给。 */}
      {(block.status === "completed" || block.status === "failed") && (
        <div
          className={`absolute -bottom-2 -right-2 flex size-6 items-center justify-center rounded-full ${
            block.status === "completed" ? "bg-ok" : "bg-danger"
          }`}
        >
          <svg viewBox="0 0 14 14" className="size-3.5" aria-hidden>
            {block.status === "completed" ? (
              <path
                d="M3 7.5 L6 10.5 L11 4"
                fill="none"
                stroke="white"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            ) : (
              <path
                d="M4 4 L10 10 M10 4 L4 10"
                stroke="white"
                strokeWidth="1.8"
                strokeLinecap="round"
              />
            )}
          </svg>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2 text-xs text-ink-muted">
        <span>
          {strings.transcriptRoleSystem} · {strings.transcriptTaskNotification}
        </span>
        <span className={`rounded-control px-1.5 font-medium ${statusTone(block.status)}`}>
          {block.status === "completed"
            ? strings.transcriptTaskStatusCompleted
            : block.status === "failed"
              ? strings.transcriptTaskStatusFailed
              : block.status}
        </span>
        {tsMs !== null && (
          <span className="font-mono tabular-nums">{formatTimestamp(tsMs)}</span>
        )}
      </div>
      <p className="mt-1 truncate text-sm text-ink">
        {block.summary !== "" ? block.summary : block.taskId}
      </p>
      <CollapseToggle
        open={open}
        onToggle={() => setOpen(!open)}
        strings={strings}
      />
      {open && (
        <p className="mt-1 text-xs leading-relaxed break-words whitespace-pre-wrap text-ink-muted">
          {block.text}
        </p>
      )}
    </div>
  );
}

function HistorySummaryCard({
  block,
  tsMs,
  strings,
}: {
  readonly block: Extract<TranscriptBlock, { kind: "historySummary" }>;
  readonly tsMs: number | null;
  readonly strings: Strings;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      {/* 压缩分隔线：时间线上"此处发生上下文压缩"的标记。 */}
      <div className="mb-2 flex items-center gap-2">
        <span className="h-px flex-1 bg-border" />
        <span className="text-xs text-ink-muted">
          {strings.transcriptCompacted}
        </span>
        <span className="h-px flex-1 bg-border" />
      </div>
      <div className="rounded-card bg-surface-subtle px-4 py-3">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          className="flex w-full cursor-pointer items-center gap-2 text-left"
        >
          <ChevronIcon
            className={`size-3 shrink-0 transition-transform ${open ? "rotate-180" : ""}`}
          />
          <span className="text-xs font-medium text-ink">
            {strings.transcriptHistorySummary}
          </span>
          {tsMs !== null && (
            <span className="font-mono text-xs tabular-nums text-ink-muted">
              {formatTimestamp(tsMs)}
            </span>
          )}
          {block.hasContinue && (
            <span className="min-w-0 truncate text-xs text-ink-muted">
              · {strings.transcriptWithContinue}
            </span>
          )}
        </button>
        {open && (
          <div className="mt-2 border-l-2 border-border pl-3">
            <MarkdownView text={block.text} strings={strings} />
          </div>
        )}
      </div>
    </div>
  );
}

function BlockView({
  block,
  strings,
  onAccent = false,
}: {
  readonly block: TranscriptBlock;
  readonly strings: Strings;
  /** 在强调色气泡内：正文用 on-accent 色，不再自带上底色。 */
  readonly onAccent?: boolean;
}) {
  // 长内容卡（工具调用参数 / 工具结果）的折叠态：默认收起，展开后可再收起。
  const [expanded, setExpanded] = useState(false);
  switch (block.kind) {
    case "text":
      // accent 气泡内的正文保持原样（用户气泡有自己的口径切换）；助手
      // 文本卡走 Markdown 双口径（渲染/源码），折叠阈值与工具结果一致
      //（400 字符 / 12 行）：超长默认收起只露头部，渲染/源码切换 chip
      // 在卡片顶部不受钳高影响，展开后可再收起。
      return onAccent ? (
        <CollapsibleText text={block.text} strings={strings} />
      ) : (() => {
        const mdLong =
          block.text.length > COLLAPSE_CHARS ||
          block.text.split("\n").length > RESULT_PREVIEW_LINES;
        return (
          <div className="w-fit max-w-[85%] rounded-card bg-surface-subtle px-4 py-3">
            <div className={mdLong && !expanded ? "max-h-40 overflow-hidden" : ""}>
              <MarkdownView text={block.text} strings={strings} />
            </div>
            {mdLong && (
              <CollapseToggle
                open={expanded}
                onToggle={() => setExpanded(!expanded)}
                strings={strings}
              />
            )}
          </div>
        );
      })();
    case "thinking": {
      // 方案 1 · 摘要行升级：保留左竖线样式，收起行给 chevron + 字数 +
      // 首行内容预览（取第一个非空行，截 64 字），展开后预览让位给全文。
      const chars = block.text.length;
      const firstLine =
        block.text.split("\n").find((line) => line.trim() !== "") ?? "";
      const preview =
        firstLine.length > 64 ? `${firstLine.slice(0, 64)}…` : firstLine;
      return (
        <div className="border-l-2 border-border pl-2.5">
          <button
            type="button"
            onClick={() => setExpanded(!expanded)}
            className="flex w-fit max-w-full cursor-pointer items-center gap-1.5 text-left text-xs text-ink-muted select-none transition-colors hover:text-ink"
          >
            <ChevronIcon
              className={`size-3 shrink-0 transition-transform ${
                expanded ? "rotate-180" : ""
              }`}
            />
            <span className="shrink-0">
              {strings.transcriptThinkingCount.replace(
                "{count}",
                chars.toLocaleString("en-US"),
              )}
            </span>
            {!expanded && preview !== "" && (
              <span className="min-w-0 truncate">{preview}</span>
            )}
          </button>
          {expanded && (
            <p className="mt-1 text-sm leading-relaxed break-words whitespace-pre-wrap text-ink-muted italic">
              {block.text}
            </p>
          )}
        </div>
      );
    }
    case "toolCall": {
      // 参数超阈值即默认折叠：收起时钳制高度只露头部，展开看全量。
      const argsLong =
        block.arguments !== null &&
        (block.arguments.length > COLLAPSE_CHARS ||
          block.arguments.split("\n").length > COLLAPSE_LINES);
      return (
        <div className="w-fit max-w-[85%] rounded-card bg-surface-subtle px-3.5 py-3">
          <p className="text-xs text-ink-muted">{strings.transcriptToolCall}</p>
          {block.name !== null && (
            <span className="mt-1.5 inline-flex rounded-control bg-accent/10 px-1.5 py-0.5 text-xs font-medium text-accent">
              {block.name}
            </span>
          )}
          {block.arguments !== null && block.arguments !== "" && (
            <>
              <p className="mt-2 text-xs text-ink-muted">
                {strings.transcriptParams}
              </p>
              {/* 收起态用 max-h 钳高（pre-wrap 内容会换行，纵向钳制即可），
                  展开态才放开横向滚动——line-clamp 的 overflow:hidden 会和
                  overflow-x-auto 打架，不用它。 */}
              <pre
                className={`mt-1 rounded-control bg-surface px-2.5 py-1.5 font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-ink ${
                  argsLong && !expanded
                    ? "max-h-40 overflow-hidden"
                    : "overflow-x-auto"
                }`}
              >
                {block.arguments}
              </pre>
              {argsLong && (
                <CollapseToggle
                  open={expanded}
                  onToggle={() => setExpanded(!expanded)}
                  strings={strings}
                />
              )}
            </>
          )}
        </div>
      );
    }
    case "toolResult": {
      // 超阈值默认折叠：收起时钳制高度只露头部几行，展开看全量（仍以
      // RESULT_MAX_LINES 截断防极端巨块），展开后可再收起。
      const lines = block.content.split("\n");
      const long =
        lines.length > RESULT_PREVIEW_LINES ||
        block.content.length > COLLAPSE_CHARS;
      const shown = expanded
        ? lines.slice(0, RESULT_MAX_LINES)
        : lines.slice(0, RESULT_PREVIEW_LINES);
      return (
        <div
          className={`w-fit max-w-[85%] rounded-card px-3.5 py-3 ${
            block.isError ? "bg-danger/10" : "bg-surface-subtle"
          }`}
        >
          <p
            className={`text-xs ${
              block.isError ? "text-danger" : "text-ink-muted"
            }`}
          >
            {strings.transcriptToolResult}
          </p>
          {block.content !== "" && (
            <div
              className={`grid grid-cols-[auto_1fr] gap-x-3 font-mono text-xs leading-5 ${
                block.isError ? "text-danger" : "text-ink"
              } ${long && !expanded ? "max-h-40 overflow-hidden" : ""}`}
            >
              {shown.map((line, index) => (
                <Fragment key={index}>
                  <span className="tabular-nums text-ink-muted/60 select-none">
                    {index + 1}
                  </span>
                  <span className="break-all whitespace-pre-wrap">{line}</span>
                </Fragment>
              ))}
            </div>
          )}
          {lines.length > shown.length && (
            <p className="mt-1 text-xs text-ink-muted">…</p>
          )}
          {long && (
            <CollapseToggle
              open={expanded}
              onToggle={() => setExpanded(!expanded)}
              strings={strings}
            />
          )}
        </div>
      );
    }
    case "raw":
      return (
        <details>
          <summary className="cursor-pointer list-none text-xs text-ink-muted select-none hover:text-ink">
            {strings.transcriptRawBlock}
          </summary>
          <pre className="mt-1 overflow-x-auto rounded-control bg-surface px-2.5 py-1.5 font-mono text-xs leading-relaxed break-words whitespace-pre-wrap text-ink-muted">
            {block.json}
          </pre>
        </details>
      );
    case "image": {
      // 内嵌显示原图（Tauri asset 协议，scope 限定 WorkBuddy 附件目录）；
      // 非运行时/加载失败退回元数据卡。图片内容不读不传、绝不落库。
      return <ImageAttachment block={block} strings={strings} onAccent={onAccent} />;
    }
    case "injected":
      // 注入块由 UserTurnBubble 随所属消息的气泡统一呈现（占比条 + 切换 +
      // 完整口径原文），不会走到 BlockView；此分支只为穷尽块类型。
      return null;
    case "taskNotification":
    case "historySummary":
    case "continueNotice":
      // 系统生成消息块由 StreamEntry 的 MetaEntryCard 承载；此分支只为
      // 穷尽块类型（防御路径）。
      return null;
    case "harnessContext":
      // 纯包裹消息由 StreamEntry 的 HarnessCard 承载；混在真实消息里的
      // 包裹段走这里，同样折叠成单块卡。
      return <HarnessCard blocks={[block]} strings={strings} />;
    case "turnAborted":
      // 纯中断消息由 StreamEntry 的分隔线承载；混合场景走这里同款。
      return <TurnAbortedDivider strings={strings} />;
  }
}
