/** 左栏：固定模型列表（搜索 + 状态筛选承担未定价/已定价切换）+ 目录同步。 */

import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { MultiSelect } from "../../components/data/MultiSelect";
import { ResetIcon, SearchIcon } from "../../components/ui/icons";
import type { Strings } from "../../i18n/strings";
import type { PricingModelRow } from "../../lib/api";
import { ModelIcon } from "./ModelIcon";
import { hasSuggestion } from "./price";

interface ModelListPaneProps {
  readonly strings: Strings;
  /** 状态筛选后的模型行。 */
  readonly rows: readonly PricingModelRow[];
  /** 全量模型数：空列表时区分"还没有模型"与"筛后无匹配"。 */
  readonly hasModels: boolean;
  readonly selectedId: string | null;
  readonly query: string;
  readonly statusCounts: { priced: number; pending: number; unpriced: number };
  readonly statusSelection: ReadonlySet<string>;
  readonly syncing: boolean;
  /** 目录上次同步时间（已格式化）；null = 从未同步。 */
  readonly syncedAt: string | null;
  readonly onQueryChange: (query: string) => void;
  readonly onStatusToggle: (value: string) => void;
  readonly onStatusClear: () => void;
  readonly onSelect: (id: string) => void;
  readonly onSync: () => void;
}

/** 列表行的状态徽章：只报需要动作的两态（待确认 / 未定价）。已定价且归属
 * 已确认的行不渲染徽章——来源细分是存储层的功能字段，界面上不展示。 */
function ListStatusBadge({ row, strings }: { row: PricingModelRow; strings: Strings }) {
  if (row.priceSource === null) {
    if (hasSuggestion(row)) {
      return <Badge tone="danger">{strings.pricingStatusPending}</Badge>;
    }
    return (
      <Badge tone={row.usageCount > 0 ? "danger" : "neutral"}>
        {strings.pricingStatusUnpriced}
      </Badge>
    );
  }
  if (row.mappingSource === "user" || row.mappingSource === null) {
    return null;
  }
  return <Badge tone="danger">{strings.pricingStatusPending}</Badge>;
}

export function ModelListPane({
  strings,
  rows,
  hasModels,
  selectedId,
  query,
  statusCounts,
  statusSelection,
  syncing,
  syncedAt,
  onQueryChange,
  onStatusToggle,
  onStatusClear,
  onSelect,
  onSync,
}: ModelListPaneProps) {
  return (
    <Card padding="none" className="flex min-h-0 flex-col overflow-hidden">
      <div className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <label className="flex h-7 min-w-0 flex-1 items-center gap-1.5 rounded-control border border-border bg-surface px-2">
          <SearchIcon className="size-3.5 shrink-0 text-ink-muted" />
          <input
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder={strings.pricingSearchPlaceholder}
            className="h-full w-full bg-transparent text-xs text-ink placeholder:text-ink-muted focus:outline-none"
          />
        </label>
        <MultiSelect
          bordered
          label={strings.pricingFilterTitle}
          options={[
            { value: "pending", label: strings.pricingFilterPending, count: statusCounts.pending },
            { value: "unpriced", label: strings.pricingFilterUnpriced, count: statusCounts.unpriced },
            { value: "priced", label: strings.pricingFilterPriced, count: statusCounts.priced },
          ]}
          selected={statusSelection}
          onToggle={onStatusToggle}
          onClear={onStatusClear}
          searchPlaceholder={strings.filterSearchOptions}
          clearLabel={strings.filterClear}
          noMatchLabel={strings.filterNoMatch}
        />
        <span className="shrink-0">
          <Button
            variant="icon"
            aria-label={strings.pricingSyncNow}
            title={
              syncedAt === null
                ? strings.pricingSyncNever
                : strings.pricingSyncLast.replace("{time}", syncedAt)
            }
            onClick={onSync}
            disabled={syncing}
          >
            <ResetIcon className={`size-4 ${syncing ? "animate-spin" : ""}`} />
          </Button>
        </span>
      </div>

      <ul className="min-h-0 flex-1 overflow-y-auto">
        {rows.map((row) => {
          const active = selectedId === row.id;
          return (
            <li key={row.id}>
              <button
                type="button"
                onClick={() => onSelect(row.id)}
                className={`w-full border-l-2 px-3 py-2.5 text-left transition duration-150 ease-out active:scale-[0.99] active:bg-row-hover ${
                  active
                    ? "border-accent bg-accent/10"
                    : "border-transparent hover:border-border hover:bg-row-hover"
                }`}
              >
                <div className="flex items-center gap-2">
                  <ModelIcon id={row.id} />
                  <span className="truncate font-mono text-sm text-ink-strong">
                    {row.id}
                  </span>
                  <span className="ml-auto shrink-0">
                    <ListStatusBadge row={row} strings={strings} />
                  </span>
                </div>
              </button>
            </li>
          );
        })}
        {rows.length === 0 ? (
          <li className="px-3 py-4 text-xs text-ink-muted">
            {hasModels ? strings.pricingNoMatch : strings.pricingSelectHint}
          </li>
        ) : null}
      </ul>

      <div className="shrink-0 border-t border-border px-3 py-1.5 text-xs text-ink-muted">
        {syncedAt === null
          ? strings.pricingSyncNever
          : strings.pricingSyncLast.replace("{time}", syncedAt)}
      </div>
    </Card>
  );
}
