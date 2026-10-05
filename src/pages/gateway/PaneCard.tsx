/**
 * 网关分区共享的壳与表单样式：右栏内容卡外壳 + 文本输入统一样式
 * （上游/模型映射的文本框共用，避免四个分区各抄一份）。
 */

import type { ReactNode } from "react";
import { Card } from "../../components/ui/Card";

/** 分区内容卡：右栏统一的外壳。 */
export function PaneCard({ children }: { children: ReactNode }) {
  return (
    <Card as="section" raised className="flex min-h-0 flex-col overflow-y-auto">
      {children}
    </Card>
  );
}

export const inputClass =
  "w-full rounded-control border border-border bg-surface px-2.5 py-1.5 text-sm focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring";
