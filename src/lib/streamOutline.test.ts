import { describe, expect, it } from "vitest";

import {
  activeTurnFor,
  entryIndexForScroll,
  mapTurnsToWindow,
  SENTINEL_OFFSET,
  turnForEntry,
} from "./streamOutline";

// streamOutline 的纯查找逻辑（node 环境无 DOM，控制器部分靠 SSR 测试
// 与手动验证；热路径的正确性核心在这几个纯函数里）。

describe("entryIndexForScroll（滚动位置 → 当前条目）", () => {
  it("空缓存恒为 0", () => {
    expect(entryIndexForScroll([], 0)).toBe(0);
    expect(entryIndexForScroll([], 10000)).toBe(0);
  });

  it("未挂载的哨兵（+∞）不会被选中", () => {
    const offsets = [0, SENTINEL_OFFSET, SENTINEL_OFFSET];
    expect(entryIndexForScroll(offsets, 50)).toBe(0);
  });

  it("目标低于首条时为 0", () => {
    expect(entryIndexForScroll([100, 200, 300], 50)).toBe(0);
  });

  it("目标落在两条之间时取靠上那条", () => {
    expect(entryIndexForScroll([0, 100, 200], 150)).toBe(1);
  });

  it("目标恰好压线（<=）算命中", () => {
    expect(entryIndexForScroll([0, 100, 200], 100)).toBe(1);
  });

  it("目标越过末条时取末条", () => {
    expect(entryIndexForScroll([0, 100, 200], 500)).toBe(2);
  });

  it("全部是哨兵时为 0", () => {
    expect(entryIndexForScroll([SENTINEL_OFFSET, SENTINEL_OFFSET], 999)).toBe(0);
  });
});

describe("turnForEntry（条目 → 所属轮次）", () => {
  it("空轮次表返回 0", () => {
    expect(turnForEntry([], 5)).toBe(0);
  });

  it("单轮覆盖所有条目", () => {
    expect(turnForEntry([0], 0)).toBe(0);
    expect(turnForEntry([0], 42)).toBe(0);
  });

  it("条目落在两轮之间时归上一轮", () => {
    // 轮次从条目 0、3、7 开始。
    const starts = [0, 3, 7];
    expect(turnForEntry(starts, 0)).toBe(0);
    expect(turnForEntry(starts, 2)).toBe(0);
    expect(turnForEntry(starts, 3)).toBe(1);
    expect(turnForEntry(starts, 6)).toBe(1);
    expect(turnForEntry(starts, 7)).toBe(2);
    expect(turnForEntry(starts, 100)).toBe(2);
  });

  it("起始边界之前（理论不发生）兜底为 0", () => {
    expect(turnForEntry([5, 10], 0)).toBe(0);
  });
});

describe("activeTurnFor（滚动视口 → 当前轮次）", () => {
  const offsets = [0, 100, 200, 900];
  const starts = [0, 1, 2, 3]; // 每条消息各开一轮，共 4 轮。

  it("常规滚动按视口顶判定线（scrollTop + 48）归属", () => {
    // scrollTop=150：判定线 198 → 命中 offset 100（第 1 轮）。
    expect(activeTurnFor(offsets, 150, 800, 1700, starts)).toBe(1);
    // scrollTop=210：判定线 258 → 命中 offset 200（第 2 轮）。
    expect(activeTurnFor(offsets, 210, 800, 1700, starts)).toBe(2);
  });

  it("滚到底强制高亮最后一轮（末轮不足一屏时判定线够不到它）", () => {
    // 末轮锚点在 900，视口 800、内容 1700：最大 scrollTop = 900，到底判定线
    // 是 scrollTop >= 899。而常规判定线（scrollTop + 48）要够到 900 需要
    // scrollTop >= 852——不到底时末轮高亮不了，到底必须特判。
    expect(activeTurnFor(offsets, 850, 800, 1700, starts)).toBe(2);
    expect(activeTurnFor(offsets, 900, 800, 1700, starts)).toBe(3);
  });

  it("接近底部但未到底仍按判定线", () => {
    expect(activeTurnFor(offsets, 500, 800, 1700, starts)).toBe(2);
  });

  it("未加载到会话底时滚到底不强制（窗口底只是分页边界）", () => {
    expect(activeTurnFor(offsets, 900, 800, 1700, starts, false)).toBe(3);
  });

  it("空轮次表返回 0", () => {
    expect(activeTurnFor([], 0, 800, 800, [])).toBe(0);
  });
});

describe("mapTurnsToWindow（轮次 ↔ 已加载窗口映射）", () => {
  // 轮次稀疏：轮 0 开在条目 0，轮 1 开在条目 5，轮 2 开在条目 10。
  const turns = [{ seq: 0 }, { seq: 5 }, { seq: 10 }];

  it("窗口从会话顶开始：全部轮次按 seq 命中", () => {
    const m = mapTurnsToWindow(turns, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    expect(m.globals).toEqual([0, 1, 2]);
    expect(m.locals).toEqual([0, 5, 10]);
  });

  it("窗口顶切在轮中间：窗口外的轮 X 钉在 local 0 补虚拟起点", () => {
    // 窗口从条目 7 开始：轮 1（seq 5）开在窗口外，条目 7-9 属于它。
    const m = mapTurnsToWindow(turns, [7, 8, 9, 10, 11]);
    expect(m.globals).toEqual([1, 2]);
    expect(m.locals).toEqual([0, 3]);
  });

  it("恰有轮开在窗口顶：真轮自带 local 0，不推虚拟起点", () => {
    const m = mapTurnsToWindow(turns, [5, 6, 7, 10, 11]);
    expect(m.globals).toEqual([1, 2]);
    expect(m.locals).toEqual([0, 3]);
  });

  it("窗口顶在所有轮之后：条目归属最后一轮（虚拟起点钉在 local 0）", () => {
    // 窗口顶条目（如工具结果）属于其上方最后开轮的轮 2。
    const m = mapTurnsToWindow(turns, [20, 21]);
    expect(m.globals).toEqual([2]);
    expect(m.locals).toEqual([0]);
  });

  it("空窗口：空映射", () => {
    const m = mapTurnsToWindow(turns, []);
    expect(m.globals).toEqual([]);
    expect(m.locals).toEqual([]);
  });

  it("窗口出现重复 seq（不应发生，纵深防御）：取首次出现", () => {
    const m = mapTurnsToWindow(turns, [0, 1, 2, 0, 1, 2, 3]);
    expect(m.globals).toEqual([0]);
    expect(m.locals).toEqual([0]);
  });
});
