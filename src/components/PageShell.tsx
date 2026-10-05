/**
 * 页面外壳：共用的「标题 + 说明 + 内容」骨架。页头可省（如明细页直接顶格放表格），
 * 只管版式，真实内容替换 children 即可。
 */

import type { ReactNode } from "react";

interface PageShellProps {
  readonly title?: string;
  readonly description?: string;
  readonly children?: ReactNode;
  /** true 时占满父容器高度，内容区交给调用方做弹性布局（如表格内部滚动）。 */
  readonly fill?: boolean;
}

export function PageShell({ title, description, children, fill = false }: PageShellProps) {
  return (
    <section className={`flex flex-col gap-5 ${fill ? "h-full" : ""}`}>
      {title === undefined ? null : (
        <header className="shrink-0">
          <h1 className="text-lg font-semibold tracking-tight">{title}</h1>
          {description === undefined ? null : (
            <p className="mt-1 max-w-3xl text-sm leading-6 text-ink-muted">{description}</p>
          )}
        </header>
      )}

      {children}
    </section>
  );
}
