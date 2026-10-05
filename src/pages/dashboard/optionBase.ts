/** 各图表共用的坐标轴与提示框默认值，颜色全部来自令牌。
 * config 传了卡片配置就顺带接管"外观"组里挂在轴上的两项：网格线开关与 Y 轴零点；
 * 不传（无设置能力的卡）保持原样。 */

import type { ChartPalette } from "../../lib/chartTheme";

export function axisDefaults(
  palette: ChartPalette,
  config?: { readonly showGridLines?: boolean; readonly yAxisFromZero?: boolean },
): Record<string, unknown> {
  return {
    grid: { left: 4, right: 12, top: 8, bottom: 2, containLabel: true },
    textStyle: { color: palette.inkMuted, fontSize: 11 },
    tooltip: {
      trigger: "axis",
      // 挂到 body 上渲染：卡片有 overflow 裁边，容器内渲染（confine）会把长 tooltip
      // 钳在图表里动不了；挂出去之后自由跟随鼠标，也不会被卡片挡住。
      appendToBody: true,
      // 多序列按值从大到小排：最相关的在最上面，长列表也不把重点顶出视野。
      order: "valueDesc",
      // 不画默认那条虚线中轴：它把柱子切成两半，信息量为零。
      axisPointer: { type: "none" },
      backgroundColor: palette.surface,
      borderColor: palette.border,
      textStyle: { color: palette.ink, fontSize: 11 },
    },
    xAxis: {
      type: "category",
      boundaryGap: true,
      axisLine: { lineStyle: { color: palette.border } },
      axisTick: { show: false },
      axisLabel: { color: palette.inkMuted, fontSize: 10, hideOverlap: true },
    },
    yAxis: {
      type: "value",
      splitLine: { show: config?.showGridLines ?? true, lineStyle: { color: palette.border } },
      axisLine: { show: false },
      axisLabel: { color: palette.inkMuted, fontSize: 10 },
      // 零点档只对值轴有意义：开了就把基线钉在 0；不传走 ECharts 自适应。
      ...((config?.yAxisFromZero ?? false) ? { min: 0 } : {}),
    },
  };
}

// 桑基图用 adjacency 而不是 self：点一段流要看到它所在的整条通路（工具→模型→项目），
// 只高亮这一段的话，看不出这笔用量最终落在哪个项目。
export function flowFocus(): Record<string, unknown> {
  return {
    emphasis: { focus: "adjacency" },
    blur: { itemStyle: { opacity: 0.25 } },
    stateAnimation: { duration: 600, easing: "cubicOut" },
  };
}

/** 柱状卡统一的类目间隙：柱宽 = 槽宽 × (1 - BAR_GAP)。悬浮背景带必须与同卡的
 * barCategoryGap 同源，改这里时检查各卡的悬浮带。 */
export const BAR_GAP = 0.12;

/** 柱状图进场逐根错峰，封顶 250ms 免得桶多时拖沓。
 * 折线不适用：进场是整体扫出，逐点延时会画歪，调用方按图表类型条件展开。
 * 堆叠图的上段锚在半空（ECharts 进场是"锚在最终上边缘、向下展开"），两段一起生长
 * 时交界处会短暂脱开——产品接受这个效果，换来的是两段同时长、节奏一致。 */
export function barStagger(): Record<string, unknown> {
  return { animationDelay: (index: number) => Math.min(index * 50, 250) };
}

/** 悬浮一根柱时淡出其他柱，衬托当前这根。堆叠卡的两个系列共用同一份。 */
export function barFocus(): Record<string, unknown> {
  return {
    emphasis: { focus: "self" },
    blur: { itemStyle: { opacity: 0.3 } },
    // 默认 300ms 太急，420ms 还是偏快；600ms 才有"托起来"的层次感。
    stateAnimation: { duration: 600, easing: "cubicOut" },
  };
}
