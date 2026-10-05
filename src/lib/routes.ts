/**
 * 页面路由（纯逻辑）。七个平级页面，不引路由库——
 * 等出现深链、前进后退或实例缓存需求再说。
 */

export const PAGE_ORDER = [
  "overview",
  "details",
  "sessions",
  "gateway",
  "pricing",
  "config",
  "settings",
] as const;

export type PageId = (typeof PAGE_ORDER)[number];

export const DEFAULT_PAGE: PageId = "overview";

/** 跨页焦点：明细页点会话 → 会话页定位并选中该会话。 */
export interface SessionFocus {
  readonly tool: string;
  readonly externalId: string;
}

/** 环绕；偏移量不限 ±1。 */
export function stepPage(current: PageId, delta: number): PageId {
  const size = PAGE_ORDER.length;
  const index = Math.max(0, PAGE_ORDER.indexOf(current));
  const next = (((index + delta) % size) + size) % size;

  return PAGE_ORDER[next] ?? DEFAULT_PAGE;
}
