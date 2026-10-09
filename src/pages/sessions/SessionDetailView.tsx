/**
 * 会话详情视图：信息头（左栏 标题 / 会话坐标，目录视图切换右对齐；第三行
 * 小字 时间范围 / 请求 / Tokens）+ 时间线消息流（渲染见
 * TranscriptStream.tsx，目录见 OutlineList.tsx）。独立导出供测试
 * （SSR 直渲）。embedded 时作为右卡内嵌视图：隐藏返回按钮（列表常驻，点
 * 其他行即切换），Esc 仍可取消选中。
 */

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Button } from "../../components/ui/Button";
import { Segmented } from "../../components/ui/Segmented";
import { ToolIcon } from "../../components/ui/ToolIcon";
import type { Strings } from "../../i18n/strings";
import {
  buildTranscriptIndex,
  fetchTranscriptPage,
  fetchTranscriptTurns,
  isTauriRuntime,
  type SessionRow,
  type TranscriptPageEntry,
  type TranscriptProgressPayload,
  type TranscriptTurn,
} from "../../lib/api";
import { formatTimestamp, formatTokens } from "../../lib/format";
import { useNumberUnit } from "../../lib/numberUnit";
import { listen } from "@tauri-apps/api/event";
import { mapTurnsToWindow, StreamOutline } from "../../lib/streamOutline";
import { OutlineList, type OutlineItem, type OutlineListHandle } from "./OutlineList";
import { TurnRuler } from "./TurnRuler";
import { EmptyTranscript } from "./EmptyTranscript";
import { SourceGoneTranscript } from "./SourceGoneTranscript";
import { UnsupportedTranscript } from "./UnsupportedTranscript";
import { StreamEntry } from "./TranscriptStream";

type DetailState =
  | { readonly kind: "loading" }
  | { readonly kind: "building"; readonly done: number; readonly total: number }
  | { readonly kind: "failed"; readonly code: string }
  | { readonly kind: "ready"; readonly data: ReadyTranscript };

/** 已就绪的转录：全部轮次（目录）+ 连续 seq 区间的已加载条目窗口。 */
interface ReadyTranscript {
  readonly turns: readonly TranscriptTurn[];
  readonly total: number;
  readonly entries: readonly TranscriptPageEntry[];
}

/** 每次向前/向后拉取的条目数。 */
const TRANSCRIPT_PAGE_SIZE = 80;
/** 无限滚动的窗口上限：补页后超出即裁掉远离视口的一端——长会话单向浏览
 * 的条目数组与 DOM 不再无界增长。3 页：视口上/下各留足预读余量。 */
const TRANSCRIPT_WINDOW_MAX = TRANSCRIPT_PAGE_SIZE * 3;
/** 建站进度事件（壳广播，按会话过滤）。 */
const TRANSCRIPT_PROGRESS_EVENT = "transcript://progress";

export function SessionDetailView({
  row,
  strings,
  onBack,
  embedded = false,
  initialData,
}: {
  readonly row: SessionRow;
  readonly strings: Strings;
  readonly onBack: () => void;
  /** 内嵌双卡模式：无返回按钮，撑满右卡。 */
  readonly embedded?: boolean;
  /** 测试缝隙：注入首帧转录（轮次 + 条目窗口），跳过 IPC 拉取。 */
  readonly initialData?: {
    readonly turns: readonly TranscriptTurn[];
    readonly total: number;
    readonly entries: readonly TranscriptPageEntry[];
  };
}) {
  const numberUnit = useNumberUnit();
  const [state, setState] = useState<DetailState>(
    initialData === undefined
      ? { kind: "loading" }
      : { kind: "ready", data: initialData },
  );

  // 目录视图：列表（默认）或刻度尺；两者共用 OutlineListHandle 与跳转。
  const [outlineView, setOutlineView] = useState<"list" | "ruler">("list");

  // 滚动容器与消息锚点（ref 回调收集，bind 时交给控制器测量）。
  const streamRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLDivElement | null)[]>([]);

  // 滚动联动控制器：与 React 解耦（见 streamOutline.ts），滚动帧里只做
  // 缓存二分 + 命令式推送，零 setState、零逐条布局读取。
  const outlineCtlRef = useRef<StreamOutline | null>(null);
  if (outlineCtlRef.current === null) {
    outlineCtlRef.current = new StreamOutline();
  }
  const outlineCtl = outlineCtlRef.current;
  const outlineListRef = useRef<OutlineListHandle>(null);
  // 最新轮次：换会话重挂目录时做初值，避免高亮闪断。
  const lastTurnRef = useRef(0);

  useEffect(() => {
    // 切会话回到顶部。
    streamRef.current?.scrollTo({ top: 0 });
  }, [row]);

  // 打开期间只取一次：转录是历史快照，不做自动刷新。索引未建（大会话
  // 首次打开）先建站——进度经事件更新，建完自动取轮次与首页。活动会话
  // 的日志边看边写：turns 与 page 各自校验索引指纹，任何一步带回
  // built=false 都说明索引刚被追加失效，须重走建站（增量续建只补尾部）
  // 再试——把 built=false 的空页渲染成"没有可展示的消息"是误导性空态。
  // 重试封顶：后端异常导致永远建不出时终止在 failed，不无限空转。
  useEffect(() => {
    // 注入首帧转录（测试缝隙）时按契约跳过 IPC 拉取，保持注入态。
    if (initialData !== undefined) return undefined;
    let cancelled = false;
    setState({ kind: "loading" });
    const MAX_ATTEMPTS = 5;
    const CHURN_CODE = "core.transcript_churn";

    const buildThen = (next: () => Promise<void>) => {
      setState({ kind: "building", done: 0, total: 0 });
      const unlisten = isTauriRuntime()
        ? listen<TranscriptProgressPayload>(TRANSCRIPT_PROGRESS_EVENT, (event) => {
            const p = event.payload;
            if (p.tool === row.tool && p.externalId === row.externalId) {
              if (!cancelled) {
                setState({ kind: "building", done: p.done, total: p.total });
              }
            }
          })
        : null;
      // 构建（或重取）失败必须终止在 failed 态：不能落进后续 re-fetch
      // ——否则 built=false 的空页会覆盖 failed，呈现成误导性的空态。
      return buildTranscriptIndex(row.tool, row.externalId)
        .then(next, (code: unknown) => {
          if (!cancelled) setState({ kind: "failed", code: String(code ?? "") });
        })
        .finally(() => {
          void unlisten?.then((fn) => fn());
        });
    };

    const fetchTurns = (depth: number): Promise<void> =>
      fetchTranscriptTurns(row.tool, row.externalId).then((payload) => {
        if (cancelled) return undefined;
        if (!payload.built) {
          if (depth >= MAX_ATTEMPTS) {
            setState({ kind: "failed", code: CHURN_CODE });
            return undefined;
          }
          return buildThen(() => fetchTurns(depth + 1));
        }
        return fetchTranscriptPage(
          row.tool,
          row.externalId,
          null,
          null,
          TRANSCRIPT_PAGE_SIZE,
        ).then((page) => {
          if (cancelled) return undefined;
          if (!page.built) {
            if (depth >= MAX_ATTEMPTS) {
              setState({ kind: "failed", code: CHURN_CODE });
              return undefined;
            }
            return buildThen(() => fetchTurns(depth + 1));
          }
          setState({
            kind: "ready",
            data: { turns: payload.turns, total: page.total, entries: page.entries },
          });
          return undefined;
        });
      });

    fetchTurns(0).catch((code: unknown) => {
      if (!cancelled) setState({ kind: "failed", code: String(code ?? "") });
    });
    return () => {
      cancelled = true;
    };
    // initialData 仅注入态时跳过拉取，调用方须传稳定引用（现仅测试/harness）。
  }, [row, initialData]);

  // Esc 返回列表：与页面级导航的直觉一致。
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        onBack();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);

  // 时间范围取自轮次时间戳（覆盖全会话，与已加载窗口无关）；行内聚合来自列表数据。
  let range = null;
  if (state.kind === "ready" && state.data.turns.length > 0) {
    const stamps = state.data.turns
      .map((turn) => turn.tsMs)
      .filter((ts): ts is number => ts !== null);
    if (stamps.length > 0) {
      range = {
        start: formatTimestamp(Math.min(...stamps)),
        end: formatTimestamp(Math.max(...stamps)),
      };
    }
  }

  const totalTokens =
    row.inputTokens +
    row.outputTokens +
    row.cacheReadTokens +
    row.cacheWriteTokens;

  // 目录来自后端轮次索引（多 GB 会话不必读正文即可列出全部轮次）。
  const outlineItems = useMemo<OutlineItem[]>(() => {
    if (state.kind !== "ready") return [];
    return state.data.turns.map((turn) => ({
      seq: turn.seq,
      snippet: turn.isSummary
        ? strings.transcriptCompacted
        : turn.snippet !== ""
          ? turn.snippet
          : strings.transcriptRoleUser,
    }));
  }, [state, strings]);

  // 目录数据就绪时绑定控制器（测量 + 挂滚动监听 + ResizeObserver 失效），
  // 卸载或切会话时解绑。锚点只覆盖已加载的窗口条目；窗口内的轮次映射到
  // 全局轮下标，控制器上报的序号经 turnMapRef 翻译后驱动目录高亮。
  const turnMapRef = useRef<number[]>([]);
  useEffect(() => {
    if (state.kind !== "ready" || state.data.entries.length === 0) {
      outlineCtl.unbind();
      return undefined;
    }
    const container = streamRef.current;
    if (container === null) return undefined;
    const entries = state.data.entries;
    const anchors = entries.map((_, i) => itemRefs.current[i] ?? null);
    // 窗口顶切在轮中间时由 mapTurnsToWindow 补虚拟轮起点（详见其注释）。
    const mapping = mapTurnsToWindow(
      state.data.turns,
      entries.map((e) => e.seq),
    );
    turnMapRef.current = [...mapping.globals];
    return outlineCtl.bind({
      container,
      anchors,
      turnStarts: mapping.locals,
      atSessionEnd:
        (entries[entries.length - 1]?.seq ?? -1) >= state.data.total - 1,
    });
  }, [state, outlineCtl]);

  // 控制器的轮次变化 → 命令式推给当前挂载的目录（高频路径零重渲染）。
  useEffect(() => {
    outlineCtl.setListener((turnOrdinal) => {
      const global = turnMapRef.current[turnOrdinal];
      if (global !== undefined) {
        lastTurnRef.current = global;
        outlineListRef.current?.setActive(global);
      }
    });
    return () => outlineCtl.setListener(null);
  }, [outlineCtl]);

  // 换窗跳转：scrollTo 必须等新窗口 commit 后执行——在 fetch 回调里直接
  // scrollTo 的是旧 DOM（旧内容先闪到顶），用标记位让 useLayoutEffect 在
  // 新条目挂载后落位。
  const pendingTopRef = useRef(false);
  useLayoutEffect(() => {
    if (!pendingTopRef.current) return;
    pendingTopRef.current = false;
    streamRef.current?.scrollTo({ top: 0 });
  }, [state]);

  // 跳到某条 seq：在窗口内直接滚动，否则换窗（从该轮起取一页）。
  // 每次换窗发起时自增窗口代号：在途的补页/换窗请求返回后发现代号变了
  // 就丢弃——否则跳转前发起的 loadBefore 会在新窗口落地后 prepend 出
  // 重复乱序的 entries，滚动回位算出垃圾偏移（点击 1 落到 20 的根源）。
  // 索引被追加失效（built=false）或空页时增量续建后重试，保证活跃会话
  // 点击必跳（此前是静默无效）。
  const jumpToSeq = useCallback(
    (seq: number) => {
      if (state.kind !== "ready") return;
      const idx = state.data.entries.findIndex((e) => e.seq === seq);
      if (idx >= 0) {
        itemRefs.current[idx]?.scrollIntoView({ block: "start" });
        return;
      }
      const attempt = (retries: number): Promise<void> => {
        const gen = windowGenRef.current + 1;
        windowGenRef.current = gen;
        loadingMoreRef.current = true;
        return fetchTranscriptPage(row.tool, row.externalId, seq - 1, null, TRANSCRIPT_PAGE_SIZE)
          .then((page) => {
            if (gen !== windowGenRef.current) return undefined;
            if (page.built && page.entries.length > 0) {
              pendingTopRef.current = true;
              // 换窗作废在途补页的滚动锚点：旧锚点的 prevHeight 属于已
              // 抛弃的窗口，留着会让下一次 state 变化触发垃圾回位。
              prependAnchorRef.current = null;
              setState((current) =>
                current.kind === "ready"
                  ? { kind: "ready", data: { ...current.data, entries: page.entries } }
                  : current,
              );
              return undefined;
            }
            if (retries <= 0) return undefined;
            return buildTranscriptIndex(row.tool, row.externalId).then(() =>
              attempt(retries - 1),
            );
          })
          .finally(() => {
            loadingMoreRef.current = false;
          });
      };
      void attempt(2).catch(() => {});
    },
    [state, row],
  );

  const jumpToTurn = useCallback(
    (turnIndex: number) => {
      if (state.kind !== "ready") return;
      const turn = state.data.turns[turnIndex];
      if (turn !== undefined) {
        jumpToSeq(turn.seq);
      }
    },
    [state, jumpToSeq],
  );

  // 无限滚动：向前/向后补页。向前补页会平移滚动锚点，补页后按偏移差回位；
  // 窗口超上限时裁掉远离视口的一端。
  const loadingMoreRef = useRef(false);
  // 窗口代号：换窗跳转发起时自增；补页请求携带发起时的代号，返回时对不上
  // 即丢弃（跳转已换窗，旧补页合进去会产生重复乱序的 entries）。
  const windowGenRef = useRef(0);
  // 向前补页的回位锚点：视口位置 + 旧首条元素的偏移。回位量 = 提交后旧首条
  // （现位于 prepended 下标处）与旧位置的偏移差——比 scrollHeight 差值稳健：
  // 尾部窗口裁剪不改变上方内容的偏移，却会污染高度差。prepended 在补页
  // 返回时置位；合并被连续性检查拒绝时窗口首条不变，回位守卫会跳过。
  const prependAnchorRef = useRef<{
    scrollTop: number;
    firstTop: number;
    prevFirst: number;
    prepended: number | null;
  } | null>(null);
  // 向后补页触发头部裁剪时的回位锚点：裁剪发生在视口上方，视口按偏移差
  // 上移。expectedFirst 核对合并是否真的落地（被连续性检查拒绝时不补偿，
  // 锚点由下一次提交消费——没有提交就由该守卫安全丢弃）。
  const headTrimRef = useRef<{
    scrollTop: number;
    removed: number;
    expectedFirst: number;
  } | null>(null);
  // 已加载到会话底后的探新冷却：滚动事件高频，2s 内不重复探测。
  const lastProbeRef = useRef(0);

  // 轮次快照增量刷新：turns 只在打开时取过一次，活跃会话边看边写时新消息
  // 开的新轮不进目录、滚动联动把新消息错算给最后一个已知轮。补页/探新时
  // 顺带重取；无变化不 setState（避免无谓的 rebind）。
  const refreshingTurnsRef = useRef(false);
  const refreshTurns = useCallback(() => {
    if (state.kind !== "ready" || refreshingTurnsRef.current) return;
    refreshingTurnsRef.current = true;
    fetchTranscriptTurns(row.tool, row.externalId)
      .then((payload) => {
        if (!payload.built) return undefined;
        setState((current) => {
          if (current.kind !== "ready") return current;
          const prev = current.data.turns;
          const next = payload.turns;
          if (
            next.length === prev.length &&
            (prev.length === 0 ||
              (prev[0]?.seq === next[0]?.seq &&
                prev[prev.length - 1]?.seq === next[next.length - 1]?.seq))
          ) {
            return current;
          }
          return { kind: "ready", data: { ...current.data, turns: next } };
        });
        return undefined;
      })
      .catch(() => {})
      .finally(() => {
        refreshingTurnsRef.current = false;
      });
  }, [state, row]);

  const loadAfter = useCallback(() => {
    if (state.kind !== "ready" || loadingMoreRef.current) return;
    const { entries, total } = state.data;
    const last = entries[entries.length - 1];
    if (last === undefined) return;
    // 已加载到会话底也要低频探查新消息（活跃会话追加）：翻页为空即无事
    // 发生，只顺带刷新 turns 与 total。
    const atEnd = last.seq >= total - 1;
    if (atEnd && Date.now() - lastProbeRef.current < 2000) return;
    const gen = windowGenRef.current;
    loadingMoreRef.current = true;
    if (atEnd) lastProbeRef.current = Date.now();
    fetchTranscriptPage(row.tool, row.externalId, last.seq, null, TRANSCRIPT_PAGE_SIZE)
      .then((page) => {
        // 换代：期间发生了换窗跳转，这份旧窗口的续页不能再合进去。
        if (gen !== windowGenRef.current) return undefined;
        if (!page.built) {
          // 索引被追加失效：增量续建（只补尾部，很快，不挂进度 UI）后原地
          // 重探一次；重建期间置位防并发重建。
          if (!rebuildingRef.current) {
            rebuildingRef.current = true;
            void buildTranscriptIndex(row.tool, row.externalId)
              .then(() => {
                lastProbeRef.current = 0;
                loadAfterRef.current();
                return undefined;
              })
              .catch(() => {})
              .finally(() => {
                rebuildingRef.current = false;
              });
          }
          return undefined;
        }
        // 窗口上限：底部补页若使窗口超限，裁掉头部（远离视口端）。裁剪在
        // 视口上方，须在提交前测好补偿锚点（引用旧 DOM）；是否生效由
        // useLayoutEffect 按预期新首条核对——合并被连续性检查拒绝时不补偿。
        const trimCount = entries.length + page.entries.length - TRANSCRIPT_WINDOW_MAX;
        let headTrim: {
          scrollTop: number;
          removed: number;
          expectedFirst: number;
        } | null = null;
        if (trimCount > 0) {
          const el = streamRef.current;
          const newFirst = itemRefs.current[trimCount];
          const oldFirst = itemRefs.current[0];
          if (el !== null && newFirst != null && oldFirst != null) {
            headTrim = {
              scrollTop: el.scrollTop,
              removed: newFirst.offsetTop - oldFirst.offsetTop,
              expectedFirst: entries[trimCount]?.seq ?? -1,
            };
          }
        }
        setState((current) => {
          if (current.kind !== "ready") return current;
          const curLast = current.data.entries[current.data.entries.length - 1];
          // 连续性不变式：续页必须正好接在当前窗口之下（首条 seq =
          // 当前末条 seq + 1），否则丢弃——防一切竞态拼出乱序/重复窗口。
          const continuous =
            page.entries.length === 0 ||
            (curLast !== undefined && page.entries[0]?.seq === curLast.seq + 1);
          if (!continuous) return current;
          const appended =
            page.entries.length > 0
              ? [...current.data.entries, ...page.entries]
              : current.data.entries;
          // 探新无变化（无新条目且 total 未涨）不换 state，免无谓 rebind。
          if (appended === current.data.entries && page.total === current.data.total) {
            return current;
          }
          return {
            kind: "ready",
            data: {
              turns: current.data.turns,
              total: page.total,
              entries:
                appended.length > TRANSCRIPT_WINDOW_MAX
                  ? appended.slice(appended.length - TRANSCRIPT_WINDOW_MAX)
                  : appended,
            },
          };
        });
        headTrimRef.current = headTrim;
        refreshTurns();
        return undefined;
      })
      .catch(() => {})
      .finally(() => {
        loadingMoreRef.current = false;
      });
  }, [state, row, refreshTurns]);

  // loadAfter 的稳定引用：索引重建完成的回调里重探用（避免闭包过期）。
  const loadAfterRef = useRef(loadAfter);
  const rebuildingRef = useRef(false);
  useEffect(() => {
    loadAfterRef.current = loadAfter;
  }, [loadAfter]);

  const loadBefore = useCallback(() => {
    if (state.kind !== "ready" || loadingMoreRef.current) return;
    const first = state.data.entries[0];
    if (first === undefined || first.seq === 0) return;
    const el = streamRef.current;
    if (el !== null) {
      prependAnchorRef.current = {
        scrollTop: el.scrollTop,
        firstTop: itemRefs.current[0]?.offsetTop ?? 0,
        prevFirst: first.seq,
        prepended: null,
      };
    }
    const gen = windowGenRef.current;
    loadingMoreRef.current = true;
    fetchTranscriptPage(row.tool, row.externalId, null, first.seq, TRANSCRIPT_PAGE_SIZE)
      .then((page) => {
        // 换代 / 失效 / 空页：不能合并，也不能让过期锚点参与回位。
        if (gen !== windowGenRef.current || !page.built || page.entries.length === 0) {
          prependAnchorRef.current = null;
          return undefined;
        }
        // 乐观置位补页数：回位按"旧首条新下标"取元素。合并是否成立由
        // updater 的连续性检查定——被拒时窗口首条不变，回位守卫会跳过。
        const anchor = prependAnchorRef.current;
        if (anchor !== null) anchor.prepended = page.entries.length;
        setState((current) => {
          if (current.kind !== "ready") return current;
          const curFirst = current.data.entries[0];
          // 连续性不变式：补页必须正好接在当前窗口之上（末条 seq =
          // 当前首条 seq − 1），否则丢弃——防竞态拼出重复乱序窗口。
          const lastFetched = page.entries[page.entries.length - 1];
          if (curFirst === undefined || lastFetched?.seq !== curFirst.seq - 1) {
            return current;
          }
          const merged = [...page.entries, ...current.data.entries];
          return {
            kind: "ready",
            data: {
              turns: current.data.turns,
              total: current.data.total,
              // 窗口上限：顶部补页后裁掉远端（尾部）。视口在顶部，无感；
              // 也不影响回位补偿——锚点按元素偏移差计算，与尾部裁剪正交。
              entries:
                merged.length > TRANSCRIPT_WINDOW_MAX
                  ? merged.slice(0, TRANSCRIPT_WINDOW_MAX)
                  : merged,
            },
          };
        });
      })
      .catch(() => {
        prependAnchorRef.current = null;
      })
      .finally(() => {
        loadingMoreRef.current = false;
      });
  }, [state, row]);

  // 向前补页后：新内容插在上方，按旧首条元素提交前后的偏移差平移视口，
  // 视觉上"原地不动"。prepended 未置位说明补页在途（期间可能有 turns
  // 刷新的提交），锚点留着不消费；合并被连续性检查拒绝时窗口首条不变，
  // 守卫直接跳过并清锚。
  useLayoutEffect(() => {
    if (state.kind !== "ready") return;
    const el = streamRef.current;
    const anchor = prependAnchorRef.current;
    if (el === null || anchor === null || anchor.prepended === null) return;
    prependAnchorRef.current = null;
    const first = state.data.entries[0]?.seq;
    if (first === undefined || first >= anchor.prevFirst) return;
    const restored = itemRefs.current[anchor.prepended];
    if (restored !== null && restored !== undefined) {
      el.scrollTop = anchor.scrollTop + (restored.offsetTop - anchor.firstTop);
    }
  }, [state]);

  // 向后补页触发头部裁剪后：视口上方少了 removed 像素，视口上移同量保持
  // 画面不动。合并被连续性检查拒绝时没有新提交，锚点由下一次提交消费——
  // expectedFirst 核对保证那种情况下不误补偿（窗口首条与预期不符即丢弃）。
  useLayoutEffect(() => {
    if (state.kind !== "ready") return;
    const el = streamRef.current;
    const anchor = headTrimRef.current;
    if (el === null || anchor === null) return;
    headTrimRef.current = null;
    if (state.data.entries[0]?.seq !== anchor.expectedFirst) return;
    el.scrollTop = anchor.scrollTop - anchor.removed;
  }, [state]);

  useEffect(() => {
    if (state.kind !== "ready") return undefined;
    const el = streamRef.current;
    if (el === null) return undefined;
    const onScroll = () => {
      if (el.scrollTop < 320) loadBefore();
      if (el.scrollTop + el.clientHeight >= el.scrollHeight - 640) loadAfter();
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    onScroll();
    return () => el.removeEventListener("scroll", onScroll);
  }, [state, loadBefore, loadAfter]);

  return (
    <>
      <header className="shrink-0 border-b border-border px-5 py-4">
        <div className="flex min-w-0 items-center gap-4">
          {/* 左栏身份：标题 / 坐标。宽度随内容收敛，切换按钮 ml-auto 右对齐。 */}
          <div className="min-w-0">
            <div className="flex min-w-0 items-center gap-2.5">
              {embedded ? null : (
                <span className="mr-1 shrink-0">
                  <Button variant="secondary" onClick={onBack}>
                    ← {strings.sessionsBack}
                  </Button>
                </span>
              )}
              <ToolIcon toolId={row.tool} size={20} />
              <div className="min-w-0">
                <h2 className="truncate text-sm font-semibold">
                  {row.title ?? strings.sessionsUntitled}
                </h2>
                <p className="truncate font-mono text-xs text-ink-muted">
                  {row.externalId}
                  {row.projectDir !== null && ` · ${row.projectDir}`}
                </p>
              </div>
            </div>
          </div>
          <div className="ml-auto shrink-0">
            <Segmented
              size="sm"
              label={strings.sessionsViewToggle}
              value={outlineView}
              onChange={setOutlineView}
              options={[
                { value: "list", label: strings.sessionsViewList },
                { value: "ruler", label: strings.sessionsViewRuler },
              ]}
            />
          </div>
        </div>
        {/* 第三行：时间范围 + 请求 / Tokens 聚合，小字弱色（数字加重），
            窄窗口 flex-wrap 换行而不是挤压截断。 */}
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs tabular-nums text-ink-muted">
          {range !== null && (
            <span>
              {range.start} → {range.end}
            </span>
          )}
          <span>
            <span className="text-ink-strong">{row.requestCount}</span>{" "}
            {strings.sessionsRequestsUnit}
          </span>
          <span>
            <span className="text-ink-strong">{formatTokens(totalTokens, numberUnit)}</span>{" "}
            {strings.detailsColTokens}
          </span>
        </div>
      </header>
      <div className="flex min-h-0 flex-1">
        {state.kind === "ready" && state.data.entries.length > 0 && (
          outlineView === "list" ? (
            <OutlineList
              key={row.id}
              ref={outlineListRef}
              items={outlineItems}
              strings={strings}
              initialTurn={lastTurnRef.current}
              onJump={jumpToTurn}
            />
          ) : (
            <TurnRuler
              key={row.id}
              ref={outlineListRef}
              turns={state.data.turns}
              strings={strings}
              initialTurn={lastTurnRef.current}
              onJump={jumpToTurn}
            />
          )
        )}
        <div
          ref={streamRef}
          className="relative min-h-0 flex-1 overflow-y-auto overflow-x-hidden px-5 py-4"
        >
          {state.kind === "loading" && (
            <p className="py-12 text-center text-sm text-ink-muted">
              {strings.transcriptLoading}
            </p>
          )}
          {state.kind === "building" && (
            <div className="py-12 text-center text-sm text-ink-muted">
              <p>{strings.transcriptIndexBuilding}</p>
              {state.total > 0 && (
                <p className="mt-1 text-xs tabular-nums">
                  {Math.min(100, Math.round((state.done / state.total) * 100))}%
                </p>
              )}
            </div>
          )}
          {state.kind === "failed" &&
            (state.code.startsWith("core.unsupported") ? (
              <UnsupportedTranscript
                tokens={totalTokens}
                numberUnit={numberUnit}
                strings={strings}
              />
            ) : state.code.startsWith("core.source_gone") ? (
              <SourceGoneTranscript row={row} numberUnit={numberUnit} strings={strings} />
            ) : (
              <div className="py-12 text-center text-sm text-ink-muted">
                <p>
                  {state.code.startsWith("core.transcript_oversize") ? (
                    strings.transcriptOversize
                  ) : (
                    strings.transcriptLoadFailed
                  )}
                </p>
                {!state.code.startsWith("core.transcript_oversize") && (
                  <p className="mt-1 text-xs">{strings.transcriptLoadFailedHint}</p>
                )}
              </div>
            ))}
          {state.kind === "ready" && state.data.entries.length === 0 && (
            <EmptyTranscript row={row} strings={strings} />
          )}
          {state.kind === "ready" && state.data.entries.length > 0 && (
            <div className="flex flex-col gap-5">
              {state.data.entries.map((pageEntry, index) => (
                <div
                  key={pageEntry.seq}
                  ref={(el) => {
                    itemRefs.current[index] = el;
                  }}
                >
                  <StreamEntry entry={pageEntry.entry} strings={strings} />
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </>
  );
}
