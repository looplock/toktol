/** 变体区：来源徽章 + 行内改指向（未定价先看，改指在挂价后做也来得及）。
 * 改指向选择器一次只开一个（pickerRaw 记当前展开的变体），随选中模型切换重置。 */

import { useState } from "react";

import { Badge } from "../../components/ui/Badge";
import { CheckIcon, ChevronIcon } from "../../components/ui/icons";
import type { Strings } from "../../i18n/strings";
import type { PricingModelRow } from "../../lib/api";

interface VariantsSectionProps {
  readonly strings: Strings;
  readonly selected: PricingModelRow;
  /** 全部标准模型 id（改指向的下拉候选，排除当前归属）。 */
  readonly allIds: readonly string[];
  /** 未定价时不渲染改指向控件（先看，改指在挂价后做也来得及）。 */
  readonly isUnpriced: boolean;
  readonly onRepoint: (rawModel: string, modelId: string) => void;
}

export function VariantsSection({
  strings,
  selected,
  allIds,
  isUnpriced,
  onRepoint,
}: VariantsSectionProps) {
  const [pickerRaw, setPickerRaw] = useState<string | null>(null);
  /** 改指向的自定义目标态：目标模型可能已被合并删除，下拉里选不到。 */
  const [customRaw, setCustomRaw] = useState<string | null>(null);
  const [customValue, setCustomValue] = useState("");

  function applyCustomRepoint(rawModel: string): void {
    const target = customValue.trim();
    setCustomRaw(null);
    setCustomValue("");
    if (target !== "") onRepoint(rawModel, target);
  }

  return (
    <div className="border-b border-border px-4 py-3">
      <div className="flex items-center gap-2">
        <h3 className="text-sm font-medium text-ink-strong">
          {strings.pricingVariantsTitle}
        </h3>
        {selected.variants.length > 0 ? (
          <Badge pill>{selected.variants.length}</Badge>
        ) : null}
      </div>
      {selected.variants.length === 0 ? (
        <p className="mt-2 text-xs text-ink-muted">{strings.pricingNoVariants}</p>
      ) : (
        <ul className="mt-2 space-y-1.5">
          {selected.variants.map((variant) => (
            <li key={variant.rawModel} className="flex items-center gap-2">
              <span className="truncate font-mono text-xs text-ink">
                {variant.rawModel}
              </span>
              {!isUnpriced ? (
                customRaw === variant.rawModel ? (
                  <input
                    autoFocus
                    value={customValue}
                    aria-label={strings.pricingMoveTo}
                    placeholder={strings.pricingMoveToCustom}
                    onChange={(event) => setCustomValue(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter") {
                        applyCustomRepoint(variant.rawModel);
                      } else if (event.key === "Escape") {
                        setCustomRaw(null);
                        setCustomValue("");
                      }
                    }}
                    onBlur={() => {
                      setCustomRaw(null);
                      setCustomValue("");
                    }}
                    className="ml-auto h-6 w-44 shrink-0 rounded-control border border-border bg-surface px-1.5 font-mono text-xs text-ink placeholder:text-ink-muted focus:border-accent focus:outline-none"
                  />
                ) : pickerRaw === variant.rawModel ? (
                  <div className="relative ml-auto shrink-0">
                    <button
                      type="button"
                      aria-label={`${variant.rawModel} ${strings.pricingMoveTo}`}
                      aria-expanded="true"
                      onClick={() => setPickerRaw(null)}
                      className="flex h-6 items-center gap-1 rounded-control border border-accent bg-surface px-1.5 text-xs text-ink focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                    >
                      <span className="max-w-40 truncate font-mono">{selected.id}</span>
                      <ChevronIcon className="size-3 rotate-180 text-ink-muted" />
                    </button>
                    {/* 与"复制目录价"选单同款面板；当前归属只读（原生
                        select 点当前项不触发，这里显式禁点防自映射）。 */}
                    <ul className="absolute right-0 top-7 z-20 max-h-56 w-64 overflow-y-auto rounded-control border border-border bg-surface-raised py-1 shadow-raised">
                      <li
                        aria-disabled="true"
                        className="flex items-center gap-2 px-2.5 py-1.5 text-xs text-ink-muted"
                      >
                        <span className="min-w-0 flex-1 truncate font-mono">
                          {selected.id}
                        </span>
                        <CheckIcon className="size-3.5 shrink-0" />
                      </li>
                      {allIds
                        .filter((id) => id !== selected.id)
                        .map((id) => (
                          <li key={id}>
                            <button
                              type="button"
                              onClick={() => {
                                setPickerRaw(null);
                                onRepoint(variant.rawModel, id);
                              }}
                              className="w-full truncate px-2.5 py-1.5 text-left text-xs text-ink hover:bg-row-hover"
                            >
                              {`${strings.pricingMoveTo} ${id}`}
                            </button>
                          </li>
                        ))}
                      <li>
                        <button
                          type="button"
                          onClick={() => {
                            setPickerRaw(null);
                            setCustomRaw(variant.rawModel);
                            setCustomValue("");
                          }}
                          className="w-full truncate px-2.5 py-1.5 text-left text-xs text-ink hover:bg-row-hover"
                        >
                          {strings.pricingMoveToCustom}
                        </button>
                      </li>
                    </ul>
                    {/* 点外面收起：透明遮罩铺满视口，层级在面板之下。 */}
                    <button
                      type="button"
                      tabIndex={-1}
                      aria-hidden="true"
                      onClick={() => setPickerRaw(null)}
                      className="fixed inset-0 z-10 cursor-default"
                    />
                  </div>
                ) : (
                  <button
                    type="button"
                    aria-label={`${variant.rawModel} ${strings.pricingMoveTo}`}
                    aria-expanded="false"
                    onClick={() => setPickerRaw(variant.rawModel)}
                    className="ml-auto flex h-6 shrink-0 items-center gap-1 rounded-control border border-border bg-surface px-1.5 text-xs text-ink transition-colors hover:border-ink-muted/60 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
                  >
                    <span className="max-w-40 truncate font-mono">{selected.id}</span>
                    <ChevronIcon className="size-3 text-ink-muted" />
                  </button>
                )
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
