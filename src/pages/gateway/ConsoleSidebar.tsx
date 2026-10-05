/**
 * 网关控制台的左侧四分区导航；pane id 类型（GatewayPane）也住这里，
 * 页面壳与导航共用。
 */

import type { JSX } from "react";
import { Card } from "../../components/ui/Card";
import {
  EntranceIcon,
  MappingIcon,
  TrafficIcon,
  UpstreamIcon,
} from "../../components/ui/icons";
import type { Strings } from "../../i18n/strings";

export type GatewayPane = "entrance" | "upstreams" | "mappings" | "traffic";

interface SidebarProps {
  readonly strings: Strings;
  readonly pane: GatewayPane;
  readonly onPaneChange: (pane: GatewayPane) => void;
  readonly running: boolean;
  readonly upstreamMeta: string;
  readonly mappingMeta: string;
  readonly trafficMeta: string;
}

export function ConsoleSidebar({
  strings,
  pane,
  onPaneChange,
  running,
  upstreamMeta,
  mappingMeta,
  trafficMeta,
}: SidebarProps) {
  const items: {
    readonly id: GatewayPane;
    readonly label: string;
    readonly meta: string;
    readonly icon: (props: { className?: string }) => JSX.Element;
  }[] = [
    {
      id: "entrance",
      label: strings.gatewayTabEntrance,
      meta: running ? strings.gatewayRunning : strings.gatewayStopped,
      icon: EntranceIcon,
    },
    {
      id: "upstreams",
      label: strings.gatewayTabUpstreams,
      meta: upstreamMeta,
      icon: UpstreamIcon,
    },
    {
      id: "mappings",
      label: strings.gatewayTabMappings,
      meta: mappingMeta,
      icon: MappingIcon,
    },
    {
      id: "traffic",
      label: strings.gatewayTabTraffic,
      meta: trafficMeta,
      icon: TrafficIcon,
    },
  ];

  return (
    <Card
      as="nav"
      aria-label={strings.gatewayConsoleTitle}
      raised
      padding="p-2"
      className="self-stretch"
    >
      <p className="px-3 py-2 text-sm font-semibold">
        {strings.gatewayConsoleTitle}
      </p>
      {/* 项间留 1 档空隙：相邻项的高亮/悬浮底色不能连成一片。 */}
      <ul className="space-y-1">
        {items.map((item) => {
          const active = pane === item.id;
          const Icon = item.icon;
          return (
            <li key={item.id}>
              <button
                type="button"
                onClick={() => onPaneChange(item.id)}
                aria-current={active ? "page" : undefined}
                className={`flex w-full items-center gap-2 rounded-control px-3 py-2 text-sm transition-colors ${
                  active
                    ? "bg-accent/10 text-ink"
                    : "text-ink-muted hover:bg-accent/5"
                }`}
              >
                <Icon
                  className={`size-4 shrink-0 ${active ? "text-accent" : ""}`}
                />
                <span className="min-w-0 flex-1 text-left">{item.label}</span>
                <span className="shrink-0 text-xs text-ink-muted">
                  {item.meta}
                </span>
              </button>
            </li>
          );
        })}
      </ul>
    </Card>
  );
}
