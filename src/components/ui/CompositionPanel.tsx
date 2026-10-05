/**
 * 构成面板：左侧构成环（中心合计）+ 右侧图例 + 总量行。当前用于明细页
 * tokens / 费用列的 ⓘ tooltip（四桶构成），本体与页面无耦合。
 */

export function CompositionPanel({
  parts,
  total,
  totalLabel,
  format,
  centerFormat = format,
  ringCaption,
}: {
  readonly parts: readonly {
    readonly label: string;
    readonly value: number;
    /** 设计令牌名（--tt-bucket-in 这类），环弧与色点共用同一颗，保证两处同色。 */
    readonly token: string;
    /** 色点拼色的第二颗令牌（如输入并入缓存读时），双色各半；环弧仍只用主色。 */
    readonly secondaryToken?: string;
    /** 值已并入其他行的弧（如缓存读并入输入），只进图例，不进构成环。 */
    readonly legendOnly?: boolean;
  }[];
  readonly total: number;
  readonly totalLabel: string;
  /** 图例与总量行的数值格式（tokens 用紧凑计数，费用用全精度美元）。 */
  readonly format: (value: number) => string;
  /** 环心合计的格式：空间只有 45px，费用这里用短格式。 */
  readonly centerFormat?: (value: number) => string;
  /** 环图下方的一行小字（如 tokens 的缓存率）；费用 tooltip 不传就没有。 */
  readonly ringCaption?: string;
}) {
  // 构成环：各段圆弧按占比拼接。C 是整圆周长；dashoffset 依次前移，首段从
  // 12 点钟方向起（rotate -90）。零值项不进环也不进图例——0 token / $0 的桶
  // 没有信息量，只会撑长面板。legendOnly 项只进图例不进环——值已并入其他
  // 行的弧，进环会把同一份量算两遍。全为 0 时只剩底环，不会画出 NaN 弧段。
  const legendParts = parts.filter((part) => part.value > 0);
  const ringParts = legendParts.filter((part) => part.legendOnly !== true);
  const C = 2 * Math.PI * 20;
  let consumed = 0;
  const segments = ringParts.map((part) => {
    const fraction = total > 0 ? part.value / total : 0;
    const segment = {
      ...part,
      dashArray: `${fraction * C} ${C}`,
      dashOffset: -consumed * C,
    };
    consumed += fraction;
    return segment;
  });

  return (
    <div className="flex items-center gap-3.5">
      {/* 环与其下的小字（缓存率）成一列整体居中；没有小字时高度=环高，布局不变。 */}
      <div className="flex shrink-0 flex-col items-center gap-1">
        <div className="relative size-16">
          <svg aria-hidden="true" viewBox="0 0 48 48" className="size-full">
          <g transform="rotate(-90 24 24)">
            <circle
              cx="24"
              cy="24"
              r="20"
              fill="none"
              strokeWidth="6"
              style={{ stroke: "var(--tt-surface-muted)" }}
            />
            {segments.map((segment) => (
              <circle
                key={segment.label}
                cx="24"
                cy="24"
                r="20"
                fill="none"
                strokeWidth="6"
                strokeDasharray={segment.dashArray}
                strokeDashoffset={segment.dashOffset}
                style={{ stroke: `var(${segment.token})` }}
              />
            ))}
          </g>
        </svg>
          <span className="absolute inset-0 flex items-center justify-center font-mono text-[10px] tabular-nums text-ink-strong">
            {centerFormat(total)}
          </span>
        </div>
        {ringCaption === undefined ? null : (
          <span className="whitespace-nowrap text-[10px] text-ink-muted tabular-nums">
            {ringCaption}
          </span>
        )}
      </div>
      {/* min-w 而不是定宽：英文标签（"Cache read"）加全精度金额比中文宽，
          定宽会把 nowrap 的数值挤出面板边框；宽度交给内容定，160px 只是下限。 */}
      <div className="flex min-w-40 flex-col gap-1.5">
        {legendParts.map((part) => (
          <div
            key={part.label}
            className="flex items-center gap-2 whitespace-nowrap"
          >
            <span
              aria-hidden="true"
              className="size-2 shrink-0 rounded-full"
              style={
                part.secondaryToken === undefined
                  ? { backgroundColor: `var(${part.token})` }
                  : {
                      backgroundImage: `linear-gradient(90deg, var(${part.token}) 50%, var(${part.secondaryToken}) 50%)`,
                    }
              }
            />
            <span className="text-ink-muted">{part.label}</span>
            <span className="ml-auto pl-3 font-mono tabular-nums text-ink-strong">
              {format(part.value)}
            </span>
          </div>
        ))}
        <div className="mt-0.5 flex items-center gap-2 border-t border-border pt-1.5 whitespace-nowrap">
          <span className="text-ink">{totalLabel}</span>
          <span className="ml-auto pl-3 font-mono tabular-nums text-ink-strong">
            {format(total)}
          </span>
        </div>
      </div>
    </div>
  );
}
