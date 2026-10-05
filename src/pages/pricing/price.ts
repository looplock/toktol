/** 定价页共享的纯逻辑：四桶常量、价格草稿解析、状态判定。不含 React。 */

import type { ModelPriceInput, PricingModelRow } from "../../lib/api";
import { formatCostMicros } from "../../lib/format";

/** 四桶编辑的受控输入值（USD 小数字符串；空串 = 该桶无价）。 */
export interface PriceDraft {
  readonly input: string;
  readonly output: string;
  readonly cacheRead: string;
  readonly cacheWrite: string;
}

/** 状态筛选：全部 / 已定价 / 待确认（目录有建议价未采纳）/ 未定价。 */
export type StatusFilter = "all" | "priced" | "pending" | "unpriced";

export const BUCKETS = [
  ["input", "pricingBucketInput"],
  ["output", "pricingBucketOutput"],
  ["cacheRead", "pricingBucketRead"],
  ["cacheWrite", "pricingBucketWrite"],
] as const;

/** "并入…"候选：目录规范名（rename_model，补价）或本地模型（merge_models）。 */
export interface UnifyCandidate {
  readonly id: string;
  readonly provider: string | null;
  readonly status: string | null;
  readonly kind: "catalog" | "local";
}

export function draftFromRow(row: {
  inputPerMtokMicros: number | null;
  outputPerMtokMicros: number | null;
  cacheReadPerMtokMicros: number | null;
  cacheWritePerMtokMicros: number | null;
}): PriceDraft {
  const text = (micros: number | null): string =>
    micros === null ? "" : String(micros / 1_000_000);
  return {
    input: text(row.inputPerMtokMicros),
    output: text(row.outputPerMtokMicros),
    cacheRead: text(row.cacheReadPerMtokMicros),
    cacheWrite: text(row.cacheWritePerMtokMicros),
  };
}

/** USD 小数文本 → 微美元；空串 = 无价（null），非法数字返回 undefined 供拦截保存。 */
export function parseDraftField(text: string): number | null | undefined {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  if (!Number.isFinite(value) || value < 0) return undefined;
  return Math.round(value * 1_000_000);
}

export function parseDraft(draft: PriceDraft): ModelPriceInput | undefined {
  const input = parseDraftField(draft.input);
  const output = parseDraftField(draft.output);
  const cacheRead = parseDraftField(draft.cacheRead);
  const cacheWrite = parseDraftField(draft.cacheWrite);
  if (
    input === undefined ||
    output === undefined ||
    cacheRead === undefined ||
    cacheWrite === undefined
  ) {
    return undefined;
  }
  return { input, output, cacheRead, cacheWrite };
}

/** 目录建议价是否可用（有条目且至少一桶有价）——"待确认"与"未定价"的分界。 */
export function hasSuggestion(row: PricingModelRow): boolean {
  const hint = row.catalog;
  return (
    hint !== null &&
    [hint.inputPerMtokMicros, hint.outputPerMtokMicros, hint.cacheReadPerMtokMicros, hint.cacheWritePerMtokMicros].some(
      (v) => v !== null,
    )
  );
}

export function bucketText(
  row: PricingModelRow,
  bucket: (typeof BUCKETS)[number][0],
): string {
  const micros = {
    input: row.inputPerMtokMicros,
    output: row.outputPerMtokMicros,
    cacheRead: row.cacheReadPerMtokMicros,
    cacheWrite: row.cacheWritePerMtokMicros,
  }[bucket];
  return micros === null ? "–" : formatCostMicros(micros);
}

export function microText(micros: number | null): string {
  return micros === null ? "–" : formatCostMicros(micros);
}
