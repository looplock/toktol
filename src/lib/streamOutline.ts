/**
 * 会话时间线刻度目录的滚动联动控制器（与 React 完全解耦）。
 *
 * 热路径（滚动帧）只有：一次 scrollTop 读取 → 缓存上的二分查找 → 轮次
 * 变化时回调订阅者。零 React setState、零逐条 DOM 几何读取，几百条
 * 消息的大会话滚动不再触发任何重渲染。
 *
 * 结构：
 *   - entryIndexForScroll / turnForEntry 是纯函数（可单测）；
 *   - StreamOutline 类持有偏移缓存，负责测量、监听与失效。
 *
 * 偏移缓存：绑定/尺寸变化时量一次（此时允许布局读取），滚动帧里只查
 * 缓存。未挂载的锚点用 +∞ 哨兵，二分天然跳过。条目高度随展开收起、
 * 窗口宽度变化：ResizeObserver 逐个上报，用 rAF 合帧重测，避免初始
 * 观察时 n 次连发。
 */

/** 未挂载锚点的偏移哨兵。 */
export const SENTINEL_OFFSET = Number.MAX_SAFE_INTEGER;

/** 视口顶判定线：视口顶 48px 以内最近的条目为当前条目。 */
const TOP_LINE = 48;

/** 跳转时目标条目距容器顶的余量（留出一点呼吸感）。 */
const JUMP_MARGIN = 8;

/**
 * 二分查找当前条目：offsets 升序（哨兵 +∞ 排在末尾），返回满足
 * offsets[i] <= target 的最大 i；全部大于 target 时返回 0。
 */
export function entryIndexForScroll(
  offsets: readonly number[],
  target: number,
): number {
  let index = 0;
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if ((offsets[mid] ?? SENTINEL_OFFSET) <= target) {
      index = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return index;
}

/**
 * 二分查找条目所属轮次：turnStarts 是每轮首条消息下标的升序数组，
 * 返回满足 turnStarts[i] <= entryIndex 的最大 i；空数组返回 0。
 */
export function turnForEntry(
  turnStarts: readonly number[],
  entryIndex: number,
): number {
  let turn = 0;
  let lo = 0;
  let hi = turnStarts.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if ((turnStarts[mid] ?? -1) <= entryIndex) {
      turn = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return turn;
}

/**
 * 滚动视口 → 当前轮次。常规路径按"视口顶 48px 判定线"归属；但最后一轮
 * 内容不足一屏时，滚到尽头其锚点也够不到判定线（永远停在倒数第二轮）——
 * 滚到底（留 1px 容差）就强制高亮最后一轮，目录与"看完了"保持一致。
 * 强制仅在 fullyLoaded（已加载到会话底）时生效：窗口未到底时"容器底"只是
 * 分页边界，最后一轮未必真是正在看的轮（补页随时会接上）。
 */
export function activeTurnFor(
  offsets: readonly number[],
  scrollTop: number,
  clientHeight: number,
  scrollHeight: number,
  turnStarts: readonly number[],
  fullyLoaded = true,
): number {
  if (turnStarts.length === 0) return 0;
  if (fullyLoaded && scrollTop + clientHeight >= scrollHeight - 1) {
    return turnStarts.length - 1;
  }
  const entryIndex = entryIndexForScroll(offsets, scrollTop + TOP_LINE);
  return turnForEntry(turnStarts, entryIndex);
}

/** bind() 的入参：滚动容器、每条消息的锚点元素、每轮首条消息下标。 */
export interface StreamOutlineBinding {
  readonly container: HTMLElement;
  readonly anchors: readonly (HTMLElement | null)[];
  readonly turnStarts: readonly number[];
  /** 已加载窗口是否覆盖到会话末尾（决定滚到底是否强制高亮最后一轮）。 */
  readonly atSessionEnd: boolean;
}

/** 轮次 ↔ 已加载窗口的映射：ordinal → 全局轮下标 / 窗口内局部条目下标。 */
export interface TurnWindowMapping {
  readonly globals: readonly number[];
  readonly locals: readonly number[];
}

/**
 * 把轮次映射到已加载窗口。窗口顶切在轮中间（向前补页 / 远跳后上滚的边界
 * 是任意条目）时，首个窗口条目所属的轮 X 开轮于窗口之外——不补虚拟起点，
 * 这些条目会被错算给"X 之后第一个窗口内轮"。把 X 钉在 local 0（虚拟轮
 * 起点），顶部条目就正确归属回 X；若恰有轮开在窗口顶（seq == 首条 seq），
 * 真轮自带 local 0，虚拟起点让位不推。
 */
export function mapTurnsToWindow(
  turns: readonly { readonly seq: number }[],
  windowSeqs: readonly number[],
): TurnWindowMapping {
  // seq → 首次出现的局部下标：窗口一旦出现重复 seq（不应发生，但守住
  // 纵深），取首次才能让轮起点落在最靠前的条目上，判定线不脱靶。
  const localsBySeq = new Map<number, number>();
  windowSeqs.forEach((seq, i) => {
    if (!localsBySeq.has(seq)) localsBySeq.set(seq, i);
  });
  const globals: number[] = [];
  const locals: number[] = [];
  const firstSeq = windowSeqs[0];
  if (firstSeq !== undefined) {
    let above: number | null = null;
    for (let i = 0; i < turns.length; i += 1) {
      const turn = turns[i];
      if (turn !== undefined && turn.seq < firstSeq) {
        above = i;
      } else {
        break;
      }
    }
    if (above !== null && turns[above + 1]?.seq !== firstSeq) {
      globals.push(above);
      locals.push(0);
    }
  }
  turns.forEach((turn, global) => {
    const local = localsBySeq.get(turn.seq);
    if (local !== undefined) {
      globals.push(global);
      locals.push(local);
    }
  });
  return { globals, locals };
}

export class StreamOutline {
  private binding: StreamOutlineBinding | null = null;
  /** 每条消息相对滚动内容顶部的偏移缓存（含哨兵）。 */
  private offsets: number[] = [];
  private activeTurn = 0;
  private listener: ((turn: number) => void) | null = null;
  private observer: ResizeObserver | null = null;
  private scrollRaf = 0;
  private resizeScheduled = false;
  /** 绑定换代号：bind/unbind 都会自增，rAF 回调据此丢弃换代前的过期帧。 */
  private generation = 0;

  /** 注册轮次变化回调（当前轮改变时调用，含 bind 时的强制首报）。 */
  setListener(listener: ((turn: number) => void) | null): void {
    this.listener = listener;
  }

  /** 当前轮次（换会话重挂目录时给新挂载的目录做初值）。 */
  get turn(): number {
    return this.activeTurn;
  }

  /**
   * 绑定滚动容器与锚点：立即测量一次、挂滚动监听与 ResizeObserver，
   * 并强制上报当前轮次。返回解绑函数（供 useEffect 清理）。
   */
  bind(binding: StreamOutlineBinding): () => void {
    this.unbind();
    this.generation += 1;
    this.binding = binding;
    this.activeTurn = 0;
    this.measure();
    binding.container.addEventListener("scroll", this.onScroll, {
      passive: true,
    });
    const observer = new ResizeObserver(this.onResize);
    observer.observe(binding.container);
    // 滚动内容的包裹元素也要观察：上方内容（图片加载、展开收起）变高会把
    // 下方锚点整体推移，而锚点自身尺寸不变——只盯锚点会漏掉这类位移，
    // 偏移缓存过期后跳转与高亮全错。包裹元素的高度兜住一切内部变化。
    const content = binding.container.firstElementChild;
    if (content instanceof HTMLElement) {
      observer.observe(content);
    }
    for (const anchor of binding.anchors) {
      if (anchor !== null) observer.observe(anchor);
    }
    this.observer = observer;
    // 容器可能带着恢复的滚动位置（或跳转残留），先同步一次当前轮。
    this.updateActive(true);
    return () => {
      this.unbind();
    };
  }

  /** 解绑监听与观察、清空缓存。 */
  unbind(): void {
    this.generation += 1;
    if (this.scrollRaf !== 0) {
      window.cancelAnimationFrame(this.scrollRaf);
      this.scrollRaf = 0;
    }
    this.observer?.disconnect();
    this.observer = null;
    this.binding?.container.removeEventListener("scroll", this.onScroll);
    this.binding = null;
    this.offsets = [];
    this.activeTurn = 0;
    this.resizeScheduled = false;
  }

  /** 平滑跳到某轮开头：优先走偏移缓存（零布局读取），未建缓存时实测。 */
  jumpToTurn(turn: number): void {
    const binding = this.binding;
    if (binding === null) return;
    const entryIndex = binding.turnStarts[turn];
    if (entryIndex === undefined) return;
    const cached = this.offsets[entryIndex];
    if (cached !== undefined && cached !== SENTINEL_OFFSET) {
      binding.container.scrollTo({ top: cached - JUMP_MARGIN, behavior: "auto" });
      return;
    }
    const anchor = binding.anchors[entryIndex];
    if (anchor === null || anchor === undefined) return;
    const top =
      binding.container.scrollTop +
      anchor.getBoundingClientRect().top -
      binding.container.getBoundingClientRect().top -
      JUMP_MARGIN;
    binding.container.scrollTo({ top, behavior: "auto" });
  }

  /** 重建偏移缓存（绑定与尺寸变化时调用，允许布局读取）。 */
  measure(): void {
    const binding = this.binding;
    if (binding === null) {
      this.offsets = [];
      return;
    }
    const containerTop = binding.container.getBoundingClientRect().top;
    const scrollTop = binding.container.scrollTop;
    this.offsets = binding.anchors.map((anchor) =>
      anchor === null
        ? SENTINEL_OFFSET
        : anchor.getBoundingClientRect().top - containerTop + scrollTop,
    );
  }

  private readonly onScroll = (): void => {
    // 记下发起帧的换代号：补页回位等场景里，rAF 回调可能跑到 rebind
    // （换 turnStarts）之后才执行——用过期数据算出的轮次不得推送。
    const gen = this.generation;
    if (this.scrollRaf !== 0) return;
    this.scrollRaf = window.requestAnimationFrame(() => {
      this.scrollRaf = 0;
      if (gen !== this.generation) return;
      this.updateActive(false);
    });
  };

  private readonly onResize = (): void => {
    if (this.resizeScheduled) return;
    this.resizeScheduled = true;
    const gen = this.generation;
    window.requestAnimationFrame(() => {
      this.resizeScheduled = false;
      if (gen !== this.generation) return;
      this.measure();
      this.updateActive(false);
    });
  };

  /** 滚动位置 → 当前轮次；变化时通知订阅者。 */
  private updateActive(force: boolean): void {
    const binding = this.binding;
    if (binding === null) return;
    const { container } = binding;
    const turn = activeTurnFor(
      this.offsets,
      container.scrollTop,
      container.clientHeight,
      container.scrollHeight,
      binding.turnStarts,
      binding.atSessionEnd,
    );
    if (force || turn !== this.activeTurn) {
      this.activeTurn = turn;
      this.listener?.(turn);
    }
  }
}
