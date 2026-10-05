// 数字滚动：对原始数值做 rAF 插值，经调用方的格式化函数输出到 DOM。
// 动画直接写 textContent、不经过 setState——多个数字同时滚动时 React 不参与渲染。
// 缓动 cubicOut 与图表进场同语言；prefers-reduced-motion 或设置页关掉图表
// 生长动画时跳过动画直接终值。

import { useLayoutEffect, useRef } from "react";

import { useChartAnimation } from "../lib/chartAnimation";

interface AnimatedNumberProps {
  readonly value: number;
  /** 数字 → 字符串的格式化函数（formatTokens / formatCount / formatDuration…）。 */
  readonly format: (value: number) => string;
  /** 滚动时长（毫秒）。 */
  readonly duration?: number;
}

const DEFAULT_DURATION = 800;

function easeOutCubic(t: number): number {
  return 1 - (1 - t) ** 3;
}

export function AnimatedNumber({
  value,
  format,
  duration = DEFAULT_DURATION,
}: AnimatedNumberProps) {
  const ref = useRef<HTMLSpanElement>(null);
  const animationPref = useChartAnimation();
  // 上一帧实际显示的数值：动画的起点是"此刻显示的数"，目标变了就从当前值滚过去。
  const displayedRef = useRef(0);

  // useLayoutEffect：首帧绘制前写入起始文本，避免span空一帧的闪烁。
  useLayoutEffect(() => {
    const el = ref.current;
    if (el === null) return;

    const from = displayedRef.current;
    const reduce =
      animationPref === "off" ||
      (typeof window.matchMedia === "function" &&
        window.matchMedia("(prefers-reduced-motion: reduce)").matches);

    if (reduce || duration <= 0 || from === value) {
      displayedRef.current = value;
      el.textContent = format(value);
      return;
    }

    const t0 = performance.now();
    let raf = 0;
    const tick = (now: number): void => {
      const progress = Math.min((now - t0) / duration, 1);
      const displayed = from + (value - from) * easeOutCubic(progress);
      displayedRef.current = displayed;
      el.textContent = format(displayed);
      if (progress < 1) {
        raf = requestAnimationFrame(tick);
      }
    };
    raf = requestAnimationFrame(tick);

    return () => cancelAnimationFrame(raf);
  }, [value, format, duration, animationPref]);

  return <span ref={ref} />;
}
