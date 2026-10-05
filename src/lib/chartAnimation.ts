/**
 * 图表生长动画偏好：总览各卡片的进场动画（柱体生长、环形扫出、布局过渡）。
 * 纯前端显示偏好（归属同明暗/强调色/单位制），localStorage 持久化；关闭时
 * EChart 层直接 animation: false，自绘进场（barEntry/treemapEntry）不再触发。
 */

import { createContext, useContext } from "react";

import { LS_KEY_CHART_ANIMATION } from "../constants";

export const CHART_ANIMATIONS = ["on", "off"] as const;

/** `on` 播放生长动画，`off` 图表直接呈现最终形态。 */
export type ChartAnimation = (typeof CHART_ANIMATIONS)[number];

export const DEFAULT_CHART_ANIMATION: ChartAnimation = "on";

function parseChartAnimation(value: string | null): ChartAnimation | null {
  return (CHART_ANIMATIONS as readonly string[]).includes(value ?? "")
    ? (value as ChartAnimation)
    : null;
}

function readStored(
  key: string,
  storage: Pick<Storage, "getItem">,
): string | null {
  try {
    return storage.getItem(key);
  } catch {
    // 静默跳过：读不到就当没存过。
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

export function readStoredChartAnimation(
  storage: Pick<Storage, "getItem"> = localStorage,
): ChartAnimation | null {
  return parseChartAnimation(readStored(LS_KEY_CHART_ANIMATION, storage));
}

export function storeChartAnimation(
  animation: ChartAnimation,
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  writeStored(LS_KEY_CHART_ANIMATION, animation, storage);
}

/** 读出用户存的动画偏好；没存过或存了不认识的值就是播放。 */
export function readChartAnimationSettings(
  storage: Pick<Storage, "getItem"> = localStorage,
): ChartAnimation {
  return readStoredChartAnimation(storage) ?? DEFAULT_CHART_ANIMATION;
}

export const ChartAnimationContext = createContext<ChartAnimation>(
  DEFAULT_CHART_ANIMATION,
);

/** 取当前动画偏好；图表组件直接调用，设置页走显式 props。 */
export function useChartAnimation(): ChartAnimation {
  return useContext(ChartAnimationContext);
}
