/**
 * 指标卡。数字一律 tabular-nums——成本与用量是竖排对照着看的，
 * 等宽数字能让上下两行的数位对齐，非等宽字体会让数字左右跳。
 */

import { Card } from "./Card";

interface StatCardProps {
  readonly label: string;
  readonly value: string;
  readonly delta?: { readonly direction: "up" | "down"; readonly text: string };
  readonly deltaSymbol?: string;
  readonly hint?: string;
}

export function StatCard({
  label,
  value,
  delta,
  deltaSymbol,
  hint,
}: StatCardProps) {
  return (
    <Card padding="p-4">
      <p className="text-xs text-ink-muted">{label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>

      {delta === undefined ? null : (
        <p
          className={`mt-1 text-xs tabular-nums ${delta.direction === "up" ? "text-up" : "text-down"}`}
        >
          {deltaSymbol === undefined ? null : (
            <span aria-hidden="true">{deltaSymbol} </span>
          )}
          {delta.text}
        </p>
      )}

      {hint === undefined ? null : (
        <p className="mt-1 text-xs text-ink-muted">{hint}</p>
      )}
    </Card>
  );
}
