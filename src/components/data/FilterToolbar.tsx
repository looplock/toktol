/**
 * 页面筛选条：时间段 + 若干多选维度 + 搜索 + 计数 + 重置。
 * 明细页与总览页共用同一份，两页的筛选条才不会各自漂移；文案全部由调用方传入
 * （组件不认识 strings 表，与 DateRangePicker / MultiSelect 一致）。
 * 单行不折行：窄窗口横向滚动，因为弹层是 portal 到 body 的，不会被裁切。
 */

import {
  DateRangePicker,
  type TimePreset,
  type TimeSelection,
} from "./DateRangePicker";
import { Card } from "../ui/Card";
import { MultiSelect, type MultiSelectOption } from "./MultiSelect";
import { SearchIcon } from "../ui/icons";
import { formatDayRange } from "../../lib/format";

export interface FilterDimension {
  readonly id: string;
  readonly label: string;
  readonly options: readonly MultiSelectOption[];
  readonly selected: ReadonlySet<string>;
  readonly onToggle: (value: string) => void;
  readonly onClear: () => void;
}

export interface FilterToolbarProps {
  readonly time: TimeSelection;
  readonly onTimeChange: (next: TimeSelection) => void;
  readonly timePresets: readonly {
    readonly id: TimePreset;
    readonly label: string;
  }[];
  /** 未选任何时间段时的按钮文案。 */
  readonly allTimeLabel: string;
  readonly customLabel: string;
  readonly startLabel: string;
  readonly endLabel: string;
  readonly weekdays: readonly string[];
  readonly dimensions: readonly FilterDimension[];
  readonly query: string;
  readonly onQueryChange: (value: string) => void;
  readonly searchHint: string;
  readonly searchPlaceholder: string;
  readonly clearLabel: string;
  readonly noMatchLabel: string;
  /** 已格式化的计数文案（"254 条"）；条数口径由页面定。 */
  readonly totalLabel: string;
  readonly resetLabel: string;
  readonly onReset: () => void;
}

export function FilterToolbar({
  time,
  onTimeChange,
  timePresets,
  allTimeLabel,
  customLabel,
  startLabel,
  endLabel,
  weekdays,
  dimensions,
  query,
  onQueryChange,
  searchHint,
  searchPlaceholder,
  clearLabel,
  noMatchLabel,
  totalLabel,
  resetLabel,
  onReset,
}: FilterToolbarProps) {
  // 按钮文案：自定义区间显示 "MM-DD ~ MM-DD"（跨年带年份），预设显示预设名。
  const timeLabel =
    time.preset === "custom" && time.range !== null
      ? formatDayRange(time.range.start, time.range.end)
      : (timePresets.find((preset) => preset.id === time.preset)?.label ??
        allTimeLabel);

  return (
    /* 工具条不是内容卡：贴边密度用 none + 自带 2px，raised 底色与两侧卡片区分。 */
    <Card
      raised
      padding="none"
      className="flex items-center gap-1 overflow-x-auto px-2 py-2"
    >
      <DateRangePicker
        label={timeLabel}
        active={time.range !== null}
        value={time}
        onChange={onTimeChange}
        presets={timePresets}
        customLabel={customLabel}
        startLabel={startLabel}
        endLabel={endLabel}
        weekdays={weekdays}
      />
      {dimensions.map((dimension) => (
        <MultiSelect
          key={dimension.id}
          label={dimension.label}
          options={dimension.options}
          selected={dimension.selected}
          onToggle={dimension.onToggle}
          onClear={dimension.onClear}
          searchPlaceholder={searchPlaceholder}
          clearLabel={clearLabel}
          noMatchLabel={noMatchLabel}
        />
      ))}
      <div className="ml-auto flex shrink-0 items-center gap-2 pl-1">
        <div className="flex h-8 w-64 items-center gap-1.5 rounded-control border border-border px-2 focus-within:border-ink-muted/60">
          <SearchIcon className="size-3.5 shrink-0 text-ink-muted" />
          <input
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder={searchHint}
            aria-label={searchHint}
            className="h-full w-full min-w-0 bg-transparent text-xs text-ink placeholder:text-ink-muted/60 focus:outline-none focus-visible:outline-none"
          />
        </div>
        <span className="whitespace-nowrap font-mono text-xs tabular-nums text-ink-muted">
          {totalLabel}
        </span>
        <div className="h-4 w-px bg-border" />
        <button
          type="button"
          onClick={onReset}
          className="whitespace-nowrap rounded-control px-1 text-xs text-accent transition-colors hover:opacity-80"
        >
          {resetLabel}
        </button>
      </div>
    </Card>
  );
}
