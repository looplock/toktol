/**
 * 定价页容器：状态机与数据装载，界面拆在 pricing/ 目录（列表/详情/变体/并入）。
 * 左列表固定（搜索 + 状态筛选承担未定价/已定价切换），右栏一次只呈现一件事——
 * 主操作由选中模型的状态唯一决定：
 * - 未定价 + 目录有建议价 → 采纳建议价并重算（建议价压成一行）；
 * - 未定价 + 无目录价 → 四桶手填（编辑器常开，红线：不编价）；
 * - 有价 + 映射未确认 → 确认映射（全部变体转正）；
 * - 有价 + 已确认 → 无主按钮，改价走行内铅笔。
 * 变体行内改指向纠错；改名与合并统一收敛为底部"并入其他模型…"：选目录项走
 * rename_model（自动补价），选本地项走 merge_models。
 * 单价口径：库里微美元 / 每百万 token，页面按 USD 小数展示（×1e6 回库）。
 * 两级来源 `catalog < user`：目录同步只补缺，用户改过的价永不覆盖。
 * 所有写操作后端同事务自动重算，前端只需重拉 overview + 弹气泡。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Card } from "../components/ui/Card";
import { PageShell } from "../components/PageShell";
import { ScanToast } from "../components/ScanToast";
import type { Strings } from "../i18n/strings";
import {
  confirmModel,
  fetchPricingOverview,
  mergeModels,
  renameModel,
  setModelMapping,
  setModelPrice,
  syncModelCatalog,
  type CatalogModelBrief,
  type PricingModelRow,
  type PricingOverviewPayload,
} from "../lib/api";
import { formatCount, formatTimestamp } from "../lib/format";
import { TOAST_DONE_MS, type ScanToastKind } from "../lib/scanToast";
import {
  hasSuggestion,
  parseDraft,
  type PriceDraft,
  type StatusFilter,
  type UnifyCandidate,
} from "./pricing/price";
import { ModelDetailPane } from "./pricing/ModelDetailPane";
import { ModelListPane } from "./pricing/ModelListPane";

interface PricingPageProps {
  readonly strings: Strings;
  /** 自动/手动扫描带来新模型时递增：定价概览跟着重拉。 */
  readonly scanVersion: number;
  /** 测试缝隙：注入首帧数据（SSR 不跑 effect，IPC 拉取自然被跳过）。 */
  readonly initialOverview?: PricingOverviewPayload;
  /** 测试缝隙：SSR 直接以"改价编辑中"渲染（跳过点击铅笔）。 */
  readonly initialEditing?: boolean;
  /** 测试缝隙：复制价格选单的初始搜索词。 */
  readonly initialCopyQuery?: string;
}

export function PricingPage({
  strings,
  scanVersion,
  initialOverview,
  initialEditing = false,
  initialCopyQuery = "",
}: PricingPageProps) {
  const [payload, setPayload] = useState<PricingOverviewPayload | null>(
    initialOverview ?? null,
  );
  const [loadError, setLoadError] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  /** 左列表当前选中的标准模型 id；null = 跟随 filtered 首项。 */
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  /** 写操作/目录同步的右下角气泡（复用扫描气泡）：成功即报，几秒后自行消失。 */
  const [toast, setToast] = useState<
    { seq: number; kind: ScanToastKind; title: string; detail?: string } | null
  >(null);
  const toastSeq = useRef(0);
  const toastTimer = useRef<number | undefined>(undefined);

  const reload = useCallback(() => {
    fetchPricingOverview()
      .then((next) => {
        setPayload(next);
        setLoadError(null);
      })
      .catch((err: unknown) => setLoadError(String(err)));
  }, []);

  const popToast = useCallback((kind: ScanToastKind, title: string, detail?: string) => {
    toastSeq.current += 1;
    const seq = toastSeq.current;
    // exactOptionalPropertyTypes：detail 缺省时不能显式塞 undefined。
    setToast({ seq, kind, title, ...(detail === undefined ? {} : { detail }) });
    if (toastTimer.current !== undefined) window.clearTimeout(toastTimer.current);
    // 进行中不自动消失（同步可能比 4 秒久），结果形态到点自散。
    if (kind === "running") return;
    toastTimer.current = window.setTimeout(() => {
      setToast((prev) => (prev !== null && prev.seq === seq ? null : prev));
    }, TOAST_DONE_MS);
  }, []);

  useEffect(() => {
    reload();
  }, [reload, scanVersion]);

  const models = useMemo(() => payload?.models ?? [], [payload]);
  const allIds = useMemo(() => models.map((m) => m.id), [models]);
  const catalogModels = useMemo(() => payload?.catalogModels ?? [], [payload]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return models.filter((row) => {
      const pending = row.priceSource === null && hasSuggestion(row);
      if (statusFilter === "priced" && row.priceSource === null) return false;
      if (statusFilter === "pending" && !pending) return false;
      if (statusFilter === "unpriced" && (row.priceSource !== null || pending)) return false;
      if (q === "") return true;
      return (
        row.id.toLowerCase().includes(q) ||
        row.variants.some((v) => v.rawModel.toLowerCase().includes(q))
      );
    });
  }, [models, query, statusFilter]);

  /** 各档计数：下拉选项上的分面数字，待确认还剩几件活一眼清楚。 */
  const statusCounts = useMemo(
    () => ({
      priced: models.filter((m) => m.priceSource !== null).length,
      pending: models.filter((m) => m.priceSource === null && hasSuggestion(m)).length,
      unpriced: models.filter((m) => m.priceSource === null && !hasSuggestion(m)).length,
    }),
    [models],
  );

  /** 单选语义接进 MultiSelect：不选 = 全部；再点已选项或清除 = 回到全部。 */
  const statusSelection = useMemo(
    () => new Set(statusFilter === "all" ? [] : [statusFilter]),
    [statusFilter],
  );

  const selected = models.find((m) => m.id === selectedId) ?? filtered[0] ?? null;

  /** 重算气泡副标题：后端有计数报计数，没有就笼统说重算过了。 */
  function recostDetail(count: number | null): string {
    return count === null
      ? strings.pricingToastRecost
      : strings.pricingToastRepriced.replace("{n}", formatCount(count));
  }

  function saveDraft(modelId: string, draft: PriceDraft): void {
    const price = parseDraft(draft);
    if (price === undefined) return; // 有非法数字：保存按钮已禁用，这里兜底。
    setModelPrice(modelId, price)
      .then(() => {
        reload();
        popToast("done", strings.pricingToastPrice, strings.pricingToastRecost);
      })
      .catch((err: unknown) => setLoadError(String(err)));
  }

  function applyCatalogPrice(entry: CatalogModelBrief): void {
    if (selectedId === null) return;
    setModelPrice(selectedId, {
      input: entry.inputPerMtokMicros,
      output: entry.outputPerMtokMicros,
      cacheRead: entry.cacheReadPerMtokMicros,
      cacheWrite: entry.cacheWritePerMtokMicros,
    })
      .then(() => {
        reload();
        popToast("done", strings.pricingToastCopyPrice, strings.pricingToastRecost);
      })
      .catch((err: unknown) => setLoadError(String(err)));
  }

  function confirmSuggestion(row: PricingModelRow): void {
    const hint = row.catalog;
    if (hint === null) return;
    setModelPrice(row.id, {
      input: hint.inputPerMtokMicros,
      output: hint.outputPerMtokMicros,
      cacheRead: hint.cacheReadPerMtokMicros,
      cacheWrite: hint.cacheWritePerMtokMicros,
    })
      .then(() => {
        reload();
        popToast("done", strings.pricingToastSuggestion, strings.pricingToastRecost);
      })
      .catch((err: unknown) => setLoadError(String(err)));
  }

  function runSync(): void {
    setSyncing(true);
    popToast("running", strings.pricingToastSyncRunning);
    syncModelCatalog()
      .then((result) => {
        setSyncing(false);
        reload();
        // 收成口径与扫描气泡一致：有变化报数字，没变化明说无变化。
        const parts = [
          ...(result.priced > 0
            ? [strings.pricingToastSyncPriced.replace("{count}", String(result.priced))]
            : []),
          ...(result.remapped > 0
            ? [strings.pricingToastSyncRemapped.replace("{count}", String(result.remapped))]
            : []),
        ];
        popToast(
          "done",
          strings.pricingToastSyncDone,
          parts.length === 0 ? strings.pricingSyncNoChange : parts.join(" · "),
        );
      })
      .catch((err: unknown) => {
        setSyncing(false);
        popToast("failed", strings.pricingSyncFailed, String(err));
      });
  }

  function repoint(rawModel: string, modelId: string): void {
    setModelMapping(rawModel, modelId)
      .then((n) => {
        reload();
        popToast("done", strings.pricingToastRepoint, recostDetail(n));
      })
      .catch((err: unknown) => setLoadError(String(err)));
  }

  function confirmMapping(modelId: string): void {
    confirmModel(modelId)
      .then((n) => {
        reload();
        popToast("done", strings.pricingToastMapping, recostDetail(n));
      })
      .catch((err: unknown) => setLoadError(String(err)));
  }

  function runUnify(target: UnifyCandidate): void {
    if (selectedId === null || target.id === selectedId) return;
    const op =
      target.kind === "catalog"
        ? renameModel(selectedId, target.id).then((n) =>
            popToast("done", strings.pricingToastRename, recostDetail(n)),
          )
        : mergeModels(selectedId, target.id).then((n) =>
            popToast("done", strings.pricingToastMerge, recostDetail(n)),
          );
    op.then(() => reload()).catch((err: unknown) => setLoadError(String(err)));
  }

  const syncedAt = payload?.syncedAt == null ? null : formatTimestamp(payload.syncedAt);

  return (
    <PageShell fill>
      {loadError === null ? null : (
        <Card className="border-danger px-4 py-2 text-sm text-danger">
          {strings.pricingLoadError}：{loadError}
        </Card>
      )}

      <div className="grid min-h-0 flex-1 grid-cols-[280px_minmax(0,1fr)] items-stretch gap-4">
        <ModelListPane
          strings={strings}
          rows={filtered}
          hasModels={models.length > 0}
          selectedId={selected?.id ?? null}
          query={query}
          statusCounts={statusCounts}
          statusSelection={statusSelection}
          syncing={syncing}
          syncedAt={syncedAt}
          onQueryChange={setQuery}
          onStatusToggle={(value) =>
            setStatusFilter((prev) => (prev === value ? "all" : (value as StatusFilter)))
          }
          onStatusClear={() => setStatusFilter("all")}
          onSelect={setSelectedId}
          onSync={runSync}
        />

        {/* ── 右栏：主操作由状态唯一决定；key 让编辑/选单随选中切换重置 ── */}
        {selected === null ? (
          <Card padding="none" className="flex min-h-0 items-center justify-center">
            <p className="px-6 py-16 text-sm text-ink-muted">{strings.pricingSelectHint}</p>
          </Card>
        ) : (
          <ModelDetailPane
            key={selected.id}
            strings={strings}
            selected={selected}
            allIds={allIds}
            models={models}
            catalogModels={catalogModels}
            initialEditing={initialEditing}
            initialCopyQuery={initialCopyQuery}
            onConfirmSuggestion={confirmSuggestion}
            onConfirmMapping={confirmMapping}
            onSaveDraft={saveDraft}
            onApplyCatalogPrice={applyCatalogPrice}
            onRepoint={repoint}
            onUnify={runUnify}
          />
        )}
      </div>

      {/* 写操作成功的右下角气泡：形态与扫描气泡同款，成功即报、4 秒自散。 */}
      {toast === null ? null : (
        <ScanToast
          kind={toast.kind}
          title={toast.title}
          {...(toast.detail === undefined ? {} : { detail: toast.detail })}
        />
      )}
    </PageShell>
  );
}
