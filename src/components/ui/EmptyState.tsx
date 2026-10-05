/**
 * 空态。文案由调用方传——组件不认 i18n，和其余组件保持一致。
 * 虚线边框是为了和"有数据但都是 0"区分开：空是没东西，不是零。
 */

import type { ReactNode } from "react";

import { Card } from "./Card";

interface EmptyStateProps {
  readonly children: ReactNode;
}

export function EmptyState({ children }: EmptyStateProps) {
  return (
    <Card
      dashed
      padding="none"
      className="px-6 py-12 text-center text-sm text-ink-muted"
    >
      {children}
    </Card>
  );
}
