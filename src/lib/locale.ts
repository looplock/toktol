/**
 * 界面语言（纯逻辑）：文案从哪张表取的唯一入口。
 * 加语言：LOCALES 加值 + strings.ts 的 TABLE 补表（少一张编译报错）。
 */

import { createContext, useContext } from "react";

import { LS_KEY_LOCALE } from "../constants";

/** 可用的界面语言。 */
export const LOCALES = ["zh-CN", "en"] as const;

/** 界面语言类型。 */
export type Locale = (typeof LOCALES)[number];

/** 缺省语言。 */
export const DEFAULT_LOCALE: Locale = "zh-CN";

/** 把任意字符串解析成语言；不认识的值返回 `null`（不猜，不回落）。 */
export function parseLocale(raw: string | null | undefined): Locale | null {
  return LOCALES.find((locale) => locale === raw) ?? null;
}

/**
 * 读取用户上次选择的语言。
 *
 * `localStorage` 不可用（隐私模式、被禁用）时静默返回 `null`，由调用方回落到缺省值——
 * 语言偏好读不到不该影响本次会话。
 */
export function readStoredLocale(
  storage: Pick<Storage, "getItem"> = localStorage,
): Locale | null {
  try {
    return parseLocale(storage.getItem(LS_KEY_LOCALE));
  } catch {
    return null;
  }
}

export function storeLocale(
  locale: Locale,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  try {
    storage.setItem(LS_KEY_LOCALE, locale);
  } catch {
    // 静默跳过：写不进去也不该影响本次会话。
  }
}

export const LocaleContext = createContext<Locale>(DEFAULT_LOCALE);

/** 取当前界面语言：Intl 格式化的第一参。日期/时间不落回宿主 locale——
 * 宿主语言与界面语言无关（同 numberUnit 的 context 下发，设置页走显式 props）。 */
export function useLocale(): Locale {
  return useContext(LocaleContext);
}
