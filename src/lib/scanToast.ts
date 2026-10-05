/**
 * 扫描气泡的判定与文案：什么时候出、出的这句话怎么说。
 *
 * 刻意不 import react：这里的都是纯函数。气泡的显示时长要靠 window.setTimeout
 * 排，node 测试环境（无 DOM）跑不到，所以判定与文案留在这一层，计时在 hook 里。
 *
 * 可见性策略的由头：调度器每 30s 自动扫一轮，全量可见等于每半分钟闪一次气泡。
 * 所以——
 *   running：只给手动触发。用户点了按钮就该立刻看到反馈；
 *   done：手动必出（哪怕 0 条，"点了没反应"最难受）；自动扫描只在真扫到东西时出声；
 *   failed：一律出。同一件错事不该因为是自己跑的就藏起来。
 */

import type { ScanBacklog, ScanReport } from "./api";
import { formatByteSize, formatCount } from "./format";
import { scanFoundData } from "./hooks/useScanScheduler";

export type ScanToastKind = "running" | "done" | "failed";

/**
 * 一轮扫描的结果。`seq` 每次结束自增：自动扫描会连着产出两份一模一样的报告，
 * 没有它，第二次的结果会被当成"还是上一次那句话"而不再露头。
 */
export interface ScanOutcome {
  readonly seq: number;
  readonly manual: boolean;
  /** null = 失败。原因太长且已在顶栏按钮的悬停提示里，气泡只说结果。 */
  readonly report: ScanReport | null;
}

export type ScanToastView =
  | { readonly kind: "running"; readonly backlog: ScanBacklog | null }
  | { readonly kind: "done"; readonly seq: number; readonly report: ScanReport }
  | { readonly kind: "failed"; readonly seq: number };

export interface ScanToastInput {
  readonly scanning: boolean;
  /** 正在跑的这轮是不是用户点的（只在 scanning 为真时有意义）。 */
  readonly manual: boolean;
  readonly outcome: ScanOutcome | null;
  /** 本轮开始时取到的首扫量级；取不到为 null（见 useScanScheduler）。 */
  readonly backlog: ScanBacklog | null;
}

/**
 * 首扫量级提示阈值：剩余字节到一个超限文件（256 MB，与扫描层单文件上限同口径）
 * 的量级才值得提示——尾部几 MB 的余量是常态，不该让"这轮会久"的警告贬值。
 */
export const BACKLOG_HINT_BYTES = 256 * 1024 * 1024;

/** 决定这一刻该出什么气泡；不该出的时候返回 null。 */
export function scanToastView(input: ScanToastInput): ScanToastView | null {
  if (input.scanning) {
    const backlog =
      input.backlog !== null && input.backlog.bytes >= BACKLOG_HINT_BYTES
        ? input.backlog
        : null;
    return input.manual ? { kind: "running", backlog } : null;
  }

  const outcome = input.outcome;
  if (outcome === null) {
    return null;
  }
  if (outcome.report === null) {
    return { kind: "failed", seq: outcome.seq };
  }
  if (!outcome.manual && !scanFoundData(outcome.report)) {
    return null;
  }

  return { kind: "done", seq: outcome.seq, report: outcome.report };
}

/** 结果气泡停留时长：4 秒够读完一行字，又不会赖在右下角不走。 */
export const TOAST_DONE_MS = 4_000;
/** 失败多留 2 秒：这句话要引着用户去顶栏翻失败原因，读不完就消失更烦。 */
export const TOAST_FAILED_MS = 6_000;

export interface ScanToastLabels {
  /** 模板："新增 {n} 条记录"。 */
  readonly scanToastRecords: string;
  /** 模板："新增 {n} 个会话"。 */
  readonly scanToastSessions: string;
  readonly scanToastNoChanges: string;
  /** 模板："首次解析大文件：剩余 {size}"。 */
  readonly scanToastBacklog: string;
}

/**
 * done 气泡的副标题：先报记录、没记录才报会话，都没有就说"没有新记录"——
 * 会话但没有用量（常见于只写了 header 的工具）值得一说，但记录数才是用户关心的。
 */
export function scanToastDetail(report: ScanReport, labels: ScanToastLabels): string {
  if (report.recordsInserted > 0) {
    return labels.scanToastRecords.replace("{n}", formatCount(report.recordsInserted));
  }
  if (report.sessionsCreated > 0) {
    return labels.scanToastSessions.replace("{n}", formatCount(report.sessionsCreated));
  }

  return labels.scanToastNoChanges;
}

/** running 气泡的首扫量级副标题（"剩余 12.8 GB"），解释这轮为什么久。 */
export function scanToastBacklogDetail(
  backlog: ScanBacklog,
  labels: Pick<ScanToastLabels, "scanToastBacklog">,
): string {
  return labels.scanToastBacklog.replace("{size}", formatByteSize(backlog.bytes));
}
