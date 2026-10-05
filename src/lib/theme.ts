/**
 * 主题底座（纯逻辑）。明暗只有这一维，用户存的是"偏好"（含跟随系统），
 * 落到 DOM 的是**解析后**的明暗——两者分开是因为跟随系统要实时跟 OS 走。
 * 强调色是第二个维度：一组精选预设（data-accent）。
 */

import { LS_KEY_ACCENT, LS_KEY_THEME } from "../constants";

export const THEME_MODES = ["light", "dark", "system"] as const;

/** 明暗偏好：`system` 表示跟 OS 走。 */
export type ThemeMode = (typeof THEME_MODES)[number];

export const DEFAULT_THEME_MODE: ThemeMode = "system";

/** 强调色预设。落在 <html data-accent> 上，与 tokens.css 的覆盖块一一对应；
 * 默认靛蓝不写块，恢复默认等于删属性。预设避开涨跌的红绿色相——
 * 真绿/青绿系原本进不来：暗档一亮起来色相就漂进涨跌绿的家族。green（正淡绿）
 * 是用户拍板的唯一例外：与"跌"同色相的语义风险已知并接受，校验脚本按预设名豁免。 */
export const ACCENT_IDS = [
  "indigo",
  "blue",
  "cyan",
  "slate",
  "purple",
  "fuchsia",
  "pink",
  "amber",
  "yellow",
  "green",
] as const;

/** 强调色：按钮、选中态与焦点环的色相。 */
export type AccentId = (typeof ACCENT_IDS)[number];

export const DEFAULT_ACCENT: AccentId = "indigo";

/** 设置页色板的取色令牌（定义于 tokens.css 的 :root，不随明暗变）。 */
export const ACCENT_SWATCH_VARS: Record<AccentId, string> = {
  indigo: "--tt-accent-preview-indigo",
  blue: "--tt-accent-preview-blue",
  cyan: "--tt-accent-preview-cyan",
  slate: "--tt-accent-preview-slate",
  purple: "--tt-accent-preview-purple",
  fuchsia: "--tt-accent-preview-fuchsia",
  pink: "--tt-accent-preview-pink",
  amber: "--tt-accent-preview-amber",
  yellow: "--tt-accent-preview-yellow",
  green: "--tt-accent-preview-green",
};

/** 落到 DOM 上的明暗，只有两态。 */
export type ResolvedMode = "light" | "dark";

function parseOption<T extends string>(
  allowed: readonly T[],
  raw: string | null | undefined,
): T | null {
  return allowed.find((option) => option === raw) ?? null;
}

export function parseThemeMode(raw: string | null | undefined): ThemeMode | null {
  return parseOption(THEME_MODES, raw);
}

export function parseAccent(raw: string | null | undefined): AccentId | null {
  return parseOption(ACCENT_IDS, raw);
}

/** 显式选择优先，`system` 跟 OS 走。 */
export function resolveMode(mode: ThemeMode, prefersDark: boolean): ResolvedMode {
  if (mode === "light") return "light";
  if (mode === "dark") return "dark";

  return prefersDark ? "dark" : "light";
}

/** 顶栏按钮的循环顺序：亮 → 暗 → 跟随系统 → 亮。三态是上限，再多加一态就要按四次才转回来。 */
export function nextThemeMode(mode: ThemeMode): ThemeMode {
  const index = THEME_MODES.indexOf(mode);

  return THEME_MODES[(index + 1) % THEME_MODES.length] ?? DEFAULT_THEME_MODE;
}

export function applyTheme(
  mode: ResolvedMode,
  root: HTMLElement = document.documentElement,
): void {
  root.dataset["theme"] = mode;
}

/** 默认色删属性而非写 "indigo"：恢复默认与"从未设置"在 DOM 上是同一个状态。 */
export function applyAccent(
  accent: AccentId,
  root: HTMLElement = document.documentElement,
): void {
  if (accent === DEFAULT_ACCENT) {
    delete root.dataset["accent"];
  } else {
    root.dataset["accent"] = accent;
  }
}

function readStored(key: string, storage: Pick<Storage, "getItem">): string | null {
  try {
    return storage.getItem(key);
  } catch {
    return null;
  }
}

function writeStored(
  key: string,
  value: string,
  storage: Pick<Storage, "setItem">,
): void {
  try {
    storage.setItem(key, value);
  } catch {
    // 静默跳过：写不进去也不该影响本次会话。
  }
}

export function readStoredThemeMode(
  storage: Pick<Storage, "getItem"> = localStorage,
): ThemeMode | null {
  return parseThemeMode(readStored(LS_KEY_THEME, storage));
}

export function storeThemeMode(
  mode: ThemeMode,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  writeStored(LS_KEY_THEME, mode, storage);
}

/** 读出用户存的偏好；没存过或存了不认识的值就跟随系统。 */
export function readStoredThemeSettings(
  storage: Pick<Storage, "getItem"> = localStorage,
): ThemeMode {
  return readStoredThemeMode(storage) ?? DEFAULT_THEME_MODE;
}

export function readStoredAccent(
  storage: Pick<Storage, "getItem"> = localStorage,
): AccentId | null {
  return parseAccent(readStored(LS_KEY_ACCENT, storage));
}

export function storeAccent(
  accent: AccentId,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  writeStored(LS_KEY_ACCENT, accent, storage);
}

/** 读出用户存的强调色；没存过或存了不认识的值就是默认靛蓝。 */
export function readStoredAccentSettings(
  storage: Pick<Storage, "getItem"> = localStorage,
): AccentId {
  return readStoredAccent(storage) ?? DEFAULT_ACCENT;
}
