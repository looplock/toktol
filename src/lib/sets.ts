/**
 * 不可变集合操作的小工具：React 状态里的 Set 只能换引用不能改内容，
 * 这里集中"复制 → 变更 → 返回新集合"的那套样板。纯函数，泛型于元素类型。
 */

/** 切换集合里的一个值：在则摘、不在则加，返回新集合。 */
export function toggleValue<T>(set: ReadonlySet<T>, value: T): Set<T> {
  const next = new Set(set);
  if (!next.delete(value)) next.add(value);

  return next;
}

/**
 * 把一批值统一置为存在/不存在（全选/全清一页、批量勾选这类）。
 * 调用方自己保证"无变化返回原引用"的短路（如先判 has）。
 */
export function setPresence<T>(
  set: ReadonlySet<T>,
  values: readonly T[],
  present: boolean,
): Set<T> {
  const next = new Set(set);
  for (const value of values) {
    if (present) next.add(value);
    else next.delete(value);
  }

  return next;
}
