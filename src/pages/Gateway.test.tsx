import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import type { GatewayOverviewPayload, GatewayStatus } from "../lib/api";
import { GatewayPage } from "./Gateway";

// 用 react-dom/server 而不是 jsdom：只验"渲染出什么"。每个用例经 initial* 缝隙
// 注入状态，覆盖左侧导航、入口页两条主分支、上游/映射/流量的空态与数据态。

const strings = stringsFor();

function statusFixture(running: boolean): GatewayStatus {
  return {
    running,
    listen: running ? "127.0.0.1:8412" : null,
    configPath: "C:\\Users\\demo\\.toktol\\gateway.json",
    config: {
      kind: "valid",
      listen: "127.0.0.1:8412",
      upstreams: [
        {
          name: "anthropic",
          protocol: "anthropic",
          baseUrl: "https://api.anthropic.com",
          enabled: true,
        },
      ],
      routes: [{ pattern: "claude-*", upstream: "anthropic" }],
      mappings: [{ from: "gpt-x", to: "claude-sonnet-5" }],
      models: ["claude-sonnet-5"],
      tokenCount: 2,
    },
  };
}

const OVERVIEW: GatewayOverviewPayload = {
  totals: {
    inputTokens: 0,
    outputTokens: 0,
    cacheReadTokens: 0,
    cacheWriteTokens: 0,
    reasoningTokens: 0,
    requestCount: 0,
    errorCount: 0,
    knownCostMicros: 0,
    unknownCostRows: 0,
  },
  byModel: [],
};

it("左侧导航列出四个分区并带元信息", () => {
  const html = renderToStaticMarkup(
    <GatewayPage
      strings={strings}
      initialStatus={statusFixture(false)}
      initialOverview={OVERVIEW}
    />,
  );

  expect(html).toContain(strings.gatewayConsoleTitle);
  expect(html).toContain(strings.gatewayTabEntrance);
  expect(html).toContain(strings.gatewayTabUpstreams);
  expect(html).toContain(strings.gatewayTabMappings);
  expect(html).toContain(strings.gatewayTabTraffic);
  // 入口 meta 是运行状态；上游 meta 是 x/y 已启用。
  expect(html).toContain(strings.gatewayStopped);
  expect(html).toContain(`1/1 ${strings.gatewayEnabledShort}`);
});

it("入口页停止态：启动按钮、接口地址、令牌占位与环境变量示例", () => {
  const html = renderToStaticMarkup(
    <GatewayPage
      strings={strings}
      initialStatus={statusFixture(false)}
      initialOverview={OVERVIEW}
    />,
  );

  expect(html).toContain(strings.gatewayServiceStoppedHint);
  expect(html).toContain(strings.gatewayStart);
  expect(html).not.toContain(strings.gatewayStop);
  expect(html).toContain("http://127.0.0.1:8412");
  expect(html).toContain(strings.gatewayTokenNone);
  // 四个协议都在分段控件里。
  expect(html).toContain(strings.gatewayProtocolOpenAI);
  expect(html).toContain(strings.gatewayProtocolResponses);
  expect(html).toContain(strings.gatewayProtocolGemini);
  // 四张指标卡。
  expect(html).toContain(strings.gatewayStatRequests);
  expect(html).toContain(strings.gatewayStatErrors);
  expect(html).toContain(strings.gatewayStatEnabledUpstreams);
  expect(html).toContain(strings.gatewayStatModels);
});

it("入口页运行态：停止按钮与运行提示", () => {
  const html = renderToStaticMarkup(
    <GatewayPage
      strings={strings}
      initialStatus={statusFixture(true)}
      initialOverview={OVERVIEW}
    />,
  );

  expect(html).toContain(strings.gatewayRunning);
  expect(html).toContain(strings.gatewayServiceRunningHint);
  expect(html).toContain(strings.gatewayStop);
});

it("上游分区：空配置给空态，有数据时列出上游", () => {
  const html = renderToStaticMarkup(
    <GatewayPage
      strings={strings}
      initialStatus={statusFixture(false)}
      initialPane="upstreams"
    />,
  );
  expect(html).toContain(strings.gatewayUpstreamEmptyList);
  expect(html).toContain(strings.gatewayUpstreamEmptyDetail);

  const html2 = renderToStaticMarkup(
    <GatewayPage strings={strings} initialStatus={statusFixture(true)} initialPane="upstreams" />,
  );
  expect(html2).toContain("anthropic");
  expect(html2).toContain(strings.gatewayUpstreamAdd);
});

it("映射分区：空态提示与映射行", () => {
  const emptyConfig = {
    ...statusFixture(false),
    config: {
      ...statusFixture(false).config,
      mappings: [],
      models: [],
      upstreams: [],
      routes: [],
    },
  };
  const html = renderToStaticMarkup(
    <GatewayPage strings={strings} initialStatus={emptyConfig} initialPane="mappings" />,
  );
  expect(html).toContain(strings.gatewayMappingEmptyTitle);
  expect(html).toContain(strings.gatewayMappingEmptyDetail);

  const html2 = renderToStaticMarkup(
    <GatewayPage strings={strings} initialStatus={statusFixture(true)} initialPane="mappings" />,
  );
  expect(html2).toContain("gpt-x");
  expect(html2).toContain("claude-sonnet-5");
});

it("流量分区：空态文案", () => {
  const html = renderToStaticMarkup(
    <GatewayPage
      strings={strings}
      initialStatus={statusFixture(false)}
      initialOverview={OVERVIEW}
      initialPane="traffic"
    />,
  );
  expect(html).toContain(strings.gatewayTrafficEmptyTitle);
  expect(html).toContain(strings.gatewayTrafficEmptyDetail);
  // 流量行数据由 effect 拉取，SSR 首帧只渲染空态。
  expect(html).not.toContain("<table");
});
