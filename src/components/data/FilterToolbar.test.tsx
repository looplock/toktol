import { renderToStaticMarkup } from "react-dom/server";
import { it, expect } from "vitest";

import { stringsFor } from "../../i18n/strings";
import { timePresetsOf, weekdaysOf } from "../../lib/filters";
import { FilterToolbar, type FilterDimension } from "./FilterToolbar";

// 用 react-dom/server：只验"渲染出什么"。两页共用同一条筛选栏，这里守住它的组成，
// 免得将来某一页加东西时只改一个页面、两边长歪。

const noop = () => {};

function dimension(id: string, label: string): FilterDimension {
  return {
    id,
    label,
    options: [{ value: "a", label: "a", count: 3 }],
    selected: new Set(),
    onToggle: noop,
    onClear: noop,
  };
}

function markup(): string {
  const strings = stringsFor();

  return renderToStaticMarkup(
    <FilterToolbar
      time={{ preset: "all", range: null }}
      onTimeChange={noop}
      timePresets={timePresetsOf(strings)}
      allTimeLabel={strings.rangeAll}
      customLabel={strings.filterCustomRange}
      startLabel={strings.filterStart}
      endLabel={strings.filterEnd}
      weekdays={weekdaysOf(strings)}
      dimensions={[dimension("tool", "工具"), dimension("model", "模型")]}
      query=""
      onQueryChange={noop}
      searchHint={strings.filterSearchHint}
      searchPlaceholder={strings.filterSearchOptions}
      clearLabel={strings.filterClear}
      noMatchLabel={strings.filterNoMatch}
      totalLabel="254 条"
      resetLabel={strings.resetFilters}
      onReset={noop}
    />,
  );
}

it("筛选栏五件套齐全：时间段、多选、搜索、计数、重置", () => {
  const html = markup();
  const strings = stringsFor();

  expect(html).toContain(strings.rangeAll);
  expect(html).toContain("工具");
  expect(html).toContain("模型");
  expect(html).toContain(strings.filterSearchHint);
  expect(html).toContain("254 条");
  expect(html).toContain(strings.resetFilters);
});

it("不用原生 select：下拉走自绘弹层，样式才和主题一致", () => {
  expect(markup()).not.toContain("<select");
});

it("自定义区间在按钮上显示成 MM-DD ~ MM-DD", () => {
  const strings = stringsFor();
  const start = new Date(2026, 8, 20).getTime();
  const end = new Date(2026, 8, 26).getTime();
  const html = renderToStaticMarkup(
    <FilterToolbar
      time={{ preset: "custom", range: { start, end } }}
      onTimeChange={noop}
      timePresets={timePresetsOf(strings)}
      allTimeLabel={strings.rangeAll}
      customLabel={strings.filterCustomRange}
      startLabel={strings.filterStart}
      endLabel={strings.filterEnd}
      weekdays={weekdaysOf(strings)}
      dimensions={[]}
      query=""
      onQueryChange={noop}
      searchHint={strings.filterSearchHint}
      searchPlaceholder={strings.filterSearchOptions}
      clearLabel={strings.filterClear}
      noMatchLabel={strings.filterNoMatch}
      totalLabel="0 条"
      resetLabel={strings.resetFilters}
      onReset={noop}
    />,
  );

  expect(html).toContain("09-20 ~ 09-26");
});
