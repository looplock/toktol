/**
 * 入口分区：服务启停、接入端点与访问令牌、协议说明、总量统计。
 */

import { useState } from "react";
import { Button } from "../../components/ui/Button";
import { Field } from "../../components/ui/Field";
import { Segmented } from "../../components/ui/Segmented";
import { StatCard } from "../../components/ui/StatCard";
import {
  CopyIcon,
  EyeIcon,
  PlayIcon,
  ResetIcon,
  StopIcon,
} from "../../components/ui/icons";
import type { Strings } from "../../i18n/strings";
import {
  issueGatewayToken,
  type GatewayOverviewPayload,
  type GatewayStatus,
} from "../../lib/api";
import { formatCount } from "../../lib/format";
import { PaneCard } from "./PaneCard";

/** 四种受支持接入协议的 UI 标识；"responses" 是 OpenAI Responses SDK。 */
type CompatProtocol = "openai" | "responses" | "anthropic" | "gemini";

/** 只读值框（接口地址 / 令牌）：mono、弱化底色。 */
// 只读值框（接口地址 / 令牌）：静态内嵌底用 surface-subtle（比 muted 浅、
// 比 surface 深）；muted 语义是悬浮/激活着色。
const readOnlyClass =
  "w-full rounded-control bg-surface-subtle px-2.5 py-1.5 font-mono text-sm";

interface EntranceProps {
  readonly strings: Strings;
  readonly running: boolean;
  readonly pending: boolean;
  readonly listen: string | null;
  readonly status: GatewayStatus | null;
  readonly config: GatewayStatus["config"] | null;
  readonly overview: GatewayOverviewPayload | null;
  readonly statusError: string | null;
  readonly actionError: string | null;
  readonly enabledUpstreams: number;
  readonly onToggle: (checked: boolean) => void;
}

export function EntrancePane({
  strings,
  running,
  pending,
  listen,
  status,
  config,
  overview,
  statusError,
  actionError,
  enabledUpstreams,
  onToggle,
}: EntranceProps) {
  const [issuedToken, setIssuedToken] = useState<string | null>(null);
  const [revealed, setRevealed] = useState(false);
  const [issuing, setIssuing] = useState(false);
  const [issueError, setIssueError] = useState<string | null>(null);
  const [protocol, setProtocol] = useState<CompatProtocol>("anthropic");

  const copy = (text: string) => {
    navigator.clipboard.writeText(text).catch(() => {});
  };

  const issue = () => {
    setIssuing(true);
    setIssueError(null);
    issueGatewayToken()
      .then((token) => {
        setIssuedToken(token);
        // 新令牌默认打码：明文只在用户点眼睛时可见，降低截屏/旁观泄露面。
        setRevealed(false);
      })
      .catch((err: unknown) => setIssueError(String(err)))
      .finally(() => setIssuing(false));
  };

  const endpoint = listen ?? "127.0.0.1:8412";
  const protocolHint =
    protocol === "anthropic"
      ? strings.gatewayProtocolAnthropicHint
      : protocol === "responses"
        ? strings.gatewayProtocolResponsesHint
        : protocol === "gemini"
          ? strings.gatewayProtocolGeminiHint
          : strings.gatewayProtocolOpenAIHint;

  const protocolOptions = [
    { value: "openai" as const, label: strings.gatewayProtocolOpenAI },
    { value: "responses" as const, label: strings.gatewayProtocolResponses },
    { value: "anthropic" as const, label: strings.gatewayProtocolAnthropic },
    { value: "gemini" as const, label: strings.gatewayProtocolGemini },
  ];

  return (
    <PaneCard>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <span
            aria-hidden="true"
            className={`size-2 rounded-full ${running ? "bg-ok" : "bg-danger"}`}
          />
          <h2 className="text-sm font-semibold">
            {strings.gatewayServiceTitle}
          </h2>
          <span className="text-xs text-ink-muted">
            {running ? strings.gatewayRunning : strings.gatewayStopped}
          </span>
        </div>
        <Button
          variant={running ? "secondary" : "primary"}
          onClick={() => onToggle(!running)}
          disabled={pending}
        >
          {running ? (
            <StopIcon className="size-3.5" />
          ) : (
            <PlayIcon className="size-3.5" />
          )}
          {running ? strings.gatewayStop : strings.gatewayStart}
        </Button>
      </div>

      <p className="mt-2 text-sm">
        {running
          ? strings.gatewayServiceRunningHint
          : strings.gatewayServiceStoppedHint}
      </p>

      {statusError === null ? null : (
        <p className="mt-2 text-xs text-danger">{statusError}</p>
      )}
      {actionError === null ? null : (
        <p className="mt-2 text-xs text-danger">{actionError}</p>
      )}

      <div className="mt-4">
        {/* 接入卡组：地址与令牌并排——复制 Base URL 和复制令牌是配套操作，
            落在同一水平线；协议切换是接入的第三要素，并入同一张卡。
            容器只用现有 border/rounding 令牌，不引入新底色。 */}
        <div className="rounded-card border border-border p-4">
          <p className="mb-3 text-sm font-semibold">
            {strings.gatewayConnectionTitle}
          </p>
          <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
            <Field
              label={strings.gatewayEndpoint}
              hint={strings.gatewayEndpointHint}
            >
              <div className="flex items-center gap-2">
                <span className={`flex-1 ${readOnlyClass}`}>http://{endpoint}</span>
                <Button
                  variant="ghost"
                  onClick={() => copy(`http://${endpoint}`)}
                  aria-label={strings.gatewayCopied}
                >
                  <CopyIcon className="size-4" />
                </Button>
              </div>
            </Field>

            <Field
              label={strings.gatewayTokenField}
              hint={strings.gatewayTokenResetHint}
            >
              <div className="flex items-center gap-2">
                <span className={`flex-1 truncate ${readOnlyClass}`}>
                  {issuedToken === null
                    ? strings.gatewayTokenNone
                    : revealed
                      ? issuedToken
                      : strings.gatewayTokenMasked}
                </span>
                {issuedToken === null ? null : (
                  <Button
                    variant="ghost"
                    onClick={() => setRevealed(!revealed)}
                    aria-label={strings.gatewayTokenField}
                  >
                    <EyeIcon className="size-4" />
                  </Button>
                )}
                {issuedToken === null ? null : (
                  <Button
                    variant="ghost"
                    onClick={() => copy(issuedToken)}
                    aria-label={strings.gatewayCopied}
                  >
                    <CopyIcon className="size-4" />
                  </Button>
                )}
                <Button
                  variant="ghost"
                  onClick={issue}
                  disabled={issuing}
                  aria-label={strings.gatewayTokenReset}
                >
                  <ResetIcon className="size-4" />
                </Button>
              </div>
            </Field>
          </div>
          {issueError === null ? null : (
            <p className="mt-2 text-xs text-danger">{issueError}</p>
          )}

          <div className="mt-4">
            <p className="mb-1 text-sm">{strings.gatewayProtocols}</p>
            <Segmented
              label={strings.gatewayProtocols}
              options={protocolOptions}
              value={protocol}
              onChange={setProtocol}
            />
            <p className="mt-1 text-xs text-ink-muted">{protocolHint}</p>
          </div>
        </div>

        {/* 统计下沉为页脚区；配置文件路径随后。 */}
        <div className="mt-4 grid grid-cols-4 gap-3">
          <StatCard
            label={strings.gatewayStatRequests}
            value={
              overview === null
                ? "—"
                : formatCount(overview.totals.requestCount)
            }
          />
          <StatCard
            label={strings.gatewayStatErrors}
            value={
              overview === null ? "—" : formatCount(overview.totals.errorCount)
            }
          />
          <StatCard
            label={strings.gatewayStatEnabledUpstreams}
            value={formatCount(enabledUpstreams)}
          />
          <StatCard
            label={strings.gatewayStatModels}
            value={formatCount(
              config?.kind === "valid" ? config.models.length : 0,
            )}
          />
        </div>

        {config?.kind === "valid" && status?.configPath ? (
          <p className="mt-2 break-all text-xs text-ink-muted">
            {strings.gatewayConfigPath}: {status.configPath}
          </p>
        ) : null}
      </div>
    </PaneCard>
  );
}
