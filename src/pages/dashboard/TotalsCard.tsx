/** 统计总览：居中的 Token 大数 + 一条分隔线 + 三列无框指标（参考产品稿样式）。
 * 指标不再套 StatCard 的边框盒：灰标签在上、加粗数值在下，纯排版。
 * 数值全部走 AnimatedNumber：进场/数据更新时数字滚动。 */

import type { ReactNode } from "react";

import { AnimatedNumber } from "../../components/AnimatedNumber";
import { DashboardCard } from "./DashboardCard";
import type { Strings } from "../../i18n/strings";
import { formatCount, formatDuration, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import type { DashboardData } from "../../lib/overview/types";

import type { CardGeometry } from "./geometry";

interface TotalsCardProps {
  readonly geo: CardGeometry;
  readonly strings: Strings;
  readonly data: DashboardData;
}

/** 无框指标：灰标签 + 加粗数值，可选一行灰色注记（日均），与参考稿的排版一致。 */
function PlainStat({
  label,
  value,
  hint,
}: {
  readonly label: string;
  readonly value: ReactNode;
  readonly hint?: ReactNode;
}) {
  return (
    <div>
      <p className="text-xs text-ink-muted">{label}</p>
      <p className="mt-1.5 text-[28px] font-bold tabular-nums">{value}</p>
      {hint === undefined ? null : <p className="mt-1 text-xs text-ink-muted">{hint}</p>}
    </div>
  );
}

export function TotalsCard({ geo, strings, data }: TotalsCardProps) {
  const numberUnit = useNumberUnit();
  // 日均的分母：daily 是按天的热力点，天然覆盖当前筛选范围（今天/24h 只有 1 天）。
  const days = Math.max(data.daily.length, 1);
  const durationFormat = (ms: number): string =>
    formatDuration(ms, {
      day: strings.durationDay,
      hour: strings.durationHour,
      minute: strings.durationMinute,
    });

  return (
    <DashboardCard
      id={geo.id}
      x={geo.x}
      y={geo.y}
      w={geo.w}
      h={geo.h}
      title={strings.cardTotals}
      scrollBody
    >
      <div className="text-center">
        <p className="text-xs tracking-[0.2em] text-ink-muted">{strings.overviewTotalTokens}</p>
        <p className="mt-1 text-[72px] font-bold leading-none tabular-nums">
          <AnimatedNumber value={data.totals.tokens} format={(v) => formatTokens(v, numberUnit)} />
        </p>
      </div>

      <div className="mt-5 border-t border-border" />

      <div className="mt-6 grid grid-cols-3 gap-x-8 gap-y-10">
        <PlainStat
          label={strings.overviewTotalCalls}
          value={
            <AnimatedNumber
              value={data.totals.calls}
              format={(v) => formatCount(Math.round(v))}
            />
          }
          hint={
            <AnimatedNumber
              value={Math.round(data.totals.calls / days)}
              format={(v) => `${strings.overviewDailyAvg} ${formatCount(Math.round(v))}`}
            />
          }
        />
        <PlainStat
          label={strings.overviewUsageTime}
          value={<AnimatedNumber value={data.totals.activeDurationMs} format={durationFormat} />}
          hint={
            <AnimatedNumber
              value={data.totals.activeDurationMs / days}
              format={(v) => `${strings.overviewDailyAvg} ${durationFormat(v)}`}
            />
          }
        />
        <PlainStat
          label={strings.overviewSessionCount}
          value={
            <AnimatedNumber value={data.totals.sessions} format={(v) => formatCount(Math.round(v))} />
          }
        />
        <PlainStat
          label={strings.overviewTotalProjects}
          value={
            <AnimatedNumber value={data.totals.projects} format={(v) => formatCount(Math.round(v))} />
          }
        />
        <PlainStat
          label={strings.overviewActiveTools}
          value={
            <AnimatedNumber value={data.totals.activeTools} format={(v) => formatCount(Math.round(v))} />
          }
        />
        <PlainStat
          label={strings.overviewCacheHitRate}
          value={
            data.totals.tokens === 0 ? (
              "—"
            ) : (
              <AnimatedNumber
                value={(data.totals.cacheReadTokens / data.totals.tokens) * 100}
                format={(v) => `${v.toFixed(1)}%`}
              />
            )
          }
        />
      </div>
    </DashboardCard>
  );
}
