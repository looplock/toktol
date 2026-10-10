/** 右栏工作台：一次只呈现一件事——主操作由选中模型的状态唯一决定。
 * 未定价 + 目录有建议价 → 采纳；未定价 + 无目录价 → 四桶手填（编辑器常开）；
 * 有价 + 映射未确认 → 确认映射；有价 + 已确认 → 无主按钮，改价走行内铅笔。
 * 编辑/草稿/复制选单是本组件的内部状态，随选中模型切换重置（容器带 key）。
 * 写操作经回调上行（页内跑 IPC + 重拉 + 气泡）。 */

import { useState } from "react";

import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { PencilIcon } from "../../components/ui/icons";
import type { Strings } from "../../i18n/strings";
import type { CatalogModelBrief, PricingModelRow } from "../../lib/api";
import { ModelIcon } from "./ModelIcon";
import {
  BUCKETS,
  bucketText,
  microText,
  parseDraft,
  draftFromRow,
  type PriceDraft,
  type UnifyCandidate,
} from "./price";
import { UnifySection } from "./UnifySection";
import { VariantsSection } from "./VariantsSection";

interface ModelDetailPaneProps {
  readonly strings: Strings;
  readonly selected: PricingModelRow;
  /** 全部标准模型 id（变体改指向的下拉候选）。 */
  readonly allIds: readonly string[];
  /** 全部标准模型（并入候选的本地部分）。 */
  readonly models: readonly PricingModelRow[];
  readonly catalogModels: readonly CatalogModelBrief[];
  /** 测试缝隙：SSR 直接以"改价编辑中"渲染（跳过点击铅笔）。 */
  readonly initialEditing: boolean;
  /** 测试缝隙：复制价格选单的初始搜索词。 */
  readonly initialCopyQuery: string;
  readonly onConfirmSuggestion: (row: PricingModelRow) => void;
  readonly onConfirmMapping: (modelId: string) => void;
  readonly onSaveDraft: (modelId: string, draft: PriceDraft) => void;
  readonly onApplyCatalogPrice: (entry: CatalogModelBrief) => void;
  readonly onRepoint: (rawModel: string, modelId: string) => void;
  readonly onUnify: (target: UnifyCandidate) => void;
}

/** 单价盒子：只读展示或编辑输入（USD 小数，空串 = 无价）。 */
function PriceBox({
  label,
  text,
  editing,
  onChange,
}: {
  readonly label: string;
  readonly text: string;
  readonly editing: boolean;
  readonly onChange: (next: string) => void;
}) {
  return (
    <div className="rounded-control border border-border bg-surface-subtle px-3 py-2">
      <div className="text-xs text-ink-muted">{label}</div>
      {editing ? (
        <input
          value={text}
          inputMode="decimal"
          aria-label={label}
          onChange={(event) => onChange(event.target.value)}
          className="mt-1 h-6 w-full rounded-control border border-border bg-surface px-1.5 font-mono text-sm text-ink tabular-nums focus:border-accent focus:outline-none"
        />
      ) : (
        <div className="mt-0.5 font-mono text-sm tabular-nums text-ink-strong">{text}</div>
      )}
    </div>
  );
}

export function ModelDetailPane({
  strings,
  selected,
  allIds,
  models,
  catalogModels,
  initialEditing,
  initialCopyQuery,
  onConfirmSuggestion,
  onConfirmMapping,
  onSaveDraft,
  onApplyCatalogPrice,
  onRepoint,
  onUnify,
}: ModelDetailPaneProps) {
  const [editing, setEditing] = useState(initialEditing === true);
  const [draft, setDraft] = useState<PriceDraft | null>(null);
  /** "复制价格"选单的搜索词：目录模型 → 四桶价套用到当前模型。 */
  const [copyQuery, setCopyQuery] = useState(initialCopyQuery);

  const isUnpriced = selected.priceSource === null;
  const manualDraft = draft ?? draftFromRow(selected);
  const manualInvalid = parseDraft(manualDraft) === undefined;
  /** 目录没收录的未定价模型没有别的挂价路径：编辑器常开。 */
  const showEditor = editing || (isUnpriced && selected.catalog === null);
  const showSuggestion = isUnpriced && selected.catalog !== null && !editing;
  const mappingUnconfirmed =
    !isUnpriced && selected.mappingSource !== null && selected.mappingSource !== "user";

  function openEditor(): void {
    setEditing(true);
    setDraft(draftFromRow(selected));
  }

  function closeEditor(): void {
    setEditing(false);
    setDraft(null);
    setCopyQuery("");
  }

  /** 复制目录价：选中目录模型即把它的四桶价原样挂到当前模型（source=user，
      后端同事务重算），与"采纳建议价"同一写路径；缺价桶传 null（目录没给就缺，
      计价侧的缓存回落规则照常生效）。 */
  function applyCatalogPrice(entry: CatalogModelBrief): void {
    onApplyCatalogPrice(entry);
    closeEditor();
  }

  /** 复制价格候选：目录模型按 id / 展示名过滤。空词不出列表（目录上千条，
      全量是噪音）；排除当前模型已关联的目录条目——自己抄自己没有意义。 */
  const copyCandidates = (() => {
    const needle = copyQuery.trim().toLowerCase();
    if (needle === "") return [];
    const linkedId = selected.catalog?.modelId ?? null;
    return catalogModels.filter(
      (c) =>
        c.modelId !== linkedId &&
        (c.modelId.toLowerCase().includes(needle) ||
          (c.displayName !== null && c.displayName.toLowerCase().includes(needle))),
    );
  })();
  /** 列表渲染上限：匹配再多只显示前 30，继续输入自然缩小范围。 */
  const copyShown = copyCandidates.slice(0, 30);

  return (
    <Card
      padding="none"
      className="flex min-h-0 flex-col overflow-y-auto animate-[tt-fade-in_160ms_ease-out]"
    >
      {/* 头部 */}
      <div className="border-b border-border px-4 py-3">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="flex items-center gap-2 font-mono text-base text-ink-strong">
            <ModelIcon id={selected.id} size={16} />
            {selected.id}
          </h2>
          {selected.catalog?.status === "deprecated" ? (
            <Badge tone="unknown">{strings.pricingDeprecated}</Badge>
          ) : null}
        </div>
      </div>

      {/* 主操作：未定价+有建议价 → 采纳；有价+映射未确认 → 确认映射 */}
      {showSuggestion && selected.catalog !== null ? (
        <div className="border-b border-border px-4 py-3">
          <p className="font-mono text-xs tabular-nums text-ink-muted">
            {strings.pricingSuggestion}:{' '}
            {[
              microText(selected.catalog.inputPerMtokMicros),
              microText(selected.catalog.outputPerMtokMicros),
              microText(selected.catalog.cacheReadPerMtokMicros),
              microText(selected.catalog.cacheWritePerMtokMicros),
            ].join(" / ")}
          </p>
          <div className="mt-2 flex items-center gap-2">
            <Button variant="primary" onClick={() => onConfirmSuggestion(selected)}>
              {strings.pricingAdoptSuggestion}
            </Button>
            <Button variant="ghost" onClick={openEditor}>
              {strings.pricingManualPrice}
            </Button>
          </div>
        </div>
      ) : null}
      {mappingUnconfirmed ? (
        <div className="border-b border-border px-4 py-3">
          <Button variant="primary" onClick={() => onConfirmMapping(selected.id)}>
            {strings.pricingConfirmMapping}
          </Button>
          <p className="mt-1 text-xs text-ink-muted">{strings.pricingConfirmHint}</p>
        </div>
      ) : null}

      {/* 单价：已定价只读展示 + 行内改价；未定价手填（无目录价时常开） */}
      {!isUnpriced || showEditor ? (
        <div className="border-b border-border px-4 py-3">
          {/* 工具栏：左侧固定标题，右侧随状态切换——只读放改价入口；
              编辑时换成复制价格搜索 + 取消/保存，候选浮层向下盖住价格盒。 */}
          <div className="flex items-center gap-2">
            <span className="text-xs text-ink-muted">{strings.pricingUnitHint}</span>
            {showEditor ? (
              <div className="relative ml-auto flex items-center gap-1.5">
                {/* 复制价格选单：models.dev 目录模型按名过滤，选中即套用四桶价。
                    与"并入"选单同款交互：Enter 应用首条，Esc 关编辑器。 */}
                <input
                  autoFocus
                  value={copyQuery}
                  aria-label={strings.pricingCopyPriceSearch}
                  placeholder={strings.pricingCopyPriceSearch}
                  onChange={(event) => setCopyQuery(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Escape") {
                      closeEditor();
                    } else if (event.key === "Enter") {
                      const first = copyShown[0];
                      if (first !== undefined) applyCatalogPrice(first);
                    }
                  }}
                  className="h-8 w-48 rounded-control border border-border bg-surface px-2 font-mono text-sm text-ink placeholder:text-ink-muted focus:border-accent focus:outline-none"
                />
                {editing ? (
                  <Button variant="ghost" size="sm" onClick={closeEditor}>
                    {strings.pricingCancel}
                  </Button>
                ) : null}
                <Button
                  variant="primary"
                  size="sm"
                  disabled={manualInvalid}
                  onClick={() => onSaveDraft(selected.id, manualDraft)}
                >
                  {strings.pricingSave}
                </Button>
                {/* 候选浮层：锚定工具栏右侧，空词不出列表（目录上千条，全量是噪音）。 */}
                {copyQuery.trim() !== "" ? (
                  <ul className="absolute right-0 top-8 z-20 max-h-56 w-72 overflow-y-auto rounded-control border border-border bg-surface-raised py-1 shadow-raised">
                    {copyShown.map((entry) => (
                      <li key={entry.modelId}>
                        <button
                          type="button"
                          onClick={() => applyCatalogPrice(entry)}
                          title={`${entry.modelId} · ${entry.provider}`}
                          className="flex w-full items-start gap-2 px-2.5 py-1.5 text-left hover:bg-row-hover"
                        >
                          <span className="min-w-0 flex-1">
                            {/* 首行：模型 id + 供应商；次行：四桶价带文字标签
                                （箭头符号太密难读）。缓存桶只列目录给了价的。 */}
                            <span className="flex items-center gap-2">
                              <span className="truncate font-mono text-xs text-ink-strong">
                                {entry.modelId}
                              </span>
                              {entry.status === "deprecated" ? (
                                <Badge tone="unknown">{strings.pricingDeprecated}</Badge>
                              ) : null}
                              <span className="ml-auto shrink-0 text-xs text-ink-muted">
                                {entry.provider}
                              </span>
                            </span>
                            <span className="mt-0.5 flex flex-wrap gap-x-3 font-mono text-xs tabular-nums text-ink-muted">
                              <span>
                                {strings.pricingBucketInput} {microText(entry.inputPerMtokMicros)}
                              </span>
                              <span>
                                {strings.pricingBucketOutput}{" "}
                                {microText(entry.outputPerMtokMicros)}
                              </span>
                              {entry.cacheReadPerMtokMicros !== null ? (
                                <span>
                                  {strings.pricingBucketRead}{" "}
                                  {microText(entry.cacheReadPerMtokMicros)}
                                </span>
                              ) : null}
                              {entry.cacheWritePerMtokMicros !== null ? (
                                <span>
                                  {strings.pricingBucketWrite}{" "}
                                  {microText(entry.cacheWritePerMtokMicros)}
                                </span>
                              ) : null}
                            </span>
                          </span>
                        </button>
                      </li>
                    ))}
                    {copyShown.length === 0 ? (
                      <li className="px-2.5 py-2 text-xs text-ink-muted">
                        {catalogModels.length === 0
                          ? strings.pricingRenameSyncFirst
                          : strings.pricingNoMatch}
                      </li>
                    ) : null}
                  </ul>
                ) : null}
              </div>
            ) : !isUnpriced ? (
              <span className="ml-auto">
                <Button variant="ghost" size="sm" onClick={openEditor}>
                  <PencilIcon className="size-3.5" />
                  {strings.pricingEdit}
                </Button>
              </span>
            ) : null}
          </div>
          <div className="mt-2 grid grid-cols-2 gap-2">
            {BUCKETS.map(([bucket, labelKey]) => (
              <PriceBox
                key={bucket}
                label={strings[labelKey]}
                text={
                  showEditor && draft !== null ? draft[bucket] : bucketText(selected, bucket)
                }
                editing={showEditor}
                onChange={(next) =>
                  setDraft((prev) =>
                    prev === null ? draftFromRow(selected) : { ...prev, [bucket]: next },
                  )
                }
              />
            ))}
          </div>
          {isUnpriced && selected.catalog === null ? (
            <p className="mt-2 text-xs text-ink-muted">{strings.pricingManualNote}</p>
          ) : null}
        </div>
      ) : null}

      <VariantsSection
        strings={strings}
        selected={selected}
        allIds={allIds}
        isUnpriced={isUnpriced}
        onRepoint={onRepoint}
      />

      <UnifySection
        strings={strings}
        selectedId={selected.id}
        models={models}
        catalogModels={catalogModels}
        onUnify={onUnify}
      />
    </Card>
  );
}
