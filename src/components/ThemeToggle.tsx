/**
 * 顶栏的明暗循环按钮：亮 → 暗 → 跟随系统 → 亮。
 * 不认识 i18n（文案由调用方传），也不自己决定顺序（`nextThemeMode` 在 lib/theme.ts，有单测）。
 *
 * 图标画的是**用户选的那一态**而不是此刻生效的明暗：跟随系统时它得跟另外两态长得不一样，
 * 否则用户看不出自己正处于"自动"这一档。三态各一个形状，不靠颜色区分。
 */

import { nextThemeMode, type ThemeMode } from "../lib/theme";

interface ThemeToggleProps {
  readonly mode: ThemeMode;
  readonly onChange: (mode: ThemeMode) => void;
  /** 无障碍名与悬停提示：当前是什么、按下去会变成什么。 */
  readonly label: string;
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

function SunIcon() {
  return (
    <svg {...ICON_PROPS}>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41" />
    </svg>
  );
}

function MoonIcon() {
  return (
    <svg {...ICON_PROPS}>
      <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z" />
    </svg>
  );
}

function SystemIcon() {
  return (
    <svg {...ICON_PROPS}>
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <path d="M8 21h8M12 17v4" />
    </svg>
  );
}

export function ThemeToggle({ mode, onChange, label }: ThemeToggleProps) {
  return (
    <button
      type="button"
      onClick={() => onChange(nextThemeMode(mode))}
      aria-label={label}
      title={label}
      className="grid size-8 shrink-0 place-items-center rounded-control text-ink-muted transition-colors hover:text-ink"
    >
      {mode === "light" ? <SunIcon /> : mode === "dark" ? <MoonIcon /> : <SystemIcon />}
    </button>
  );
}
