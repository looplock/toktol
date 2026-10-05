/**
 * 气泡的显示时长归属：running 跟着 scanning 走，done/failed 到点自动消失。
 * 同一轮只允许排一次计时器（依赖里只放 kind + seq），父组件重渲染不会把
 * 倒计时按回去——否则扫描一结束就被后续渲染无限续命。
 */

import { useEffect, useState } from "react";

import {
  TOAST_DONE_MS,
  TOAST_FAILED_MS,
  scanToastView,
  type ScanToastInput,
  type ScanToastView,
} from "../scanToast";

/** 没有被隐藏过的结果：真实的 seq 从 0 起，用 -1 当"无"。 */
const NO_SEQ = -1;

export function useScanToast(input: ScanToastInput): ScanToastView | null {
  const view = scanToastView(input);
  const [hiddenSeq, setHiddenSeq] = useState(NO_SEQ);

  const kind = view?.kind ?? null;
  const seq = view !== null && view.kind !== "running" ? view.seq : NO_SEQ;

  useEffect(() => {
    if (kind === null || kind === "running") return undefined;

    const timer = window.setTimeout(
      () => setHiddenSeq(seq),
      kind === "done" ? TOAST_DONE_MS : TOAST_FAILED_MS,
    );
    return () => window.clearTimeout(timer);
  }, [kind, seq]);

  if (view === null) return null;

  return view.kind !== "running" && view.seq === hiddenSeq ? null : view;
}
