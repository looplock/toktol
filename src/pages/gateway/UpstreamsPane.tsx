/**
 * 上游分区：上游列表 + 编辑表单（增改删），保存经 IPC 直写 gateway.json。
 */

import { useState } from "react";
import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { EmptyState } from "../../components/ui/EmptyState";
import { Field } from "../../components/ui/Field";
import { Segmented } from "../../components/ui/Segmented";
import { Switch } from "../../components/ui/Switch";
import type { Strings } from "../../i18n/strings";
import {
  addUpstream,
  deleteUpstream,
  updateUpstream,
  type GatewayStatus,
  type UpstreamInput,
  type UpstreamView,
} from "../../lib/api";
import { formatCount } from "../../lib/format";
import { inputClass, PaneCard } from "./PaneCard";

interface UpstreamsPaneProps {
  readonly strings: Strings;
  readonly config: GatewayStatus["config"] | null;
  /** 保存/删除成功后回调（刷新状态快照）。 */
  readonly onChanged: () => void;
}

export function UpstreamsPane({ strings, config, onChanged }: UpstreamsPaneProps) {
  const upstreams = config?.kind === "valid" ? config.upstreams : [];
  const [selected, setSelected] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState<UpstreamInput | null>(null);
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const openEditor = (upstream: UpstreamView | null) => {
    setFormError(null);
    if (upstream === null) {
      setAdding(true);
      setSelected(null);
      setDraft({
        name: "",
        protocol: "openai",
        baseUrl: "",
        keyRef: "",
        enabled: true,
      });
    } else {
      setAdding(false);
      setSelected(upstream.name);
      setDraft({
        name: upstream.name,
        protocol: upstream.protocol,
        baseUrl: upstream.baseUrl,
        keyRef: "",
        enabled: upstream.enabled,
      });
    }
  };

  const closeEditor = () => {
    setAdding(false);
    setSelected(null);
    setDraft(null);
    setFormError(null);
  };

  const save = () => {
    if (draft === null) {
      return;
    }
    setBusy(true);
    setFormError(null);
    const action =
      adding || selected === null
        ? addUpstream(draft)
        : updateUpstream(selected, draft);
    action
      .then(() => {
        onChanged();
        closeEditor();
        setSelected(draft.name);
      })
      .catch((err: unknown) => setFormError(String(err)))
      .finally(() => setBusy(false));
  };

  const remove = () => {
    if (selected === null) {
      return;
    }
    setBusy(true);
    setFormError(null);
    deleteUpstream(selected)
      .then(() => {
        onChanged();
        closeEditor();
      })
      .catch((err: unknown) => setFormError(String(err)))
      .finally(() => setBusy(false));
  };

  const showForm = draft !== null;
  const editorTitle = adding ? strings.gatewayUpstreamAdd : selected;

  return (
    <PaneCard>
      <div className="grid grid-cols-[240px_minmax(0,1fr)]">
        <div className="border-r border-border pr-4">
          <div className="flex items-center justify-between gap-2">
            <span className="text-sm">
              {formatCount(upstreams.length)} {strings.gatewayTabUpstreams}
            </span>
            <Button variant="ghost" onClick={() => openEditor(null)}>
              <span className="flex items-center gap-1">
                <span aria-hidden="true">+</span>
                {strings.gatewayUpstreamAdd}
              </span>
            </Button>
          </div>
          {upstreams.length === 0 ? (
            <p className="py-6 text-center text-xs text-ink-muted">
              {strings.gatewayUpstreamEmptyList}
            </p>
          ) : (
            <ul className="mt-2 space-y-1">
              {upstreams.map((upstream) => (
                <li key={upstream.name}>
                  <button
                    type="button"
                    onClick={() => openEditor(upstream)}
                    className={`flex w-full items-center justify-between gap-2 rounded-control px-2.5 py-1.5 text-left text-sm transition-colors ${
                      selected === upstream.name && !adding
                        ? "bg-accent/10 text-accent"
                        : "text-ink-muted hover:bg-row-hover"
                    }`}
                  >
                    <span className="min-w-0 truncate">{upstream.name}</span>
                    <Badge pill>{upstream.protocol}</Badge>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="mt-5 md:mt-0 md:pl-5">
          {!showForm ? (
            <EmptyState>
              <p>{strings.gatewayUpstreamEmptyList}</p>
              <p className="mt-1 text-xs">
                {strings.gatewayUpstreamEmptyDetail}
              </p>
            </EmptyState>
          ) : (
            <div className="space-y-3">
              <h3 className="text-sm font-semibold">{editorTitle}</h3>
              <Field label={strings.gatewayUpstreamName}>
                <input
                  className={inputClass}
                  aria-label={strings.gatewayUpstreamName}
                  value={draft.name}
                  onChange={(event) =>
                    setDraft({ ...draft, name: event.target.value })
                  }
                />
              </Field>
              <Field label={strings.gatewayUpstreamProtocol}>
                <Segmented
                  label={strings.gatewayUpstreamProtocol}
                  options={[
                    { value: "openai" as const, label: "OpenAI" },
                    { value: "responses" as const, label: "Responses" },
                    { value: "anthropic" as const, label: "Anthropic" },
                    { value: "gemini" as const, label: "Gemini" },
                  ]}
                  value={draft.protocol}
                  onChange={(value) => setDraft({ ...draft, protocol: value })}
                />
              </Field>
              <Field label={strings.gatewayUpstreamBaseUrl}>
                <input
                  className={inputClass}
                  aria-label={strings.gatewayUpstreamBaseUrl}
                  placeholder="https://api.anthropic.com"
                  value={draft.baseUrl}
                  onChange={(event) =>
                    setDraft({ ...draft, baseUrl: event.target.value })
                  }
                />
              </Field>
              <Field
                label={strings.gatewayUpstreamKeyRef}
                hint={strings.gatewayUpstreamKeyRefHint}
              >
                <input
                  className={inputClass}
                  aria-label={strings.gatewayUpstreamKeyRef}
                  placeholder="ANTHROPIC_API_KEY"
                  value={draft.keyRef}
                  onChange={(event) =>
                    setDraft({ ...draft, keyRef: event.target.value })
                  }
                />
              </Field>
              <Switch
                label={strings.gatewayUpstreamEnabled}
                checked={draft.enabled}
                onChange={(checked) => setDraft({ ...draft, enabled: checked })}
              />
              {formError === null ? null : (
                <p className="text-xs text-danger">{formError}</p>
              )}
              <div className="flex items-center gap-2">
                <Button variant="primary" onClick={save} disabled={busy}>
                  {strings.gatewayUpstreamSave}
                </Button>
                {adding ? null : (
                  <Button variant="secondary" onClick={remove} disabled={busy}>
                    {strings.gatewayUpstreamDelete}
                  </Button>
                )}
                <Button variant="ghost" onClick={closeEditor} disabled={busy}>
                  {strings.gatewayUpstreamCancel}
                </Button>
              </div>
            </div>
          )}
        </div>
      </div>
    </PaneCard>
  );
}
