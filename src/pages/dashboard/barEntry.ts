// 堆叠柱进场动画：整柱从零轴同步长出、分段锁死、逐列错峰。
// 原生柱进场是"锚在最终上边缘、向下展开"（BarView 建元素时 height=0、y 不动），
// 堆叠分段各自展开、交界处短暂脱开，做不出整柱锁死的生长，所以在 zr 层自绘。

import { helper, type ECharts } from "echarts/core";

interface PlotRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 绘图区矩形：getModel 在实例类型上是 private，绘图区又没有公开的取法，只能绕类型。 */
export function plotRect(of: ECharts): PlotRect {
  const model = (of as unknown as {
    getModel: () => {
      getComponent: (
        type: string,
        componentIndex: number,
      ) => { coordinateSystem: { getRect: () => PlotRect } };
    };
  }).getModel();

  return model.getComponent("grid", 0).coordinateSystem.getRect();
}

const ENTRY_MS = 600;
const COLUMN_STAGGER_MS = 50;
const COLUMN_STAGGER_CAP_MS = 250;

/** 整柱从零轴同步长出：所有段以零轴为公共缩放原点、同系数拉起——缩放保距（同原点
 * 同系数 → 分段相对距离不变），交界处天然锁死；生长中分段按比例压缩，观感与经典
 * 柱状图生长一致。只在 onAfterApply 的 rebuilt 时调用：必须赶在 zr 下一次绘制前把
 * scaleY 压下去，晚一帧就闪完整柱子。配 animationDuration: 0 关掉原生进场。 */
export function playBarEntry(instance: ECharts): void {
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

  const plot = plotRect(instance);
  const axisY = plot.y + plot.height;
  for (const el of instance.getZr().storage.getDisplayList(true)) {
    if (el.name !== "item") continue;
    // 列序在 innerStore（helper.getECData）里，el.ecData 不是公开属性，恒为 undefined。
    const column = helper.getECData(el).dataIndex ?? 0;
    el.originY = axisY;
    el.scaleY = 0.001;
    el.animateTo(
      { scaleY: 1 },
      {
        duration: ENTRY_MS,
        easing: "cubicOut",
        delay: Math.min(column * COLUMN_STAGGER_MS, COLUMN_STAGGER_CAP_MS),
        done: () => {
          // 还原 transform：后续 update 动画直接动 shape，残留缩放会叠加几何。
          el.scaleY = 1;
          el.originY = 0;
        },
      },
    );
  }
}
