/**
 * 网关控制台：左侧四分区导航（入口 / 上游 / 模型映射 / 流量）+ 右侧内容面板。
 * 服务状态每 3 秒轮询，只在页面挂载期间进行；服务本身不随页面卸载。
 * 配置（上游/映射/令牌哈希）经 IPC 直写 gateway.json——写入前 Rust 侧整份校验，
 * 手改文件仍兼容（mtime 热重载）。访问令牌明文只在本会话内存里存在。
 * 四个分区与导航拆在 gateway/ 子目录（ConsoleSidebar / *Pane）。
 */

import { useState } from "react";
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
import { useInvokeQuery } from "../lib/hooks/useInvokeQuery";
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

  // 状态 3 秒轮询（只在页面挂载期间进行）；流量概览按需重拉。启停操作
  // 的返回值经 mutate 即时落状态（等下一轮轮询会有可感的延迟），操作错
  // 走 actionError——与加载错分家。
  const statusQuery = useInvokeQuery({
    deps: ["gateway-status"],
    fetch: fetchGatewayStatus,
    ...(initialStatus === undefined ? {} : { initialData: initialStatus }),
    pollMs: STATUS_POLL_MS,
  });
  const status = statusQuery.data;
  const statusError = statusQuery.error;
  const applyStatus = statusQuery.mutate;
  const loadStatus = statusQuery.reload;

  const overviewQuery = useInvokeQuery({
    deps: ["gateway-overview"],
    fetch: fetchGatewayOverview,
    ...(initialOverview === undefined ? {} : { initialData: initialOverview }),
  });
  const overview = overviewQuery.data;
  const overviewError = overviewQuery.error;
  const loadOverview = overviewQuery.reload;

  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const toggle = (checked: boolean) => {
    setPending(true);
    setActionError(null);
    (checked ? startGateway() : stopGateway())
      .then((next) => {
        applyStatus(next);
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

  const config = status?.config ?? null;
  const running = status?.running ?? false;
  const listen =
    status?.listen ?? (config?.kind === "valid" ? config.listen : null);
  const enabledUpstreams =
    config?.kind === "valid"
      ? config.upstreams.filter((upstream) => upstream.enabled).length
      : 0;

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
