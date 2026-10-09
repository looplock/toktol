/**
 * IPC 查询的统一装载状态机：deps（浅比较）变化即重拉、在途响应按代号作废、
 * 重拉期间保留上一份成功数据供降透明度展示。六处手写 useEffect + cancelled
 * + setState 的取数模式（Sessions/Details/Config/Pricing/Gateway/Overview）
 * 都收敛到这里，加载错与操作错由此分家——错误属于"当前这组 deps 的查询"，
 * 操作失败走调用方自己的 action/toast 通道，不再混进页面级 loadError。
 */

import { useCallback, useEffect, useRef, useState } from "react";

export interface InvokeQuery<T> {
  /** 最近一次成功的数据；重拉期间保留旧值，从未成功时为 initialData ?? null。 */
  readonly data: T | null;
  /** 请求在途（含轮询与 reload）。注入 initialData 时首帧为 false。 */
  readonly loading: boolean;
  /** 当前 deps 上最近一次失败的 String(err)；成功即清，deps 变化不串味。 */
  readonly error: string | null;
  /** 手动重拉：删除、写操作后重算这类 deps 之外的失效源。 */
  readonly reload: () => void;
  /** 直接覆盖数据并作废在途响应：写操作本身返回新值时免一次往返。 */
  readonly mutate: (next: T) => void;
}

export function useInvokeQuery<T>(options: {
  /**
   * 请求的身份：浅比较，任一项变化即重拉（语义同 useEffect 依赖；每个
   * 调用点的数组长度恒定）。null = 条件停用（如缓存命中时跳过），loading
   * 恒 false、不清理已注入的数据。
   */
  readonly deps: readonly unknown[] | null;
  readonly fetch: () => Promise<T>;
  /**
   * 首帧注入（测试缝隙 / 已知初值）：跳过首次装载（SSR 不跑 effect，
   * 注入态即终态）；deps 后续变化照常重拉。
   */
  readonly initialData?: T;
  /** 轮询间隔（毫秒）：与 deps 装载并行，只管定时重拉当前键。 */
  readonly pollMs?: number;
  /** 装载成功回调（当前代号未作废时才调）：写回缓存、清残余状态等。 */
  readonly onSuccess?: (data: T) => void;
}): InvokeQuery<T> {
  const { deps, fetch, initialData, pollMs, onSuccess } = options;
  const [data, setData] = useState<T | null>(initialData ?? null);
  const [loading, setLoading] = useState(initialData === undefined && deps !== null);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);

  // fetch / onSuccess 每次渲染同步进 ref：装载 effect 只按 deps 触发，
  // 闭包始终取最新——deps 不变时 filters 等闭包输入也不会变。
  const fetchRef = useRef(fetch);
  const successRef = useRef(onSuccess);
  useEffect(() => {
    fetchRef.current = fetch;
    successRef.current = onSuccess;
  });

  // 在途响应的代号：deps 变化 / reload / mutate 都使旧响应作废，
  // 迟到的旧数据不得覆盖新状态（与后端分页竞态的防线同源）。
  const genRef = useRef(0);
  const load = useCallback(() => {
    const gen = ++genRef.current;
    setLoading(true);
    fetchRef
      .current()
      .then((next) => {
        if (gen !== genRef.current) return;
        setData(next);
        setError(null);
        successRef.current?.(next);
      })
      .catch((err: unknown) => {
        if (gen !== genRef.current) return;
        setError(String(err));
      })
      .finally(() => {
        if (gen === genRef.current) setLoading(false);
      });
  }, []);

  // 装载：deps 浅比较 + reload 计数。effect 本体每次渲染都会跑（deps 数组
  // 字面量身份恒变），但比较成本可忽略，只有真变化才发请求。
  const depsRef = useRef<readonly unknown[] | null>(null);
  const tickRef = useRef(0);
  useEffect(() => {
    if (deps === null) return;
    const prev = depsRef.current;
    const changed =
      prev === null ||
      prev.length !== deps.length ||
      deps.some((dep, index) => !Object.is(dep, prev[index]));
    if (!changed && tick === tickRef.current) return;
    depsRef.current = deps;
    tickRef.current = tick;
    load();
  }, [deps, tick, load]);

  // 轮询：独立于装载 effect，定时重拉当前键；卸载或 pollMs 变化才重挂，
  // 不追 deps——停用与否在装载 effect 里生效，节奏不被 deps 抖动重置。
  useEffect(() => {
    if (pollMs === undefined) return undefined;
    const timer = window.setInterval(load, pollMs);
    return () => window.clearInterval(timer);
  }, [pollMs, load]);

  const reload = useCallback(() => setTick((n) => n + 1), []);
  const mutate = useCallback((next: T) => {
    genRef.current += 1;
    setData(next);
    setError(null);
  }, []);

  return { data, loading, error, reload, mutate };
}
