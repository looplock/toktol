/**
 * 顶栏的手动扫描按钮：亮暗切换旁边那颗。扫描中图标旋转并禁点——自动调度
 * 与手动共用一个通道，扫描中重复触发本来就是 no-op，禁点是把这个事实
 * 说给用户听。不认识 i18n（文案由调用方传），与 ThemeToggle 同一约定。
 */

interface ScanButtonProps {
  readonly scanning: boolean;
  /** 空闲时的无障碍名与悬停提示。 */
  readonly label: string;
  /** 扫描中的无障碍名与悬停提示（也是禁点的原因说明）。 */
  readonly scanningLabel: string;
  /** 最近一次扫描的失败信息；非空时进 title，用户能知道自动扫描出过事。 */
  readonly error?: string | null;
  readonly onClick: () => void;
}

const ICON_PROPS = {
  viewBox: "0 0 24 24",
  width: 16,
  height: 16,
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.75,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
  focusable: false,
} as const;

function RefreshIcon({ spinning }: { readonly spinning: boolean }) {
  return (
    <svg {...ICON_PROPS} className={spinning ? "animate-spin" : undefined}>
      <path d="M21 12a9 9 0 1 1-2.64-6.36" />
      <path d="M21 3v6h-6" />
    </svg>
  );
}

export function ScanButton({ scanning, label, scanningLabel, error, onClick }: ScanButtonProps) {
  const title =
    error !== null && error !== undefined && error !== ""
      ? `${scanning ? scanningLabel : label}（${error}）`
      : scanning
        ? scanningLabel
        : label;

  return (
    <button
      type="button"
      onClick={onClick}
      disabled={scanning}
      aria-label={title}
      title={title}
      className="grid size-8 shrink-0 place-items-center rounded-control text-ink-muted transition-colors hover:text-ink disabled:cursor-default disabled:text-ink-muted"
    >
      <RefreshIcon spinning={scanning} />
    </button>
  );
}
