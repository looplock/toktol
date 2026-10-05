/**
 * 模型映射分区：请求模型名 → 上游模型名的映射表，增删即时写盘。
 */

import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { DataTable, type DataTableColumn } from "../../components/data/DataTable";
import { EmptyState } from "../../components/ui/EmptyState";
import { Field } from "../../components/ui/Field";
import type { Strings } from "../../i18n/strings";
import {
  addMapping,
  deleteMapping,
  type GatewayStatus,
} from "../../lib/api";
import { formatCount } from "../../lib/format";
import { inputClass, PaneCard } from "./PaneCard";

interface MappingsPaneProps {
  readonly strings: Strings;
  readonly config: GatewayStatus["config"] | null;
  readonly onChanged: () => void;
}

export function MappingsPane({ strings, config, onChanged }: MappingsPaneProps) {
  const mappings = config?.kind === "valid" ? config.mappings : [];
  const [adding, setAdding] = useState(false);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = () => {
    setBusy(true);
    setError(null);
    addMapping(from.trim(), to.trim())
      .then(() => {
        setFrom("");
        setTo("");
        setAdding(false);
        onChanged();
      })
      .catch((err: unknown) => setError(String(err)))
      .finally(() => setBusy(false));
  };

  const remove = (name: string) => {
    setBusy(true);
    setError(null);
    deleteMapping(name)
      .then(() => onChanged())
      .catch((err: unknown) => setError(String(err)))
      .finally(() => setBusy(false));
  };

  const columns: DataTableColumn<{ from: string; to: string }>[] = [
    {
      key: "from",
      header: strings.gatewayMappingFrom,
      render: (row) => <span className="font-mono text-xs">{row.from}</span>,
    },
    {
      key: "to",
      header: strings.gatewayMappingTo,
      render: (row) => <span className="font-mono text-xs">{row.to}</span>,
    },
    {
      key: "actions",
      header: "",
      render: (row) => (
        <Button
          variant="ghost"
          onClick={() => remove(row.from)}
          disabled={busy}
        >
          {strings.gatewayUpstreamDelete}
        </Button>
      ),
    },
  ];

  return (
    <PaneCard>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm">
            {formatCount(mappings.length)} {strings.gatewayTabMappings}
          </p>
          <p className="mt-0.5 text-xs text-ink-muted">
            {strings.gatewayMappingDesc}
          </p>
        </div>
        <Button variant="secondary" onClick={() => setAdding(!adding)}>
          {strings.gatewayMappingAdd}
        </Button>
      </div>

      {error === null ? null : (
        <p className="mt-3 text-xs text-danger">{error}</p>
      )}

      {adding ? (
        <div className="mt-4 grid items-end gap-3 sm:grid-cols-[1fr_1fr_auto]">
          <Field label={strings.gatewayMappingFrom}>
            <input
              className={inputClass}
              value={from}
              onChange={(event) => setFrom(event.target.value)}
            />
          </Field>
          <Field label={strings.gatewayMappingTo}>
            <input
              className={inputClass}
              value={to}
              onChange={(event) => setTo(event.target.value)}
            />
          </Field>
          <div className="flex items-center gap-2 pb-0.5">
            <Button
              variant="primary"
              onClick={submit}
              disabled={busy || from.trim() === "" || to.trim() === ""}
            >
              {strings.gatewayMappingConfirm}
            </Button>
            <Button variant="ghost" onClick={() => setAdding(false)}>
              {strings.gatewayUpstreamCancel}
            </Button>
          </div>
        </div>
      ) : null}

      <div className="mt-4">
        <DataTable
          columns={columns}
          rows={mappings}
          rowKey={(row) => row.from}
          empty={<EmptyState>{strings.gatewayMappingEmptyTitle}</EmptyState>}
        />
        {mappings.length === 0 ? (
          <p className="mt-2 text-center text-xs text-ink-muted">
            {strings.gatewayMappingEmptyDetail}
          </p>
        ) : null}
      </div>
    </PaneCard>
  );
}
