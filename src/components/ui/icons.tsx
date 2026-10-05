/**
 * 通用小图标（自绘 SVG，随 currentColor 变色）：搜索、下拉箭头、复选勾。
 * 尺寸由调用方用 size-* 类控制。
 */

import type { ReactNode } from "react";

export function SearchIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <circle cx="11" cy="11" r="7" />
      <path d="m20 20-3.5-3.5" />
    </svg>
  );
}

/** 图片：附件占位卡。 */
export function ImageIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <circle cx="9" cy="10" r="1.5" fill="currentColor" stroke="none" />
      <path d="m5 19 5.5-6 4 4.5L18 14l3 3.5" />
    </svg>
  );
}

/** 文件夹：文件树目录项。 */
export function FolderIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
    </svg>
  );
}

/** 文件夹（开口）：文件树已展开的目录项。 */
export function FolderOpenIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" />
    </svg>
  );
}

/** 文档（折角）：文件树文件项。 */
export function FileIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
      <path d="M14 2v4a2 2 0 0 0 2 2h4" />
    </svg>
  );
}

/** 关闭（×）：浮层关闭按钮。 */
export function CloseIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      className={className}
    >
      <path d="M18 6 6 18M6 6l12 12" />
    </svg>
  );
}

export function ChevronIcon({
  className,
  up = false,
  left = false,
  right = false,
}: {
  readonly className?: string;
  readonly up?: boolean;
  readonly left?: boolean;
  readonly right?: boolean;
}) {
  const path = up
    ? "m6 15 6-6 6 6"
    : left
      ? "m15 6-6 6 6 6"
      : right
        ? "m9 6 6 6-6 6"
        : "m6 9 6 6 6-6";
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d={path} />
    </svg>
  );
}

/** 日历：时间区间选择入口。 */
export function CalendarIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <rect x="3" y="5" width="18" height="16" rx="2" />
      <path d="M3 10h18" />
      <path d="M8 3v4M16 3v4" />
    </svg>
  );
}

export function CheckIcon({ className }: { readonly className?: string }) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="3"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      <path d="m5 12.5 4.5 4.5L19 7" />
    </svg>
  );
}

/** 双 chevron：分页条跳首/跳末用。direction 取 left / right。 */
export function ChevronsIcon({
  className,
  direction,
}: {
  readonly className?: string;
  readonly direction: "left" | "right";
}) {
  return (
    <svg
      aria-hidden="true"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
    >
      {direction === "left" ? (
        <>
          <path d="m11 7-5 5 5 5" />
          <path d="m18 7-5 5 5 5" />
        </>
      ) : (
        <>
          <path d="m13 7 5 5-5 5" />
          <path d="m6 7 5 5-5 5" />
        </>
      )}
    </svg>
  );
}

// ── 网关页图标（同上：currentColor、size-* 类控尺寸）──────────────

function gateway(path: ReactNode) {
  return function GatewayIcon({ className }: { readonly className?: string }) {
    return (
      <svg
        aria-hidden="true"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinecap="round"
        strokeLinejoin="round"
        className={className}
      >
        {path}
      </svg>
    );
  };
}

/** 入口：门 + 内向箭头。 */
export const EntranceIcon = gateway(
  <>
    <path d="M9 4H6a2 2 0 0 0-2 2v12a2 2 0 0 0 2 2h3" />
    <path d="m15 8 4 4-4 4" />
    <path d="M19 12H9" />
  </>,
);

/** 上游：层叠两栏。 */
export const UpstreamIcon = gateway(
  <>
    <rect x="3" y="4" width="18" height="6" rx="2" />
    <rect x="3" y="14" width="18" height="6" rx="2" />
  </>,
);

/** 模型映射：双向箭头。 */
export const MappingIcon = gateway(
  <>
    <path d="M4 8h13" />
    <path d="m13 4 4 4-4 4" />
    <path d="M20 16H7" />
    <path d="m11 12-4 4 4 4" />
  </>,
);

/** 流量：上行折线。 */
export const TrafficIcon = gateway(
  <>
    <path d="M3 18l6-6 4 3 8-9" />
    <path d="M15 6h6v6" />
  </>,
);

/** 复制。 */
export const CopyIcon = gateway(
  <>
    <rect x="9" y="9" width="12" height="12" rx="2" />
    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
  </>,
);

/** 眼睛（明文切换）。 */
export const EyeIcon = gateway(
  <>
    <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7z" />
    <circle cx="12" cy="12" r="3" />
  </>,
);

/** 播放（启动服务）。 */
export const PlayIcon = gateway(<path d="M7 4l13 8-13 8V4z" fill="currentColor" stroke="none" />);

/** 重置（循环箭头）。 */
export const ResetIcon = gateway(
  <>
    <path d="M21 12a9 9 0 1 1-2.64-6.36L21 8" />
    <path d="M21 3v5h-5" />
  </>,
);

/** 停止（实心方块）。 */
export const StopIcon = gateway(
  <rect x="5" y="5" width="14" height="14" rx="2" fill="currentColor" stroke="none" />,
);

/** 回收站（垃圾桶）：会话批量删除入口。 */
export const TrashIcon = gateway(
  <>
    <path d="M3 6h18" />
    <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
    <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
    <path d="M10 11v6M14 11v6" />
  </>,
);

/** 会话页目录切换：列表式（编号 + 摘要行）。 */
/** 编辑（铅笔）。 */
export const PencilIcon = gateway(
  <>
    <path d="M17 3a2.8 2.8 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z" />
  </>,
);
