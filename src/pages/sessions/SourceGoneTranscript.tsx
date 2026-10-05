/**
 * 转录源消失态的占位（core.source_gone 专用）：「撕剩的半页」。
 * 一页已入账的会话清单被撕走了一半——撕剩的半页上是真实的用量事实，
 * 正文那半随源日志消失。与"不携带"态（放大镜：从来就没有）相对：
 * 这里是曾经有、现在不在了。源文件找回后转录视图会自动恢复。
 */

import type { SessionRow } from "../../lib/api";

import { EmptyTranscript } from "./EmptyTranscript";
import { formatTokens } from "../../lib/format";
import type { NumberUnit } from "../../lib/numberUnit";
import type { Strings } from "../../i18n/strings";

interface SourceGoneTranscriptProps {
  readonly row: SessionRow;
  readonly numberUnit: NumberUnit;
  readonly strings: Strings;
}

export function SourceGoneTranscript({
  row,
  numberUnit,
  strings,
}: SourceGoneTranscriptProps) {
  const totalTokens =
    row.inputTokens + row.outputTokens + row.cacheReadTokens + row.cacheWriteTokens;

  // 撕页的前提是"账还在"：零用量的空壳会话没有可留档的东西，
  // 交给空转录占位（等待第一条消息）。
  if (row.requestCount === 0 || totalTokens === 0) {
    return <EmptyTranscript row={row} strings={strings} />;
  }

  return (
    <div
      data-transcript-source-gone
      className="flex h-full animate-[tt-fade-in_200ms_ease-out] flex-col items-center justify-center gap-5 px-8 text-center"
    >
      {/* 撕剩的半页：底边锯齿用 SVG 多边形，宽度与纸页（w-72）一一对应 */}
      <div className="-rotate-1 [filter:drop-shadow(0_8px_14px_rgba(0,0,0,0.10))]">
        <div className="w-72 rounded-t-card bg-surface-raised px-5 pb-3 pt-4 text-left">
          <p className="text-[11px] font-medium uppercase tracking-[0.2em] text-ink-muted">
            {strings.transcriptSourceGoneLedger}
          </p>
          <div className="mt-3 space-y-2">
            <LedgerRow value={`${row.requestCount} ${strings.sessionsRequestsUnit}`} />
            <LedgerRow
              value={`${formatTokens(totalTokens, numberUnit)} ${strings.detailsColTokens}`}
            />
          </div>
          <div className="mt-3 border-t border-dashed border-border pt-2.5">
            <p className="text-xs text-ink-muted line-through decoration-ink-muted/50">
              {strings.transcriptSourceGoneMissing}
            </p>
          </div>
        </div>
        <svg viewBox="0 0 288 14" className="block h-3.5 w-72" aria-hidden>
          <polygon
            className="fill-surface-raised"
            points="0,0 288,0 288,4 280,12 271,5 263,13 252,6 244,14 233,7 221,13 212,5 200,12 191,6 183,14 172,8 161,13 150,5 141,12 131,7 122,14 111,6 102,12 92,5 84,13 73,7 62,12 54,6 45,13 36,5 27,12 18,7 9,13 0,6"
          />
        </svg>
      </div>
      <div className="max-w-md">
        <p className="text-sm font-medium text-ink">
          {strings.transcriptSourceGoneTitle}
        </p>
        <p className="mt-1.5 text-xs leading-relaxed text-ink-muted">
          {strings.transcriptSourceGoneBody}
        </p>
        <p className="mt-2 text-[11px] text-ink-muted/80">
          {strings.transcriptSourceGoneHint}
        </p>
      </div>
    </div>
  );
}

/** 清单行：bullet + mono 数值，与整站 token 口径一致。 */
function LedgerRow({ value }: { readonly value: string }) {
  return (
    <p className="flex items-center gap-2 font-mono text-sm tabular-nums text-ink">
      <span className="size-1.5 shrink-0 rounded-full bg-accent/60" aria-hidden />
      <span>{value}</span>
    </p>
  );
}
