/**
 * 扫描的纯逻辑单测。自动循环与退避规则已下沉 Rust 壳
 * （toktol-core::scan::scheduler，退避口径的测试在 Rust 侧），
 * 前端只剩"扫到新数据"这一个纯判断，供事件回调与气泡共用。
 */

import { expect, it } from "vitest";

import type { ScanReport } from "../api";
import { scanFoundData } from "./useScanScheduler";

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

it("扫到新记录或新会话都算有数据", () => {
  expect(scanFoundData(report({ recordsInserted: 1 }))).toBe(true);
  expect(scanFoundData(report({ sessionsCreated: 1 }))).toBe(true);
  expect(scanFoundData(report({}))).toBe(false);
});
