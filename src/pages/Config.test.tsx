import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import type { ToolConfigReport } from "../lib/api";
import { ConfigPage } from "./Config";

// 用 react-dom/server 而不是 jsdom：只验"渲染出什么"。报告经 initial 缝隙
// 注入（SSR 不跑 effect，IPC 拉取自然被跳过），覆盖左列表（工具选中态）、
// MCP 页签（计数行、条目徽章、打码后的 URL/命令行）与不支持态。

const strings = stringsFor();

function reportFixture(overrides: Partial<ToolConfigReport> = {}): ToolConfigReport {
  return {
    tool: "claude-code",
    homeLabel: "~/.claude",
    supported: true,
    mcp: [
      {
        name: "tavily",
        transport: "http",
        scope: "user",
        command: null,
        url: "https://mcp.tavily.com/mcp/?tavilyApiKey=***",
        envKeys: [],
        project: null,
      },
      {
        name: "fs",
        transport: "stdio",
        scope: "user",
        command: "npx -y mcp-server-fs",
        url: null,
        envKeys: ["FS_ALLOWED"],
        project: null,
      },
    ],
    skills: [
      {
        name: "review",
        scope: "user",
        description: "Review code",
        path: "~/.claude/skills/review",
        root: "home",
        rel: "skills/review/SKILL.md",
      },
      {
        name: "commit",
        scope: "plugin",
        description: "Write commits",
        path: "~/.claude/plugins/commit",
        root: "home",
        rel: "",
      },
    ],
    roots: [{ key: "home", label: "~/.claude", entries: [] }],
    ...overrides,
  };
}

it("配置页 MCP 页签：侧栏工具列表、计数行与打码后的元数据", () => {
  const html = renderToStaticMarkup(
    <ConfigPage strings={strings} initial={reportFixture()} />,
  );
  expect(html).toContain(strings.configSidebarTitle);
  expect(html).toContain(strings.navConfig);
  // 左列表：全工具可见，选中工具加粗出现。
  expect(html).toContain("Claude Code");
  expect(html).toContain("Codex");
  // 计数行与条目：URL 已在 Rust 侧打码，前端原样展示。
  expect(html).toContain(strings.configMcpCount.replace("{count}", "2"));
  expect(html).toContain("tavily");
  expect(html).toContain("https://mcp.tavily.com/mcp/?tavilyApiKey=***");
  expect(html).toContain("npx -y mcp-server-fs");
  expect(html).toContain(`${strings.configEnvKeys}: FS_ALLOWED`);
  expect(html).toContain(strings.configScopeUser);
});

it("配置页空态与不支持态：未装工具显示不支持而不是空白", () => {
  const empty = renderToStaticMarkup(
    <ConfigPage
      strings={strings}
      initial={reportFixture({ mcp: [], skills: [] })}
    />,
  );
  expect(empty).toContain(strings.configEmptyMcpTitle);
  expect(empty).toContain(strings.configEmptyMcpDetail);

  const unsupported = renderToStaticMarkup(
    <ConfigPage
      strings={strings}
      initial={reportFixture({ supported: false })}
    />,
  );
  expect(unsupported).toContain(strings.configUnsupported);
});

it("配置页 Skills 页签：主从双栏，缺省选中首项并常驻展示详情", () => {
  const html = renderToStaticMarkup(
    <ConfigPage strings={strings} initial={reportFixture()} initialTab="skills" />,
  );
  expect(html).toContain(strings.configSkillCount.replace("{count}", "2"));
  // 缺省选中首项：左列表两个技能都在，详情只渲染首项（名称 + 描述 + 路径）。
  expect(html).toContain("review");
  expect(html).toContain("commit");
  expect(html).toContain("Review code");
  expect(html).toContain("~/.claude/skills/review");
  expect(html).toContain(strings.configSkillOpenFiles);
  // rel 为空的技能没有可回读内容——但首项 rel 非空，详情不该出现拒读文案。
  expect(html).not.toContain(strings.configContentDenied);
});
