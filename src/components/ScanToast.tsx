/**
 * 扫描气泡：右下角的小胶囊，扫描期间跟着转，结束后报这一轮进了多少条记录，
 * 几秒后自行消失。只读提示——不接点击也不吃 pointer 事件（pointer-events-none），
 * 否则它会在右下角挡住内容区的滚动与点击。
 * 文案与条数由调用方传：组件不认 i18n，与其余组件同一约定。
 */

import type { ScanToastKind } from "../lib/scanToast";

interface ScanToastProps {
  readonly kind: ScanToastKind;
  readonly title: string;
  /** done 时的收成说明（"新增 N 条记录"）；其余形态不传。 */
  readonly detail?: string;
}

const ICON_PROPS = {
  viewBox: "0 0 24 24",
  width: 14,
  height: 14,
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
  focusable: false,
} as const;

/** 图标着色即形态语义：进行中是强调色，成功与失败走各自的结果色（不借涨跌色）。 */
const KIND_COLOR: Record<ScanToastKind, string> = {
  running: "text-accent",
  done: "text-ok",
  failed: "text-danger",
};

function ToastIcon({ kind }: { readonly kind: ScanToastKind }) {
  if (kind === "done") {
    return (
      <svg {...ICON_PROPS}>
        <path d="M20 6 9 17l-5-5" />
      </svg>
    );
  }
  if (kind === "failed") {
    return (
      <svg {...ICON_PROPS}>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 8v4" />
        <path d="M12 16h.01" />
      </svg>
    );
  }

  // 进行中：与顶栏扫描按钮同一个转圈箭头——同一个意思在两处长成两种样子最迷惑人。
  return (
    <svg {...ICON_PROPS} className="animate-spin">
      <path d="M21 12a9 9 0 1 1-2.64-6.36" />
      <path d="M21 3v6h-6" />
    </svg>
  );
}

export function ScanToast({ kind, title, detail }: ScanToastProps) {
  return (
    <div
      role="status"
      aria-live="polite"
      data-scan-toast={kind}
      /* fixed 定位：钉在视口右下角，与所属页面无关。 animate 用 tt-fade-in
       * 而不是自造关键帧，动效抑制在 tokens.css 里统一兜住。 */
      className={`pointer-events-none fixed right-4 bottom-4 z-50 flex max-w-[18rem] items-center gap-2 rounded-full border border-border bg-surface-raised px-3 py-1.5 text-xs shadow-raised animate-[tt-fade-in_160ms_ease-out] ${KIND_COLOR[kind]}`}
    >
      <ToastIcon kind={kind} />
      {/* 两行文字各自定色：容器的颜色是给图标的，不该把正文也染掉。 */}
      <span className="text-ink-strong">{title}</span>
      {detail === undefined ? null : <span className="text-ink-muted">{detail}</span>}
    </div>
  );
}
