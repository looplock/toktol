/**
 * 扫描状态的前端投影。自动循环常驻 Rust 壳（`toktol-core::scan::scheduler`，
 * 与窗口生死无关），前端只做四件事：手动触发（通知壳）、监听 started/finished
 * 维护"扫描中/气泡/最近错误"、started 时拉一次首扫量级（多 GB 大文件首次
 * 解析的剩余量，气泡用它解释"这轮为什么久"）、数据有变化时递增 dataVersion
 * 供数据页刷新。退避规则也在壳里（同口径纯函数），前端没有间隔逻辑。
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  fetchScanBacklog,
  isTauriRuntime,
  scanTrigger,
  type ScanBacklog,
  type ScanReport,
} from "../api";
import type { ScanOutcome } from "../scanToast";

const SCAN_STARTED_EVENT = "scan://started";
const SCAN_FINISHED_EVENT = "scan://finished";

/** 壳发的 finished 事件载荷；失败只带稳定错误码。 */
interface ScanFinishedPayload {
  readonly ok: boolean;
  readonly report?: ScanReport;
  readonly error?: string;
}

/** 扫描是否带来了新数据（新记录或新会话）。 */
export function scanFoundData(report: ScanReport): boolean {
  return report.recordsInserted > 0 || report.sessionsCreated > 0;
}

export interface ScanScheduler {
  /** 有扫描在飞（自动或手动）。 */
  readonly scanning: boolean;
  /** 在飞的这轮是不是用户点的：气泡据此决定要不要报"正在进行"。 */
  readonly manualRun: boolean;
  /** 最近一轮结束的结果（含失败）；初始为 null = 还没扫过。 */
  readonly outcome: ScanOutcome | null;
  /** 最近一次扫描的错误；成功后清空。 */
  readonly lastError: string | null;
  /** 数据有变化的扫描完成次数（新增记录或新增会话），数据页的刷新信号。 */
  readonly dataVersion: number;
  /**
   * 本轮开始时的首扫量级（多 GB 大文件首次解析的剩余量）；非扫描中为 null，
   * 查询失败也为 null。气泡据此解释"这轮为什么久"。
   */
  readonly backlog: ScanBacklog | null;
  /** 手动触发一次扫描。 */
  triggerManual: () => void;
}

export function useScanScheduler(): ScanScheduler {
  const [scanning, setScanning] = useState(false);
  const [manualRun, setManualRun] = useState(false);
  const [outcome, setOutcome] = useState<ScanOutcome | null>(null);
  const [lastError, setLastError] = useState<string | null>(null);
  const [dataVersion, setDataVersion] = useState(0);
  const [backlog, setBacklog] = useState<ScanBacklog | null>(null);
  // 触发标记：manualRun 是状态（气泡要读），ref 是事件回调查的"这轮是否手动"。
  const manualRef = useRef(false);
  // 完成序号：连着两轮一模一样的报告也要能被认成两次，"新一轮"不能靠报告内容判断。
  const seqRef = useRef(0);
  // 轮次序号：started 时拉积压量是异步的，finished 先到时慢返回的旧结果
  // 不能再把 backlog 写回去——每次 started 自增，写回前对号。
  const roundRef = useRef(0);

  useEffect(() => {
    if (!isTauriRuntime()) return undefined;
    let alive = true;
    const unlistens: Promise<() => void>[] = [
      listen(SCAN_STARTED_EVENT, () => {
        setScanning(true);
        const round = ++roundRef.current;
        fetchScanBacklog()
          .then((info) => {
            if (alive && round === roundRef.current) setBacklog(info);
          })
          .catch(() => {
            if (alive && round === roundRef.current) setBacklog(null);
          });
      }),
      listen<ScanFinishedPayload>(SCAN_FINISHED_EVENT, (event) => {
        const { ok, report, error } = event.payload;
        roundRef.current += 1; // 作废在途的积压查询
        setScanning(false);
        setBacklog(null);
        if (!alive) return;
        const manual = manualRef.current;
        manualRef.current = false;
        if (ok && report) {
          setOutcome({ seq: seqRef.current++, manual, report });
          setLastError(null);
          if (scanFoundData(report)) setDataVersion((version) => version + 1);
        } else {
          setOutcome({ seq: seqRef.current++, manual, report: null });
          setLastError(error ?? "unknown");
        }
      }),
    ];
    return () => {
      alive = false;
      unlistens.forEach((p) => void p.then((fn) => fn()));
    };
  }, []);

  const triggerManual = useCallback(() => {
    if (!isTauriRuntime()) return;
    // 乐观进"扫描中"：壳的 Started 事件随后会确认；手动触发的旗标由
    // finished 事件消费（这轮的气泡按手动口径展示）。
    manualRef.current = true;
    setManualRun(true);
    setScanning(true);
    scanTrigger().catch(() => {
      manualRef.current = false;
      setManualRun(false);
      setScanning(false);
      setLastError("trigger failed");
    });
  }, []);

  return { scanning, manualRun, outcome, lastError, dataVersion, backlog, triggerManual };
}
