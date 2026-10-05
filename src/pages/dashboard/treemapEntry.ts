// 矩形树图进场：逐块从自身中心弹出、按面积降序错峰。
// 原生 treemap 首建没有动画（TreemapView 对 isInit 直接 renderFinally），所以在 zr 层
// 自绘。块的视觉 = 一个 nodeGroup 里的 background/content 两层 Rect（label 文字挂在
// content 上），同心、同参数缩放即整块弹出；块的位移在父 group 上，不影响锚点换算。
// 元素上读不到 ecData（innerStore 私有存储），块的识别纯靠结构：
// 根节点的兜底背景所在 group 里还有子 group，叶子块的 group 里只有 Rect——据此排除。

import { graphic, type ECharts } from "echarts/core";

const ENTRY_MS = 600;
const BLOCK_STAGGER_MS = 50;
const BLOCK_STAGGER_CAP_MS = 250;

export function playTreemapEntry(instance: ECharts): void {
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;

  // 按父 group 归块：同块的 background/content 共享一个父，错峰序按块算不按层算。
  const blocks = new Map<object, { els: graphic.Rect[]; area: number }>();
  for (const el of instance.getZr().storage.getDisplayList(true)) {
    if (!(el instanceof graphic.Rect)) continue;
    const parent = el.parent;
    if (parent === null) continue;
    if (parent.children().some((child) => child.isGroup)) continue;
    const shape = el.shape;
    if (shape === undefined || shape.width <= 0 || shape.height <= 0) continue;
    const block = blocks.get(parent) ?? { els: [], area: 0 };
    block.els.push(el);
    block.area = Math.max(block.area, shape.width * shape.height);
    blocks.set(parent, block);
  }

  // 面积降序 = 数据序（items 进 option 前已按值降序），最大的块最先落位。
  const order = [...blocks.values()].sort((a, b) => b.area - a.area);
  order.forEach((block, rank) => {
    const delay = Math.min(rank * BLOCK_STAGGER_MS, BLOCK_STAGGER_CAP_MS);
    for (const el of block.els) {
      const shape = el.shape;
      // zrender 的 origin 在元素本地坐标系：块中心 = shape 原点 + 半宽高
      // （background 从 0 起、content 从边框宽起，换算后是同一个视觉中心）。
      const originX = shape.x + shape.width / 2;
      const originY = shape.y + shape.height / 2;
      el.originX = originX;
      el.originY = originY;
      el.scaleX = 0.001;
      el.scaleY = 0.001;
      el.animateTo(
        { scaleX: 1, scaleY: 1 },
        {
          duration: ENTRY_MS,
          easing: "cubicOut",
          delay,
          done: () => {
            // 还原 transform：后续 update 动画直接动 shape，残留缩放会叠加几何。
            el.scaleX = 1;
            el.scaleY = 1;
            el.originX = 0;
            el.originY = 0;
          },
        },
      );
    }
  });
}
