/** setOption 走合并还是整体替换的判据：结构键相同 → 合并（数据变化由 ECharts 平滑
 * 过渡，不重播进场动画）；不同（切图表类型、series 增删）→ notMerge 整体重建。 */

export function chartStructureKey(option: Record<string, unknown>): string {
  const raw = option["series"];
  const list = Array.isArray(raw) ? raw : [raw];
  const shapes = list.map((item) => {
    if (typeof item !== "object" || item === null) {
      return { type: undefined, stack: undefined };
    }

    const series = item as Record<string, unknown>;

    return { type: series["type"], stack: series["stack"] };
  });
  // yAxis.max 也要进键：百分比轴（max 100）切回绝对量轴时合并会残留旧上限，
  // 把整张图压成一条竖缝（占比 → 折线踩过这个坑）。
  const yAxis = option["yAxis"];
  const yAxisMax =
    typeof yAxis === "object" && yAxis !== null
      ? (yAxis as Record<string, unknown>)["max"]
      : undefined;

  return JSON.stringify([Object.keys(option).sort(), shapes, yAxisMax]);
}

/** 内容指纹：值相同但对象身份不同的两次 option 判等，用来跳过多余的 setOption。
 *  键序无关（重建对象键序漂移不算变化）；函数值（formatter 等）按源码文本
 *  参与比较，闭包重建但逻辑不变时不算变化。 */
export function optionFingerprint(option: Record<string, unknown>): string {
  return JSON.stringify(option, (_key, value: unknown) => {
    if (typeof value === "function") return String(value);
    if (Array.isArray(value) || typeof value !== "object" || value === null) return value;
    return sortKeys(value as Record<string, unknown>);
  });
}

function sortKeys(value: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(
    Object.entries(value).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
  );
}
