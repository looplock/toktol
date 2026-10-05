// 堆叠图悬浮交互：柱状档自绘同宽背景带 + 各档共用的"压着谁、其余淡化"tooltip。
// 背景带：echarts 的 shadow 指针固定占满类目槽、宽度没有配置项，要与柱同宽只能自绘
// （槽宽 × (1 - gap)）；z=1 压在柱（z=2）下面当背景、silent 不挡命中，柱子不会被染色。

import { useEffect, useRef } from "react";

import {
  color,
  graphic,
  type ECElementEvent,
  type ECharts,
  type ElementEvent,
} from "echarts/core";

import { plotRect } from "./barEntry";

/** 鼠标当前压着的系列图形（柱体段/折线/面积），tooltip formatter 据此淡化其他行。 */
export interface HoveredSegment {
  readonly seriesIndex?: number;
}

const BAND_ALPHA = 0.35;

/** 记录压着的系列 + 柱状档的背景带。chart 由调用方经 onReady 回传。 */
export function useStackedHover(options: {
  readonly chart: ECharts | null;
  /** 只有柱状档画背景带；折线/面积档仅记录压着的系列供 formatter 淡化。 */
  readonly bar: boolean;
  /** 柱宽 = 槽宽 × (1 - gap)，与柱的 barCategoryGap 必须同源。 */
  readonly gap: number;
  /** 类目数：悬浮越界时夹取。 */
  readonly count: number;
  readonly bandColor: string;
}): { current: HoveredSegment | null } {
  const { chart, bar, gap, count, bandColor } = options;
  const hovered = useRef<HoveredSegment | null>(null);

  useEffect(() => {
    // 空态等场景会把 EChart 卸载掉：dispose 后 getZr() 返回 null，绝不能再碰。
    // 调用方 state 里可能还留着旧实例，等 EChart 重新挂载经 onReady 换新。
    if (chart === null || chart.isDisposed()) return;

    const instance = chart;
    const zr = instance.getZr();
    const band = bar
      ? new graphic.Rect({
          z: 1,
          silent: true,
          // 半透明，且色调取网格线同色（splitLine 用的 border）：同色叠加时线条对比度
          // 不被洗掉，透明才"看得见"。
          style: { fill: color.modifyAlpha(bandColor, BAND_ALPHA) },
        })
      : null;
    if (band !== null) {
      band.hide();
      // echarts 的 graphic.Rect 与 zrender 的 Element 泛型在 exactOptionalPropertyTypes
      // 下互不兼容，add/remove 处按 zr 的签名收窄，绘制仍走 Rect 本尊。
      zr.add(band as unknown as Parameters<typeof zr.add>[0]);
    }

    function move(event: ElementEvent): void {
      const offsetX = event.offsetX ?? 0;
      const offsetY = event.offsetY ?? 0;
      if (!instance.containPixel("grid", [offsetX, offsetY])) {
        // 出了绘图区：既没有当前段，也不显示背景带。
        hovered.current = null;
        band?.hide();
        return;
      }
      // 空白处（target 为空）没有"当前段"，tooltip 落回整列；但背景带恰恰要显示。
      if (event.target == null) {
        hovered.current = null;
      }
      if (band === null) return;

      // 类目序号必须走 grid finder：单轴 finder（{xAxisIndex:0}）不做换算，返回 null。
      const point = instance.convertFromPixel("grid", [offsetX, offsetY]) as number[];
      const index = Math.min(Math.max(Math.round(point?.[0] ?? 0), 0), count - 1);
      // 类目轴会把 ±0.5 吸附到刻度上，槽边缘拿不到坐标；槽宽 = 相邻刻度间距，
      // 色带以刻度为中心、宽度 = 间距 × (1 - gap)，恰好落在槽内与柱子同宽。
      const tick0 = instance.convertToPixel({ xAxisIndex: 0 }, 0) as number;
      const tick1 = instance.convertToPixel({ xAxisIndex: 0 }, 1) as number;
      const slot = tick1 - tick0;
      const tick = instance.convertToPixel({ xAxisIndex: 0 }, index) as number;
      const width = slot * (1 - gap);
      const plot = plotRect(instance);
      band.setShape({
        x: tick - width / 2,
        y: plot.y,
        width,
        height: plot.height,
      });
      band.show();
    }
    function hide(): void {
      band?.hide();
    }
    // 记录当前压着的系列图形：柱体段走 dataIndex 路径，折线/面积走 eventData 路径
    // （triggerEvent 开启），两者都带 seriesIndex；图例/网格线不会误报。
    // 刻意不监听 mouseout 清 ref——zrender 一次 mousemove 里按
    // out(旧) → mousemove(tooltip formatter) → over(新) 派发，先清后写会让 formatter
    // 瞬间弹整列；ref 由 over 直接覆盖、仅在空白/出图时由 move 清理，没有空窗。
    function over(event: ECElementEvent): void {
      if (event.seriesIndex != null) {
        hovered.current = event;
      }
    }
    function out(): void {
      hovered.current = null;
    }

    zr.on("mousemove", move);
    zr.on("globalout", hide);
    instance.on("mouseover", over);
    instance.on("globalout", out);

    return () => {
      zr.off("mousemove", move);
      zr.off("globalout", hide);
      instance.off("mouseover", over);
      instance.off("globalout", out);
      if (band !== null) {
        zr.remove(band as unknown as Parameters<typeof zr.remove>[0]);
      }
      hovered.current = null;
    };
  }, [chart, bar, gap, count, bandColor]);

  return hovered;
}

/** axis tooltip 的 formatter：过滤 0 值、按值降序（免得一串 0 顶走有用的行）；
 * 压着某系列图形时该行正常、其余行变淡，悬浮空白则整列不带淡化。
 * 单走 axis 一条路由，不做 item 触发——那会和 axis 的 showTip 抢先后，行为看运气。
 * formatValue 按 seriesIndex 分流：双轴卡两个系列的单位不同（次数 vs 费用）。
 * 色球自绘而不用 echarts 的 marker：保证所有卡的 tooltip 都是同一款正圆。
 * 每行一个 flex 容器：名称靠左、数值靠右（min-width 撑开对齐空间）。 */
export function stackedTooltipFormatter(
  hovered: { current: HoveredSegment | null },
  formatValue: (value: number, seriesIndex?: number) => string,
): (raw: unknown) => string {
  return (raw: unknown) => {
    const points = (Array.isArray(raw) ? raw : [raw]) as Array<
      | {
          readonly name?: string;
          readonly axisValueLabel?: string;
          readonly seriesIndex?: number;
          readonly seriesName?: string;
          readonly value?: number;
          readonly color?: string;
        }
      | undefined
    >;
    const first = points[0];
    if (first === undefined || typeof first !== "object") return "";

    const segIndex = hovered.current?.seriesIndex;
    const header = first.axisValueLabel ?? first.name ?? "";
    const body = points
      .filter((point) => point !== undefined && (point.value ?? 0) !== 0)
      .sort((a, b) => (b?.value ?? 0) - (a?.value ?? 0))
      .map((point) => {
        const faded = segIndex != null && point?.seriesIndex !== segIndex;

        return (
          `<div style="display:flex;align-items:center;justify-content:space-between;gap:16px;min-width:170px;` +
          `${faded ? "opacity:0.35" : ""}">` +
          `<span style="display:flex;align-items:center">` +
          `<span style="display:inline-block;flex-shrink:0;width:8px;height:8px;border-radius:9999px;` +
          `background-color:${point?.color ?? "transparent"};margin-right:6px"></span>` +
          `${point?.seriesName ?? ""}</span>` +
          `<span>${formatValue(point?.value ?? 0, point?.seriesIndex)}</span>` +
          `</div>`
        );
      })
      .join("");

    return `<div style="margin-bottom:4px">${header}</div>${body}`;
  };
}
