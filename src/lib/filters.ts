/**
 * 筛选条两页共用的派生值：预设清单、星期表头。
 * 文案一律从 strings 取——两张页面的筛选条长得一样，措辞就不该各写一份。
 */

import type { Strings } from "../i18n/strings";
import {
  presetRange,
  type TimePreset,
  type TimeSelection,
} from "../components/data/DateRangePicker";
import type { FilterDimension } from "../components/data/FilterToolbar";
import type { MultiSelectOption } from "../components/data/MultiSelect";
import { toggleValue } from "./sets";

export interface TimePresetOption {
  readonly id: TimePreset;
  readonly label: string;
}

export function timePresetsOf(strings: Strings): readonly TimePresetOption[] {
  return [
    { id: "today", label: strings.rangeToday },
    { id: "24h", label: strings.range24h },
    { id: "7d", label: strings.range7d },
    { id: "30d", label: strings.range30d },
    { id: "all", label: strings.rangeAll },
  ];
}

/** 星期表头：周一开头的 7 个短标签。 */
export function weekdaysOf(strings: Strings): readonly string[] {
  return strings.weekdayLabelsShort.split(",");
}

/**
 * 两页共用的默认时间段：今天（按调用时刻算）。初始与重置都用它，
 * "重置"才真正回到默认而不是另一个随便的状态。
 */
export function defaultTimeSelection(): TimeSelection {
  return { preset: "today", range: presetRange("today", Date.now()) };
}

/**
 * 从选中集合里摘掉被禁用的工具：选项已不可见，留着等于永远少一块、解释不清
 * 的数据。两处短路（空禁用清单、实际没摘掉任何项）都返回原引用——状态没变
 * 就别触发重渲染。
 */
export function dropDisabled(
  current: ReadonlySet<string>,
  disabled: readonly string[],
): ReadonlySet<string> {
  if (disabled.length === 0) return current;

  const next = new Set(current);
  for (const id of disabled) next.delete(id);

  return next.size === current.size ? current : next;
}

/** 维度状态的下推口：页面传 setState 的 dispatch，这里只发不可变更新。 */
type SelectionUpdater = (current: ReadonlySet<string>) => ReadonlySet<string>;
type SelectionSetter = (updater: ReadonlySet<string> | SelectionUpdater) => void;

/**
 * 筛选条三个维度（工具/模型/项目）的构建：总览与明细页同一份——onToggle 的
 * 不可变切换与 onClear 的清空各写一份必然漂移。文案与选项由调用方给
 * （两页的标签措辞不同，选项的图标/计数来源也不同）。
 */
export function filterDimensions(input: {
  labels: { readonly tool: string; readonly model: string; readonly project: string };
  options: {
    readonly tool: readonly MultiSelectOption[];
    readonly model: readonly MultiSelectOption[];
    readonly project: readonly MultiSelectOption[];
  };
  selection: {
    readonly tool: ReadonlySet<string>;
    readonly model: ReadonlySet<string>;
    readonly project: ReadonlySet<string>;
  };
  setters: { readonly tool: SelectionSetter; readonly model: SelectionSetter; readonly project: SelectionSetter };
}): FilterDimension[] {
  const build = (
    id: FilterDimension["id"],
    label: string,
    options: readonly MultiSelectOption[],
    selected: ReadonlySet<string>,
    setter: SelectionSetter,
  ): FilterDimension => ({
    id,
    label,
    options,
    selected,
    onToggle: (value) => setter((current) => toggleValue(current, value)),
    onClear: () => setter(new Set<string>()),
  });

  return [
    build("tool", input.labels.tool, input.options.tool, input.selection.tool, input.setters.tool),
    build("model", input.labels.model, input.options.model, input.selection.model, input.setters.model),
    build("project", input.labels.project, input.options.project, input.selection.project, input.setters.project),
  ];
}
