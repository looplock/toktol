// 按需注册而非整包 import：整包会把所有图表类型拖进产物。
// option 由调用方拼，这里不引用 echarts 类型——拼错字段不该连累调用方的类型检查。

import { useEffect, useRef } from "react";
import {
  BarChart,
  BoxplotChart,
  HeatmapChart,
  LineChart,
  PieChart,
  SankeyChart,
  ScatterChart,
  TreemapChart,
} from "echarts/charts";
import {
  CalendarComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  VisualMapComponent,
} from "echarts/components";
import * as echarts from "echarts/core";
import { LegacyGridContainLabel } from "echarts/features";
import { CanvasRenderer } from "echarts/renderers";

import { animationDefaults } from "../../lib/chartTheme";
import { useChartAnimation } from "../../lib/chartAnimation";
import { chartStructureKey, optionFingerprint } from "../../lib/echartOption";

echarts.use([
  BarChart,
  BoxplotChart,
  HeatmapChart,
  LineChart,
  PieChart,
  SankeyChart,
  ScatterChart,
  TreemapChart,
  CalendarComponent,
  GridComponent,
  // 多张卡的 grid 用 containLabel: true，ECharts 6 按需注册下需要这个兼容组件。
  LegacyGridContainLabel,
  TooltipComponent,
  LegendComponent,
  VisualMapComponent,
  CanvasRenderer,
]);

type ChartOption = Parameters<echarts.ECharts["setOption"]>[0];

/** 首建图前容器尺寸需保持稳定的时长：盖过 gridstack 初始展开过渡的尾帧。 */
const SETTLE_MS = 200;

/** 进场动画窗口：重建后原生形状进场最长 800ms + 250ms 错峰。窗口内的 resize 会丢弃
 * 播到一半的形状动画（条形/饼图瞬间到终值），而布局过渡收尾恰好在建图后不久触发
 * 一次亚像素修正，所以窗口内的 resize 一律推迟到窗口结束。 */
const ENTRY_GRACE_MS = 1200;

interface EChartProps {
  readonly option: Record<string, unknown>;
  readonly label: string;
  readonly className?: string;
  /** init 后回调一次实例：要做 zr 级交互（自绘悬浮背景带等）的调用方从这里拿。 */
  readonly onReady?: (chart: echarts.ECharts) => void;
  /** setOption 应用后同步回调（rebuilt = 本次整体重建）：要抢在 zr 下一次绘制前操作
   * 元素的自绘进场动画从这里下手——走 React effect 会慢一帧，首建时闪完整柱子。 */
  readonly onAfterApply?: (chart: echarts.ECharts, rebuilt: boolean) => void;
}

export function EChart({ option, label, className = "", onReady, onAfterApply }: EChartProps) {
  const host = useRef<HTMLDivElement>(null);
  const chart = useRef<echarts.ECharts | null>(null);
  const pending = useRef<Record<string, unknown> | null>(null);
  const lastKey = useRef<string | null>(null);
  const lastFingerprint = useRef<string | null>(null);
  const reduceMotion = useRef(false);
  const entryAt = useRef(0);
  const resizeTimer = useRef<number | undefined>(undefined);
  const animationPref = useChartAnimation();

  function requestResize() {
    const instance = chart.current;
    if (instance === null) return;

    window.clearTimeout(resizeTimer.current);
    const wait = entryAt.current + ENTRY_GRACE_MS - performance.now();
    if (wait <= 0) {
      instance.resize();
      return;
    }

    // 到点重算而不是直接 resize：等待期间可能又重建（切档位），窗口顺延。
    resizeTimer.current = window.setTimeout(requestResize, wait);
  }

  function apply(raw: Record<string, unknown>) {
    const instance = chart.current;
    if (instance === null) return;

    // 系统减少动态偏好与设置页的"图表生长动画"开关都关：直接呈现最终形态。
    const animationsOn = animationPref === "on" && !reduceMotion.current;
    const key = chartStructureKey(raw);
    // 结构没变走合并：数据变化由 ECharts 平滑过渡；只有切图表类型才整体重建、重播进场。
    const notMerge = lastKey.current !== key;
    // 内容也没变就跳过：上游重渲染会重建 option 对象（值相同），多余的一次 setOption
    // 会打断进行中的进场动画。指纹带上动画偏好——切设置的瞬间没有内容变化也要真执行。
    const fingerprint = optionFingerprint(raw) + (animationsOn ? "" : "|no-anim");
    if (!notMerge && fingerprint === lastFingerprint.current) return;
    lastKey.current = key;
    lastFingerprint.current = fingerprint;
    // 关闭动画时没有进场可言，窗口不必推开 resize。
    if (notMerge && animationsOn) entryAt.current = performance.now();

    instance.setOption(
      {
        ...animationDefaults(),
        ...raw,
        ...(animationsOn ? {} : { animation: false }),
      } as ChartOption,
      { notMerge },
    );
    // onAfterApply 的唯一用途是让调用方播自绘进场（barEntry/treemapEntry）：
    // 动画关了就不该触发，否则柱子会先跳出来再被摆回去。
    if (animationsOn) onAfterApply?.(instance, notMerge);
  }

  useEffect(() => {
    const element = host.current;
    if (element === null) return;

    reduceMotion.current = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

    // 容器有尺寸才 init，且要等尺寸稳定：gridstack 初始摆放带展开过渡，图表若在卡片
    // 还在长大的中途建图，进场动画会在小画布上播完再被 resize 拉大，看起来像没动画。
    let settleTimer: number | undefined;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry === undefined || entry.contentRect.width === 0 || entry.contentRect.height === 0) {
        return;
      }

      if (chart.current === null) {
        window.clearTimeout(settleTimer);
        settleTimer = window.setTimeout(() => {
          if (chart.current !== null || element.clientWidth === 0 || element.clientHeight === 0) {
            return;
          }

          chart.current = echarts.init(element);
          if (pending.current !== null) {
            apply(pending.current);
            pending.current = null;
          }
          // 闭包持有首帧的 onReady：调用方传 setState 之类的稳定引用即可。
          onReady?.(chart.current);
        }, SETTLE_MS);
      } else {
        requestResize();
      }
    });
    observer.observe(element);

    return () => {
      window.clearTimeout(settleTimer);
      window.clearTimeout(resizeTimer.current);
      observer.disconnect();
      chart.current?.dispose();
      chart.current = null;
      lastKey.current = null;
      lastFingerprint.current = null;
      entryAt.current = 0;
    };
  }, []);

  useEffect(() => {
    // 图还没建时先挂起，只留最新一份——中间态不值得画。
    if (chart.current === null) {
      pending.current = option;

      return;
    }

    apply(option);
    // 动画偏好变化也要重放一次 setOption：指纹已带上偏好，切换即刻生效。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [option, animationPref]);

  return <div ref={host} role="img" aria-label={label} className={className} />;
}
