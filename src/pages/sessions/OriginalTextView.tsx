/**
 * 注入块原文树的渲染：三色标签行、元素折叠 chip、缩进层级线。
 * 树的构建在 lib/transcriptOriginal.ts；图片行直接渲染 ImageAttachment
 * （进树时已收窄为 image 块），本文件不依赖消息流组件——依赖方向保持
 * TranscriptStream → 本文件 → lib 单向。
 */

import { Fragment } from "react";
import { ChevronIcon } from "../../components/ui/icons";
import { ImageAttachment } from "./ImageAttachment";
import type { Strings } from "../../i18n/strings";
import {
  TAG_TOKEN_RE,
  type OriginalRow,
  type OriginalTree,
} from "../../lib/transcriptOriginal";
import type { ReactNode } from "react";

/**
 * 单行原文的三色 token：标签名（含尖括号）、属性、正文。普通气泡用
 * accent / muted / ink 三档；accent 气泡内只有 accent-ink 一个色相，
 * 用透明度与字重区分层级。
 */
function tagSpans(line: string, onAccent: boolean): ReactNode {
  const tagClass = onAccent ? "text-accent-ink font-medium" : "text-accent";
  const attrClass = onAccent ? "text-accent-ink/60" : "text-ink-muted";
  const textClass = onAccent ? "text-accent-ink/85" : "";
  const spans: ReactNode[] = [];
  let key = 0;
  let cursor = 0;
  for (const match of line.matchAll(TAG_TOKEN_RE)) {
    const start = match.index ?? 0;
    if (start > cursor) {
      spans.push(
        <span key={key++} className={textClass}>
          {line.slice(cursor, start)}
        </span>,
      );
    }
    if (match[5] !== undefined) {
      spans.push(
        <span key={key++} className={textClass}>
          {match[5]}
        </span>,
      );
    } else {
      spans.push(
        <span key={key++} className={tagClass}>
          {match[1]}
          {match[2]}
        </span>,
      );
      if (match[3]) {
        spans.push(
          <span key={key++} className={attrClass}>
            {match[3]}
          </span>,
        );
      }
      if (match[4]) {
        spans.push(
          <span key={key++} className={tagClass}>
            {match[4]}
          </span>,
        );
      }
    }
    cursor = start + match[0].length;
  }
  if (cursor < line.length) {
    spans.push(
      <span key={key++} className={textClass}>
        {line.slice(cursor)}
      </span>,
    );
  }
  return spans;
}

/** 原文行样式：缩进 > 0 时画左侧层级线。 */
const originalLineClass = (indent: number, onAccent: boolean) =>
  "text-sm leading-relaxed break-words whitespace-pre-wrap" +
  (indent > 0
    ? ` border-l ${onAccent ? "border-accent-ink/25" : "border-border"}`
    : "");

function renderOriginalRow(
  row: OriginalRow,
  onAccent: boolean,
  strings: Strings,
  key: string,
): ReactNode {
  if (row.kind === "image") {
    return (
      <div key={key} style={{ marginLeft: row.indent * 12 }}>
        <ImageAttachment block={row.block} strings={strings} onAccent={onAccent} />
      </div>
    );
  }
  return (
    <div
      key={key}
      className={originalLineClass(row.indent, onAccent)}
      style={{ marginLeft: row.indent * 12 }}
    >
      {row.text === "" ? "\u00a0" : tagSpans(row.text, onAccent)}
    </div>
  );
}

/**
 * 渲染原文树：元素默认展开，用户收起后该元素只留一行摘要 chip
 * （chevron + 开标签预览 + 隐藏行数），children 与闭合行不渲染——折叠
 * 是真隐藏，不是视觉限高。展开态的开标签行带 chevron 可再收起。
 */
export function renderOriginalTree(
  nodes: readonly OriginalTree[],
  foldOverride: Readonly<Record<string, boolean>>,
  path: string,
  onAccent: boolean,
  strings: Strings,
  onToggle: (path: string, next: boolean) => void,
): ReactNode[] {
  return nodes.map((node, index) => {
    const key = `${path}.${index}`;
    if (node.kind === "row") {
      return renderOriginalRow(node.row, onAccent, strings, key);
    }
    // 所有元素默认折叠：完整/纯文本口径先给骨架（元素 chip 一览），
    // 点开看细节。foldOverride 只记用户动过的元素。
    const folded = foldOverride[key] ?? true;
    if (folded) {
      const preview =
        node.header.length > 64 ? `${node.header.slice(0, 64)}…` : node.header;
      return (
        <button
          key={key}
          type="button"
          onClick={() => onToggle(key, false)}
          className={`flex w-fit max-w-full cursor-pointer items-center gap-1.5 rounded-control px-2 py-0.5 text-left text-sm transition-colors ${
            onAccent
              ? "bg-accent-ink/10 text-accent-ink hover:bg-accent-ink/20"
              : "bg-surface-muted text-ink hover:bg-surface-subtle"
          }`}
          style={{ marginLeft: node.indent * 12 }}
        >
          <ChevronIcon className="size-3 shrink-0" />
          <span className="min-w-0 truncate">{tagSpans(preview, onAccent)}</span>
          {node.foldedRows > 0 && (
            <span
              className={`ml-auto shrink-0 pl-2 text-xs ${
                onAccent ? "text-accent-ink/70" : "text-ink-muted"
              }`}
            >
              {strings.transcriptFoldedRows.replace(
                "{count}",
                String(node.foldedRows),
              )}
            </span>
          )}
        </button>
      );
    }
    return (
      <Fragment key={key}>
        {/* chevron 绝对定位挂在行左侧留白区：开标签文本与内容行、闭合行
            严格同缩进，不因按钮而右移。 */}
        <div className="relative" style={{ marginLeft: node.indent * 12 }}>
          <button
            type="button"
            onClick={() => onToggle(key, true)}
            aria-label={strings.transcriptCollapse}
            className={`absolute -left-4 top-1.5 cursor-pointer p-0.5 opacity-75 transition-opacity hover:opacity-100 ${
              onAccent ? "text-accent-ink" : "text-ink-muted"
            }`}
          >
            <ChevronIcon className="size-3 rotate-180" />
          </button>
          <div className={originalLineClass(node.indent, onAccent)}>
            {tagSpans(node.header, onAccent)}
          </div>
        </div>
        {renderOriginalTree(
          node.children,
          foldOverride,
          key,
          onAccent,
          strings,
          onToggle,
        )}
        {node.footer !== null &&
          renderOriginalRow(
            { kind: "line", text: node.footer, indent: node.indent },
            onAccent,
            strings,
            `${key}.f`,
          )}
      </Fragment>
    );
  });
}
