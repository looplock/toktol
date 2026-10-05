/**
 * Markdown 双口径视图：渲染（react-markdown + GFM，默认）/ 源码（原文
 * pre-wrap），顶部自带 渲染/源码 切换 chip（样式与注入占比条的切换一致）。
 * 原生 HTML 被 react-markdown 默认转义，不引入 XSS 面；链接渲染为下划线
 * 文本——点击让 webview 整窗跳走，不值得。
 */

import { useState } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Strings } from "../../i18n/strings";

const mdComponents: Components = {
  h1: ({ node: _node, ...props }) => (
    <h1 {...props} className="mt-4 text-base font-semibold first:mt-0" />
  ),
  h2: ({ node: _node, ...props }) => (
    <h2 {...props} className="mt-4 text-sm font-semibold first:mt-0" />
  ),
  h3: ({ node: _node, ...props }) => (
    <h3 {...props} className="mt-3 text-sm font-semibold first:mt-0" />
  ),
  h4: ({ node: _node, ...props }) => (
    <h4 {...props} className="mt-3 text-sm font-medium first:mt-0" />
  ),
  p: ({ node: _node, ...props }) => (
    <p {...props} className="my-2 text-sm leading-relaxed first:mt-0 last:mb-0" />
  ),
  ul: ({ node: _node, ...props }) => (
    <ul {...props} className="my-2 list-disc space-y-1 pl-5 text-sm leading-relaxed" />
  ),
  ol: ({ node: _node, ...props }) => (
    <ol {...props} className="my-2 list-decimal space-y-1 pl-5 text-sm leading-relaxed" />
  ),
  li: ({ node: _node, ...props }) => (
    <li {...props} className="leading-relaxed marker:text-ink-muted" />
  ),
  blockquote: ({ node: _node, ...props }) => (
    <blockquote
      {...props}
      className="my-2 border-l-2 border-border pl-3 text-sm leading-relaxed text-ink-muted"
    />
  ),
  pre: ({ node: _node, ...props }) => (
    <pre
      {...props}
      className="my-2 overflow-x-auto rounded-control bg-surface px-2.5 py-1.5 font-mono text-xs leading-relaxed text-ink"
    />
  ),
  // 围栏代码块（带 language- class）自身就在 pre 的底色里，不再叠底。
  code: ({ node: _node, className, children }) => (
    <code
      className={`${className ?? ""} font-mono text-xs ${
        className?.includes("language-") ? "" : "rounded-control bg-surface px-1 py-0.5 text-ink"
      }`}
    >
      {children}
    </code>
  ),
  a: ({ node: _node, ...props }) => (
    <span {...props} className="underline decoration-border underline-offset-2" />
  ),
  table: ({ node: _node, ...props }) => (
    <div className="my-2 overflow-x-auto">
      <table {...props} className="w-full border-collapse text-xs" />
    </div>
  ),
  th: ({ node: _node, ...props }) => (
    <th
      {...props}
      className="border border-border bg-surface-muted px-2 py-1 text-left font-medium"
    />
  ),
  td: ({ node: _node, ...props }) => (
    <td {...props} className="border border-border px-2 py-1 align-top" />
  ),
  hr: () => <hr className="my-3 border-border" />,
};

function MarkdownContent({ text }: { readonly text: string }) {
  return (
    <div className="min-w-0 text-ink">
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={mdComponents}>
        {text}
      </ReactMarkdown>
    </div>
  );
}

export function MarkdownView({
  text,
  strings,
}: {
  readonly text: string;
  readonly strings: Strings;
}) {
  const [view, setView] = useState<"render" | "source">("render");
  const chip = (selected: boolean) =>
    `rounded-control px-2 py-0.5 text-xs transition-colors ${
      selected ? "bg-accent text-accent-ink" : "text-ink-muted hover:text-ink"
    }`;
  return (
    <div className="min-w-0">
      <div
        className="mb-1.5 inline-flex items-center gap-0.5 rounded-control bg-surface-muted p-0.5"
        role="group"
        aria-label={strings.markdownToggle}
      >
        <button type="button" onClick={() => setView("render")} className={chip(view === "render")}>
          {strings.markdownRender}
        </button>
        <button type="button" onClick={() => setView("source")} className={chip(view === "source")}>
          {strings.markdownSource}
        </button>
      </div>
      {view === "render" ? (
        <MarkdownContent text={text} />
      ) : (
        <p className="text-sm leading-relaxed break-words whitespace-pre-wrap text-ink">
          {text}
        </p>
      )}
    </div>
  );
}
