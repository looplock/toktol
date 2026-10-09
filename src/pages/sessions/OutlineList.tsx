/**
 * 会话详情的对话目录（列表式）：左栏「一轮询问」一项。渲染一次后高亮
 * 切换与滚入视野全走命令式 DOM 更新，滚动联动零 React 重渲染。
 */

import {
  forwardRef,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
} from "react";
import type { Strings } from "../../i18n/strings";

/** 一个目录项 = 一次用户询问（一轮）。seq 是该轮首条条目的索引顺序号。 */
export interface OutlineItem {
  readonly seq: number;
  readonly snippet: string;
}

/**
 * 对话目录（列表式与刻度式共用）的命令式句柄：滚动联动时由控制器订阅回调直接调用。
 */
export interface OutlineListHandle {
  setActive(turn: number): void;
}

/** 当前轮高亮类 / 常规类（命令式切换，类名须与渲染时一致）。 */
const LIST_ACTIVE = ["border-accent", "bg-accent/10", "text-accent"] as const;
const LIST_IDLE = ["border-transparent", "text-ink-muted", "hover:bg-row-hover"] as const;

interface OutlineListProps {
  readonly items: readonly OutlineItem[];
  readonly strings: Strings;
  /** 初值轮次：重挂载（换会话）时由父级给最后已知轮次。 */
  readonly initialTurn: number;
  readonly onJump: (index: number) => void;
}

/**
 * 对话目录（列表式）：编号 + 单行摘要（取该轮首条用户消息）。点击跳到
 * 该轮开头。
 */
export const OutlineList = forwardRef<OutlineListHandle, OutlineListProps>(
  function OutlineList({ items, strings, initialTurn, onJump }, ref) {
    const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
    const activeRef = useRef(-1);

    const setActive = useCallback(
      (turn: number) => {
        if (turn === activeRef.current) return;
        const prev = itemRefs.current[activeRef.current];
        if (prev !== null && prev !== undefined) {
          // 失活要对称还原：激活时从本项移走的 LIST_IDLE（border-transparent
          // 等）必须加回来，否则 border-l-2 露出默认灰边——React 的 className
          // vdom 没变不会重写 DOM class，残留会一直留在列表上。
          for (const cls of LIST_ACTIVE) prev.classList.remove(cls);
          for (const cls of LIST_IDLE) prev.classList.add(cls);
          prev.removeAttribute("aria-current");
        }
        const next = itemRefs.current[turn];
        if (next !== null && next !== undefined) {
          for (const cls of LIST_IDLE) next.classList.remove(cls);
          for (const cls of LIST_ACTIVE) next.classList.add(cls);
          next.setAttribute("aria-current", "true");
          // 高亮项保持在面板可见范围内。
          next.scrollIntoView({ block: "nearest" });
        }
        activeRef.current = turn;
      },
      [],
    );

    useImperativeHandle(ref, () => ({ setActive }), [setActive]);

    // 挂载时应用初值（换会话重挂，由父级给最后已知轮次）。
    useLayoutEffect(() => {
      setActive(initialTurn);
    }, [setActive, initialTurn]);

    return (
      <aside className="flex w-60 shrink-0 flex-col border-r border-border">
        <p className="shrink-0 border-b border-border px-3 py-2.5 text-xs font-medium text-ink-muted">
          {strings.sessionsOutlinePanel}
        </p>
        <div className="min-h-0 flex-1 overflow-y-auto py-1">
          {items.map((item, index) => (
            <button
              key={item.seq}
              ref={(el) => {
                itemRefs.current[index] = el;
              }}
              type="button"
              onClick={() => onJump(index)}
              // content-visibility：屏外行跳过布局与绘制——目录是全量轮次
              // 列表，大会话数千行时这是无虚拟化前提下唯一的渲染护栏；
              // 行高单行截断近似 32px，auto 关键字让浏览器记住真实行高。
              className={`flex w-full cursor-pointer items-baseline gap-1.5 border-l-2 px-3 py-1.5 text-left text-xs transition-colors [content-visibility:auto] [contain-intrinsic-size:auto_32px] ${LIST_IDLE.join(" ")}`}
            >
              <span className="shrink-0 tabular-nums">{index + 1}.</span>
              <span className="min-w-0 truncate">{item.snippet}</span>
            </button>
          ))}
        </div>
      </aside>
    );
  },
);
