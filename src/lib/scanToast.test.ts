/**
 * 扫描气泡判定与文案的纯逻辑单测。可见性策略的意义见 lib/scanToast.ts 头注释：
 * 背景每 30s 自动扫一轮，全量可见会变成每半分钟闪一次的噪音。
 */

import { expect, it } from "vitest";

import type { ScanReport } from "./api";
import {
  scanToastBacklogDetail,
  scanToastDetail,
  scanToastView,
  type ScanToastLabels,
} from "./scanToast";

const LABELS: ScanToastLabels = {
  scanToastRecords: "新增 {n} 条记录",
  scanToastSessions: "新增 {n} 个会话",
  scanToastNoChanges: "没有新记录",
  scanToastBacklog: "首次解析大文件：剩余 {size}，这轮会久一些",
};

function report(partial: Partial<ScanReport>): ScanReport {
  return {
    filesScanned: 0,
    filesMissing: 0,
    filesSkipped: 0,
    sessionsCreated: 0,
    recordsInserted: 0,
    parseErrors: 0,
    ...partial,
  };
}

it("手动扫描：进行中报到单个 running", () => {
  expect(scanToastView({ scanning: true, manual: true, outcome: null, backlog: null })).toEqual({
    kind: "running",
    backlog: null,
  });
});

it("后台自动扫描：进行中不出气泡，避免每半分钟闪一次", () => {
  expect(scanToastView({ scanning: true, manual: false, outcome: null, backlog: null })).toBeNull();
});

it("手动扫描：空扫也给结果——用户点了就该知道扫没扫到", () => {
  const outcome = { seq: 0, manual: true, report: report({}) };
  expect(scanToastView({ scanning: false, manual: false, outcome, backlog: null })).toEqual({
    kind: "done",
    seq: 0,
    report: outcome.report,
  });
});

it("后台自动扫描：只有扫到新数据才出声", () => {
  const quiet = { seq: 0, manual: false, report: report({}) };
  expect(scanToastView({ scanning: false, manual: false, outcome: quiet, backlog: null })).toBeNull();

  const found = { seq: 1, manual: false, report: report({ recordsInserted: 3 }) };
  expect(scanToastView({ scanning: false, manual: false, outcome: found, backlog: null })?.kind).toBe("done");
});

it("失败一律出气泡（含后台自动轮）", () => {
  const failed = { seq: 2, manual: false, report: null };
  expect(scanToastView({ scanning: false, manual: false, outcome: failed, backlog: null })).toEqual({
    kind: "failed",
    seq: 2,
  });
});

it("还没扫过：没有结果也没有气泡", () => {
  expect(scanToastView({ scanning: false, manual: false, outcome: null, backlog: null })).toBeNull();
});

it("首扫量级：剩余到一个超限文件的量级才随 running 带出", () => {
  const big = { files: 3, bytes: 12.8 * 1024 ** 3 };
  expect(scanToastView({ scanning: true, manual: true, outcome: null, backlog: big })).toEqual({
    kind: "running",
    backlog: big,
  });
  // 尾部余量（几 MB）是常态，不值得警示。
  const tail = { files: 1, bytes: 3 * 1024 ** 2 };
  expect(scanToastView({ scanning: true, manual: true, outcome: null, backlog: tail })).toEqual({
    kind: "running",
    backlog: null,
  });
});

it("条数优先于会话，都没有才说没有新记录", () => {
  expect(scanToastDetail(report({ recordsInserted: 1200 }), LABELS)).toBe("新增 1,200 条记录");
  expect(scanToastDetail(report({ sessionsCreated: 2 }), LABELS)).toBe("新增 2 个会话");
  expect(scanToastDetail(report({}), LABELS)).toBe("没有新记录");
});

it("首扫量级文案以人类可读体积替换 {size}", () => {
  expect(scanToastBacklogDetail({ files: 9, bytes: 12.8 * 1024 ** 3 }, LABELS)).toBe(
    "首次解析大文件：剩余 12.8 GB，这轮会久一些",
  );
});
