// ECharts 不认 CSS 变量，只能把令牌读出来喂给它。

import { useEffect, useState } from "react";

import type { CardPaletteScheme } from "./overview/layout";

export interface ChartPalette {
  readonly series: ChartSeries;
  readonly ink: string;
  readonly inkMuted: string;
  readonly border: string;
  readonly surface: string;
  /** 弱化底色：悬浮背景带这类"垫在数据图形下面"的填充，比 surface 再浅一档。 */
  readonly surfaceSubtle: string;
}

/** 固定 8 路（tokens.css 的 --tt-chart-1..8）：定长元组让索引访问不必查 undefined。 */
export type ChartSeries = readonly [
  string,
  string,
  string,
  string,
  string,
  string,
  string,
  string,
];

const FALLBACK_SERIES = "#888888";
const FALLBACK_INK = "#888888";

/** 非默认配色方案：色表写死在这里。默认档走 --tt-chart-* 令牌，跟着主题走；
 * 自定义档没有令牌来源，只能按明暗各给一份——暗色用同一色相的浅端，保证对得起深底。 */
const PALETTE_SCHEMES: Record<Exclude<CardPaletteScheme, "default">, { light: ChartSeries; dark: ChartSeries }> = {
  indigo: {
    light: ["#3C3489", "#534AB7", "#7F77DD", "#AFA9EC", "#CECBF6", "#4A4499", "#8B84E3", "#625BC5"],
    dark: ["#AFA9EC", "#CECBF6", "#EEEDFE", "#8B84E3", "#C0BBF3", "#7F77DD", "#B5B0F0", "#9C95E8"],
  },
  warm: {
    light: ["#993C1D", "#D85A30", "#EF9F27", "#FAC775", "#F0997B", "#BA7517", "#854F0B", "#E88A3C"],
    dark: ["#F0997B", "#FAC775", "#F5C4B3", "#EF9F27", "#E88A5F", "#F7C9A5", "#D85A30", "#FFD9B0"],
  },
  gray: {
    light: ["#2C2C2A", "#5F5E5A", "#888780", "#B4B2A9", "#D3D1C7", "#444441", "#9C9B94", "#6E6D68"],
    dark: ["#D3D1C7", "#B4B2A9", "#9C9B94", "#888780", "#E4E2DA", "#C4C2B9", "#F1EFE8", "#A5A39B"],
  },
};

export function readChartPalette(
  root: Element | undefined = typeof document === "undefined" ? undefined : document.documentElement,
  scheme?: CardPaletteScheme,
): ChartPalette {
  // SSR（node 测试环境）没有 DOM：整套退回 fallback 常量，浏览器行为不变。
  const style = root === undefined ? null : getComputedStyle(root);

  const value = (name: string, fallback: string): string => {
    const raw = style?.getPropertyValue(name).trim() ?? "";

    return raw === "" ? fallback : raw;
  };

  let series: ChartSeries = [
    value("--tt-chart-1", FALLBACK_SERIES),
    value("--tt-chart-2", FALLBACK_SERIES),
    value("--tt-chart-3", FALLBACK_SERIES),
    value("--tt-chart-4", FALLBACK_SERIES),
    value("--tt-chart-5", FALLBACK_SERIES),
    value("--tt-chart-6", FALLBACK_SERIES),
    value("--tt-chart-7", FALLBACK_SERIES),
    value("--tt-chart-8", FALLBACK_SERIES),
  ];

  if (scheme !== undefined && scheme !== "default") {
    const table = PALETTE_SCHEMES[scheme];
    const dark = root?.getAttribute("data-theme") === "dark";

    series = dark ? table.dark : table.light;
  }

  return {
    series,
    ink: value("--tt-ink", FALLBACK_INK),
    inkMuted: value("--tt-ink-muted", FALLBACK_INK),
    border: value("--tt-border", FALLBACK_INK),
    surface: value("--tt-surface", FALLBACK_INK),
    surfaceSubtle: value("--tt-surface-subtle", FALLBACK_INK),
  };
}

/** 跟着 <html data-theme> 变：主题切换后图表要重画。scheme 变化同样触发重读。 */
export function useChartPalette(scheme?: CardPaletteScheme): ChartPalette {
  // 缺省 root 由 readChartPalette 自己解析——SSR（node 测试环境）没有 document，
  // 这里不能显式触碰它。
  const [palette, setPalette] = useState<ChartPalette>(() => readChartPalette(undefined, scheme));

  useEffect(() => {
    const root = document.documentElement;
    const update = (): void => setPalette(readChartPalette(root, scheme));
    const observer = new MutationObserver(update);

    observer.observe(root, { attributes: true, attributeFilter: ["data-theme"] });
    update();

    return () => observer.disconnect();
  }, [scheme]);

  return palette;
}

/** 进场时长基准：EChart 应用为底层默认值，option 里的同名键可覆盖。 */
const ANIMATION_ENTRANCE_MS = 800;

/** 进场与更新动画的统一默认值：EChart 应用为底层，option 里的同名键可覆盖。 */
export function animationDefaults(): Record<string, unknown> {
  return {
    animationDuration: ANIMATION_ENTRANCE_MS,
    animationEasing: "cubicOut",
    animationDurationUpdate: 450,
    animationEasingUpdate: "cubicOut",
  };
}
