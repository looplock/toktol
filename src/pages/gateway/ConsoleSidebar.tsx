/**
 * 网关控制台的左侧四分区导航；pane id 类型（GatewayPane）也住这里，
 * 页面壳与导航共用。布局与样式统一走 SideNav，这里只提供数据。
 */

import {
  EntranceIcon,
  MappingIcon,
  TrafficIcon,
  UpstreamIcon,
} from "../../components/ui/icons";
import { SideNav } from "../../components/ui/SideNav";
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
  return (
    <SideNav
      label={strings.gatewayConsoleTitle}
      title={strings.gatewayConsoleTitle}
      active={pane}
      onChange={onPaneChange}
      items={[
        {
          id: "entrance",
          label: strings.gatewayTabEntrance,
          meta: running ? strings.gatewayRunning : strings.gatewayStopped,
          icon: <EntranceIcon className="size-4" />,
        },
        {
          id: "upstreams",
          label: strings.gatewayTabUpstreams,
          meta: upstreamMeta,
          icon: <UpstreamIcon className="size-4" />,
        },
        {
          id: "mappings",
          label: strings.gatewayTabMappings,
          meta: mappingMeta,
          icon: <MappingIcon className="size-4" />,
        },
        {
          id: "traffic",
          label: strings.gatewayTabTraffic,
          meta: trafficMeta,
          icon: <TrafficIcon className="size-4" />,
        },
      ]}
    />
  );
}
