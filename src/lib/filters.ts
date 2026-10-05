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

/** 切换集合里的一个值：选中状态是不可变的，返回新集合。 */
export function toggleValue(
  set: ReadonlySet<string>,
  value: string,
): Set<string> {
  const next = new Set(set);
  if (!next.delete(value)) next.add(value);

  return next;
}
