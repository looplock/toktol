/**
 * 空转录占位，按会话是否发生过请求分两态：
 * - 空壳会话（0 请求 / 0 token）：「等待第一条消息」——空输入框里一条
 *   闪烁光标，等的是永远不会来的历史；
 * - 有用量但日志不携带消息（如 dsh）：保持一行朴素说明。
 */

import type { SessionRow } from "../../lib/api";
import { PlayIcon } from "../../components/ui/icons";
import type { Strings } from "../../i18n/strings";

interface EmptyTranscriptProps {
  readonly row: SessionRow;
  readonly strings: Strings;
}

export function EmptyTranscript({ row, strings }: EmptyTranscriptProps) {
  const totalTokens =
    row.inputTokens + row.outputTokens + row.cacheReadTokens + row.cacheWriteTokens;
  if (row.requestCount > 0 && totalTokens > 0) {
    return (
      <div
        data-transcript-empty
        className="py-12 text-center text-sm text-ink-muted"
      >
        {strings.transcriptEmpty}
      </div>
    );
  }

  return (
    <div
      data-transcript-empty
      className="flex h-full flex-col items-center justify-center gap-6 px-8 text-center"
    >
      {/* 空输入框 + 闪烁光标：等的是永远不会来的第一条消息 */}
      <div className="w-80 max-w-full" aria-hidden>
        <div className="flex h-24 items-center rounded-card border border-border bg-surface-subtle px-5">
          <span className="inline-block h-5 w-[2px] animate-[tt-blink_1.1s_step-end_infinite] bg-ink-muted" />
        </div>
        <div className="mt-3 flex justify-end">
          <span className="inline-flex items-center gap-1.5 rounded-control bg-surface-muted px-3 py-1.5 text-xs text-ink-muted/60">
            <PlayIcon className="size-3" />
            {strings.transcriptEmptySend}
          </span>
        </div>
      </div>
      <div className="max-w-md">
        <p className="text-sm font-medium text-ink">
          {strings.transcriptEmptyTitle}
        </p>
        <p className="mt-1.5 text-xs leading-relaxed text-ink-muted">
          {strings.transcriptEmptyBody}
        </p>
      </div>
    </div>
  );
}
