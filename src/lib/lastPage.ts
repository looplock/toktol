/**
 * 上次停留页面的持久化：刷新或托盘重建窗口后回到离开时的页面，而不是总览。
 * 只存页面 id；跨页会话焦点（明细页点会话跳转）是一次性的，不持久化。
 */

import { LS_KEY_LAST_PAGE } from "../constants";
import { PAGE_ORDER, type PageId } from "./routes";

/** 把任意字符串解析成页面 id；不认识的值返回 `null`（不猜，不回落）。 */
export function parsePageId(raw: string | null | undefined): PageId | null {
  return PAGE_ORDER.find((page) => page === raw) ?? null;
}

/**
 * 读取上次停留的页面。
 *
 * `localStorage` 不可用（隐私模式、被禁用）时静默返回 `null`，由调用方回落到
 * 缺省页——上次停在哪读不到不该影响本次会话。
 */
export function readStoredPage(
  storage: Pick<Storage, "getItem"> = localStorage,
): PageId | null {
  try {
    return parsePageId(storage.getItem(LS_KEY_LAST_PAGE));
  } catch {
    return null;
  }
}

export function storePage(
  page: PageId,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  try {
    storage.setItem(LS_KEY_LAST_PAGE, page);
  } catch {
    // 静默跳过：写不进去也不该影响本次会话。
  }
}
