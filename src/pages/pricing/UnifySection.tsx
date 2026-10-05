/** 并入其他模型：改名（目录规范名，自动补价）与合并（本地模型）的统一入口。
 * 开合与搜索是本组件的内部状态；候选随输入过滤，Enter 应用首条。 */

import { useMemo, useState } from "react";

import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import type { Strings } from "../../i18n/strings";
import type { CatalogModelBrief, PricingModelRow } from "../../lib/api";
import type { UnifyCandidate } from "./price";

interface UnifySectionProps {
  readonly strings: Strings;
  /** 当前选中的标准模型（并入候选排除自己）。 */
  readonly selectedId: string;
  readonly models: readonly PricingModelRow[];
  readonly catalogModels: readonly CatalogModelBrief[];
  /** 选中即执行（页内跑 IPC + 重拉 + 气泡），本组件只管收起选单。 */
  readonly onUnify: (target: UnifyCandidate) => void;
}

export function UnifySection({
  strings,
  selectedId,
  models,
  catalogModels,
  onUnify,
}: UnifySectionProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");

  /** 并入候选：本地模型在先、目录规范名在后，按输入过滤；同名去重、排除自己。 */
  const candidates = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const out: UnifyCandidate[] = [];
    const seen = new Set<string>([selectedId]);
    for (const m of models) {
      if (seen.has(m.id)) continue;
      if (needle !== "" && !m.id.toLowerCase().includes(needle)) continue;
      seen.add(m.id);
      out.push({ id: m.id, provider: null, status: null, kind: "local" });
    }
    for (const c of catalogModels) {
      if (seen.has(c.modelId)) continue;
      if (needle !== "" && !c.modelId.toLowerCase().includes(needle)) continue;
      seen.add(c.modelId);
      out.push({ id: c.modelId, provider: c.provider, status: c.status, kind: "catalog" });
    }
    return out;
  }, [models, catalogModels, query, selectedId]);

  function dismiss(): void {
    setOpen(false);
    setQuery("");
  }

  function run(candidate: UnifyCandidate): void {
    onUnify(candidate);
    dismiss();
  }

  return (
    <div className="px-4 py-3">
      {open ? (
        <div>
          <div className="flex items-center gap-2">
            <input
              autoFocus
              value={query}
              aria-label={strings.pricingUnifySearch}
              placeholder={strings.pricingUnifySearch}
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") {
                  dismiss();
                } else if (event.key === "Enter") {
                  const first = candidates[0];
                  if (first !== undefined) run(first);
                }
              }}
              className="h-8 w-full max-w-md rounded-control border border-border bg-surface px-2 font-mono text-sm text-ink placeholder:text-ink-muted focus:border-accent focus:outline-none"
            />
            <Button variant="ghost" onClick={dismiss}>
              {strings.pricingCancel}
            </Button>
          </div>
          <ul className="mt-1.5 max-h-48 w-full max-w-md overflow-y-auto rounded-control border border-border bg-surface-raised py-1 shadow-raised">
            {candidates.map((candidate) => (
              <li key={candidate.id}>
                <button
                  type="button"
                  onClick={() => run(candidate)}
                  className="flex w-full items-center gap-2 px-2.5 py-1.5 text-left hover:bg-row-hover"
                >
                  <span className="truncate font-mono text-xs text-ink-strong">
                    {candidate.id}
                  </span>
                  {candidate.status === "deprecated" ? (
                    <Badge tone="unknown">{strings.pricingDeprecated}</Badge>
                  ) : null}
                  <span className="ml-auto shrink-0 text-xs text-ink-muted">
                    {candidate.kind === "local" ? strings.pricingLocal : candidate.provider}
                  </span>
                  <Badge tone="neutral">
                    {candidate.kind === "local"
                      ? strings.pricingMerge
                      : strings.pricingSourceCatalog}
                  </Badge>
                </button>
              </li>
            ))}
            {candidates.length === 0 ? (
              <li className="px-2.5 py-2 text-xs text-ink-muted">
                {catalogModels.length === 0
                  ? strings.pricingRenameSyncFirst
                  : strings.pricingNoMatch}
              </li>
            ) : null}
          </ul>
        </div>
      ) : (
        <button
          type="button"
          onClick={() => setOpen(true)}
          className="text-xs text-ink-muted transition duration-150 ease-out hover:text-ink-strong"
        >
          {strings.pricingUnifyLink}
        </button>
      )}
    </div>
  );
}
