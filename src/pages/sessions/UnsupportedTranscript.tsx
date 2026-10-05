/**
 * 转录不支持态的占位（core.unsupported 专用）：「镜片里只有账单」。
 * 放大镜扫遍本机，镜片里浮现的是这个会话真实的 token 总数——只照见用量，
 * 没照见对话。数字从 0 滚动到真实值（prefers-reduced-motion 时直接落定）；
 * 其他失败原因（源日志消失 / 超大 / 加载失败）不走这里，各有各的呈现。
 */

import { useEffect, useState } from "react";

import type { Strings } from "../../i18n/strings";
import { formatTokens } from "../../lib/format";
import type { NumberUnit } from "../../lib/numberUnit";

interface UnsupportedTranscriptProps {
  /** 会话聚合的 token 总数（header 同口径），镜片里的主角。 */
  readonly tokens: number;
  readonly numberUnit: NumberUnit;
  readonly strings: Strings;
}

export function UnsupportedTranscript({
  tokens,
  numberUnit,
  strings,
}: UnsupportedTranscriptProps) {
  const [displayed, setDisplayed] = useState(tokens);

  useEffect(() => {
    if (tokens <= 0 || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setDisplayed(tokens);
      return undefined;
    }
    setDisplayed(0);
    const start = performance.now();
    const duration = 900;
    let raf = requestAnimationFrame(function tick(now: number) {
      const t = Math.min(1, (now - start) / duration);
      setDisplayed(Math.round(tokens * (1 - Math.pow(1 - t, 3))));
      if (t < 1) raf = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(raf);
  }, [tokens]);

  return (
    <div
      data-transcript-unsupported
      className="flex h-full flex-col items-center justify-center gap-6 px-8 text-center"
    >
      {/* 镜片里只有账单：放大镜正常工作，本机里就只有这些数字 */}
      <div className="relative size-48" aria-hidden>
        <div className="absolute inset-0 rounded-full border-[10px] border-accent bg-surface-subtle shadow-[inset_0_2px_14px_rgba(0,0,0,0.08)]">
          <div className="absolute inset-2.5 -rotate-45 rounded-full border-4 border-transparent border-t-white/30" />
          <div className="flex h-full flex-col items-center justify-center gap-0.5">
            <span className="font-mono text-3xl font-semibold tabular-nums text-accent">
              {formatTokens(displayed, numberUnit)}
            </span>
            <span className="text-xs text-ink-muted">{strings.detailsColTokens}</span>
          </div>
        </div>
        {/* 手柄：顶在镜缘 45° 方向，向外延伸 */}
        <div className="absolute left-[152px] top-[164px] h-28 w-6 origin-top -rotate-45 rounded-full bg-accent" />
      </div>
      <div className="max-w-md">
        <p className="text-sm font-medium text-ink">
          {strings.transcriptUnsupportedTitle}
        </p>
        <p className="mt-1.5 text-xs leading-relaxed text-ink-muted">
          {strings.transcriptUnsupportedBody}
        </p>
      </div>
    </div>
  );
}
