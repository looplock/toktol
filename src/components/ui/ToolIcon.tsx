/**
 * 工具图标：按 tool id 从 TOOL_ITEMS（tools.ts）取内联 SVG 渲染，
 * 无图标时回落 badge 底色 + 首字母徽章。与设置页同一数据源，
 * 保证两处显示一致。
 */

import { TOOL_ITEMS } from "../../lib/tools";

export function ToolIcon({
  toolId,
  size = 16,
}: {
  readonly toolId: string;
  readonly size?: number;
}) {
  const tool = TOOL_ITEMS.find((item) => item.id === toolId);
  if (tool === undefined) return null;
  const box = `size-${sizeToClass(size)}`;
  if (tool.icon !== undefined) {
    return (
      <span
        aria-hidden="true"
        className={`inline-flex shrink-0 items-center justify-center [&_svg]:size-4 ${box}`}
        dangerouslySetInnerHTML={{ __html: tool.icon }}
      />
    );
  }
  return (
    <span
      aria-hidden="true"
      style={{ backgroundColor: tool.badge }}
      className={`inline-flex shrink-0 items-center justify-center rounded-control text-[9px] font-semibold text-white ${box}`}
    >
      {tool.label.charAt(0)}
    </span>
  );
}

/** Tailwind 任意值：size-4/size-5 常规档，其余走 size-[Npx]。 */
function sizeToClass(size: number): string {
  if (size === 16) return "4";
  if (size === 20) return "5";
  return `[${size}px]`;
}
