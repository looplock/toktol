/**
 * 网关控制台：左侧四分区导航（入口 / 上游 / 模型映射 / 流量）+ 右侧内容面板。
 * 服务状态每 3 秒轮询，只在页面挂载期间进行；服务本身不随页面卸载。
 * 配置（上游/映射/令牌哈希）经 IPC 直写 gateway.json——写入前 Rust 侧整份校验，
 * 手改文件仍兼容（mtime 热重载）。访问令牌明文只在本会话内存里存在。
 * 四个分区与导航拆在 gateway/ 子目录（ConsoleSidebar / *Pane）。
 */

import { useCallback, useEffect, useState } from "react";
import { PageShell } from "../components/PageShell";
import type { Strings } from "../i18n/strings";
import {
  fetchGatewayOverview,
  fetchGatewayStatus,
  startGateway,
  stopGateway,
  type GatewayOverviewPayload,
  type GatewayStatus,
} from "../lib/api";
import { formatCount } from "../lib/format";
import { ConsoleSidebar, type GatewayPane } from "./gateway/ConsoleSidebar";
import { EntrancePane } from "./gateway/EntrancePane";
import { MappingsPane } from "./gateway/MappingsPane";
import { TrafficPane } from "./gateway/TrafficPane";
import { UpstreamsPane } from "./gateway/UpstreamsPane";

const STATUS_POLL_MS = 3_000;

interface GatewayPageProps {
  readonly strings: Strings;
  /** 测试缝隙：注入首帧状态，跳过"等 effect 发起 IPC"的过程；浏览器里照常轮询。 */
  readonly initialStatus?: GatewayStatus;
  readonly initialOverview?: GatewayOverviewPayload;
  /** 测试缝隙：直接渲染某个分区（默认入口）。 */
  readonly initialPane?: GatewayPane;
}

export function GatewayPage({
  strings,
  initialStatus,
  initialOverview,
  initialPane,
}: GatewayPageProps) {
  const [pane, setPane] = useState<GatewayPane>(initialPane ?? "entrance");

  const [status, setStatus] = useState<GatewayStatus | null>(
    initialStatus ?? null,
  );
  const [statusError, setStatusError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const [overview, setOverview] = useState<GatewayOverviewPayload | null>(
    initialOverview ?? null,
  );
  const [overviewError, setOverviewError] = useState<string | null>(null);

  const loadStatus = useCallback(() => {
    fetchGatewayStatus()
      .then((next) => {
        setStatus(next);
        setStatusError(null);
      })
      .catch((err: unknown) => setStatusError(String(err)));
  }, []);

  const loadOverview = useCallback(() => {
    fetchGatewayOverview()
      .then((next) => {
        setOverview(next);
        setOverviewError(null);
      })
      .catch((err: unknown) => setOverviewError(String(err)));
  }, []);

  useEffect(() => {
    loadStatus();
    loadOverview();
    const timer = window.setInterval(loadStatus, STATUS_POLL_MS);
    return () => window.clearInterval(timer);
  }, [loadStatus, loadOverview]);

  const config = status?.config ?? null;
  const running = status?.running ?? false;
  const listen =
    status?.listen ?? (config?.kind === "valid" ? config.listen : null);
  const enabledUpstreams =
    config?.kind === "valid"
      ? config.upstreams.filter((upstream) => upstream.enabled).length
      : 0;

  const toggle = (checked: boolean) => {
    setPending(true);
    setActionError(null);
    (checked ? startGateway() : stopGateway())
      .then((next) => {
        setStatus(next);
        setStatusError(null);
        if (checked) {
          loadOverview();
        }
      })
      .catch((err: unknown) =>
        setActionError(
          checked
            ? `${strings.gatewayStartError}: ${String(err)}`
            : `${strings.gatewayStopError}: ${String(err)}`,
        ),
      )
      .finally(() => setPending(false));
  };

  return (
    <PageShell fill>
      <div className="grid min-h-0 flex-1 grid-cols-[230px_minmax(0,1fr)] gap-4">
        <ConsoleSidebar
          strings={strings}
          pane={pane}
          onPaneChange={setPane}
          running={running}
          upstreamMeta={
            config?.kind === "valid"
              ? `${formatCount(enabledUpstreams)}/${formatCount(config.upstreams.length)} ${
                  strings.gatewayEnabledShort
                }`
              : "—"
          }
          mappingMeta={
            config?.kind === "valid" ? formatCount(config.mappings.length) : "—"
          }
          trafficMeta={
            overview === null ? "—" : formatCount(overview.totals.requestCount)
          }
        />

        {pane === "entrance" ? (
          <EntrancePane
            strings={strings}
            running={running}
            pending={pending}
            listen={listen}
            status={status}
            config={config}
            overview={overview}
            statusError={statusError}
            actionError={actionError}
            enabledUpstreams={enabledUpstreams}
            onToggle={toggle}
          />
        ) : null}

        {pane === "upstreams" ? (
          <UpstreamsPane
            strings={strings}
            config={config}
            onChanged={loadStatus}
          />
        ) : null}

        {pane === "mappings" ? (
          <MappingsPane
            strings={strings}
            config={config}
            onChanged={loadStatus}
          />
        ) : null}

        {pane === "traffic" ? (
          <TrafficPane strings={strings} overviewError={overviewError} />
        ) : null}
      </div>
    </PageShell>
  );
}
