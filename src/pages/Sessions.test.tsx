import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../i18n/strings";
import type { SessionRow, SessionsPagePayload, TranscriptEntry, TranscriptTurn } from "../lib/api";
import { formatCostMicros } from "../lib/format";
import { SessionsPage } from "./Sessions";
import { SessionDetailView } from "./sessions/SessionDetailView";
import { TurnRuler } from "./sessions/TurnRuler";
import { UserTurnBubble } from "./sessions/TranscriptStream";
import { buildOriginalTree, type OriginalTree } from "../lib/transcriptOriginal";

// 用 react-dom/server 而不是 jsdom：只验"渲染出什么"。数据经 initial* 缝隙
// 注入（SSR 不跑 effect，IPC 拉取自然被跳过），覆盖列表空态/数据态与
// 详情视图的气泡渲染。

const strings = stringsFor();

function rowFixture(overrides: Partial<SessionRow> = {}): SessionRow {
  return {
    id: 1,
    tool: "claude-code",
    externalId: "3bfaeb77",
    title: "修一个登录 bug",
    projectDir: "E:/proj/demo",
    lastActivityAt: 1_789_200_645_293,
    requestCount: 12,
    inputTokens: 1_000,
    outputTokens: 500,
    cacheReadTokens: 200,
    cacheWriteTokens: 0,
    ...overrides,
  };
}

/** 测试缝隙适配：把条目数组包成详情视图的就绪窗口（轮次列表置空）。 */
function initialDataOf(entries: readonly TranscriptEntry[]) {
  return {
    turns: [],
    total: entries.length,
    entries: entries.map((entry, seq) => ({ seq, entry })),
  };
}

const PAGE: SessionsPagePayload = {
  rows: [
    rowFixture(),
    rowFixture({
      id: 2,
      tool: "codex",
      externalId: "01a0947c",
      title: null,
      projectDir: null,
      requestCount: 0,
    }),
  ],
  total: 2,
};

it("会话页空态：提示扫描，右栏提示选择会话", () => {
  const html = renderToStaticMarkup(
    <SessionsPage
      strings={strings}
      disabledTools={[]}
      initialPageData={{ rows: [], total: 0 }}
    />,
  );
  expect(html).toContain(strings.sessionsEmpty);
  expect(html).toContain(strings.sessionsEmptyHint);
  expect(html).toContain(strings.sessionsPickHint);
});

it("会话页数据态：时间行左时间右次数，不显示工具名与成本", () => {
  const html = renderToStaticMarkup(
    <SessionsPage strings={strings} disabledTools={[]} initialPageData={PAGE} />,
  );
  expect(html).toContain("修一个登录 bug");
  expect(html).toContain(strings.sessionsUntitled);
  // 工具身份由图标承担：名称不再以可见文本出现（品牌 SVG 内部的
  // 渐变 id 不算——那只是图标标记）。
  expect(html).not.toContain(">claude-code<");
  expect(html).not.toContain(">codex<");
  // 元信息行两端对齐：时间在左，请求次数右对齐同一行。
  expect(html).toContain("justify-between");
  expect(html).toContain(strings.sessionsRequestsUnit);
  expect(html).not.toContain(strings.detailsCostUnknown);
  expect(html).not.toContain(formatCostMicros(3_500));
  expect(html).toContain(strings.paginationTotal.replace("{total}", "2"));
});

it("筛选条：搜索框与工具下拉并行，禁用工具不进选项", () => {
  const html = renderToStaticMarkup(
    <SessionsPage
      strings={strings}
      disabledTools={["codex"]}
      initialPageData={PAGE}
    />,
  );
  expect(html).toContain(strings.sessionsSearchHint);
  expect(html).toContain(strings.sessionsFilterTools);
  expect(html.split('role="checkbox"').length - 1).toBe(2);
  expect(html).toContain("pointer-events-none opacity-0");
  expect(html).not.toContain("stroke-accent");
  expect(html).toContain(strings.paginationTotal.replace("{total}", "2"));
});

it("勾选会话：勾选行复选框常驻，操作栏随勾选出现", () => {
  const html = renderToStaticMarkup(
    <SessionsPage
      strings={strings}
      disabledTools={[]}
      initialPageData={PAGE}
      initialCheckedIds={[1]}
    />,
  );
  expect(html).toContain(strings.sessionsSelectedCount.replace("{n}", "1"));
  expect(html).toContain(strings.sessionsSelectPage);
  expect(html).toContain(strings.sessionsClearPage);
  expect(html).toContain(strings.sessionsDeleteSelected);
  expect(html.split('role="checkbox"').length - 1).toBe(2);
  expect(html.split("[&amp;_svg]:size-4").length - 1).toBe(2);
  expect(html).toContain("stroke-accent");
  expect(html).toContain("stroke-ink-muted/50");
  expect(html).toContain("pointer-events-none opacity-0");
  expect(html).not.toContain("transition-all");
  expect(html).not.toContain(strings.paginationTotal.replace("{total}", "2"));
});

// 行级与详情头部删除入口 + 轻确认弹层（删除语义：文件进回收站，统计保留）。

it("行级删除入口：每行悬停槽位带删除按钮，槽位常驻留白", () => {
  const html = renderToStaticMarkup(
    <SessionsPage strings={strings} disabledTools={[]} initialPageData={PAGE} />,
  );
  // 两行 → 两枚删除按钮（aria-label）；槽位常驻留白不因浮现移动行内容。
  expect(html.split(`aria-label="${strings.sessionsDeleteRow}"`).length - 1).toBe(2);
  expect(html.split("w-6 shrink-0").length - 1).toBe(2);
  expect(html).toContain("hover:text-danger");
});

it("单删确认弹层：标题、说明与取消/删除两键进 DOM", () => {
  const html = renderToStaticMarkup(
    <SessionsPage
      strings={strings}
      disabledTools={[]}
      initialPageData={PAGE}
      initialConfirm={{ kind: "one", sessionId: 1 }}
    />,
  );
  expect(html).toContain('role="alertdialog"');
  expect(html).toContain(strings.sessionsDeleteOneTitle);
  expect(html).toContain(strings.sessionsDeleteConfirmBody);
  expect(html).toContain(strings.sessionsDeleteConfirm);
  expect(html).toContain(strings.confirmDialogCancel);
});

it("批量删除确认弹层：标题带上勾选数", () => {
  const html = renderToStaticMarkup(
    <SessionsPage
      strings={strings}
      disabledTools={[]}
      initialPageData={PAGE}
      initialCheckedIds={[1, 2]}
      initialConfirm={{ kind: "batch" }}
    />,
  );
  expect(html).toContain(strings.sessionsDeleteBatchTitle.replace("{n}", "2"));
  expect(html).toContain(strings.sessionsDeleteConfirmBody);
});

it("详情视图：用户气泡靠右、助手气泡与思考折叠、工具调用展示", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [{ kind: "text", text: "帮我看看这个 bug" }],
    },
    {
      role: "assistant",
      tsMs: 2_000,
      model: "claude-sonnet-4-5",
      blocks: [
        { kind: "thinking", text: "先定位" },
        { kind: "text", text: "结论：是空指针" },
        {
          kind: "toolCall",
          id: "t1",
          name: "bash",
          arguments: '{"cmd":"ls"}',
        },
        {
          kind: "toolResult",
          callId: "t1",
          content: "src/main.rs",
          isError: false,
        },
      ],
    },
    {
      role: "system",
      tsMs: null,
      model: null,
      blocks: [{ kind: "text", text: "系统上下文" }],
    },
  ];

  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );

  // 头部：返回按钮 + 标题 + 会话坐标（工具名由图标承担，坐标行只剩 id）。
  expect(html).toContain(strings.sessionsBack);
  expect(html).toContain("修一个登录 bug");
  expect(html).toContain("3bfaeb77");
  expect(html).not.toContain("claude-code /");
  // 气泡内容与标签。
  expect(html).toContain("帮我看看这个 bug");
  expect(html).toContain(strings.transcriptRoleUser);
  expect(html).toContain("结论：是空指针");
  expect(html).toContain("claude-sonnet-4-5");
  expect(html).toContain(strings.transcriptThinking);
  expect(html).toContain(strings.transcriptToolCall);
  expect(html).toContain("bash");
  expect(html).toContain("src/main.rs");
  expect(html).toContain(strings.transcriptRoleAssistant);
  expect(html).toContain("系统上下文");
  // 空指针不是错误结果：无 danger 类。
  expect(html).not.toContain("text-danger");
});

it("详情视图：超长助手回复卡默认折叠，与工具结果同阈值", () => {
  const longText = Array.from(
    { length: 20 },
    (_, i) => `回复第 ${i + 1} 行`,
  ).join("\n");
  const entries: TranscriptEntry[] = [
    {
      role: "assistant",
      tsMs: 1_000,
      model: null,
      blocks: [{ kind: "text", text: longText }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 默认收起：钳高 + 展开按钮；内容留在 DOM（视觉裁剪，与工具结果的
  // 截行不同），渲染/源码切换 chip 不受钳高影响仍在。
  expect(html).toContain("max-h-40 overflow-hidden");
  expect(html).toContain(strings.transcriptExpand);
  expect(html).toContain("回复第 1 行");
  expect(html).toContain(strings.markdownRender);
});

it("详情视图：信息头提供目录/刻度 Segmented 切换，默认列表视图", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [{ kind: "text", text: "第一问" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  expect(html).toContain(strings.sessionsViewToggle);
  expect(html).toContain(strings.sessionsViewList);
  expect(html).toContain(strings.sessionsViewRuler);
  expect(html.split('type="radio"').length - 1).toBe(2);
  // 默认仍是列表视图：目录面板在，刻度尺未挂载。
  expect(html).toContain(strings.sessionsOutlinePanel);
  expect(html).not.toContain('aria-label="轮 1"');
});

it("刻度尺：一轮一刻度，无悬停不出摘要卡", () => {
  const turns: TranscriptTurn[] = [
    { seq: 0, tsMs: 1_000, snippet: "第一问", parts: [{ kind: "text", text: "第一问" }], isSummary: false },
    { seq: 4, tsMs: 2_000, snippet: "第二问", parts: [{ kind: "text", text: "第二问" }], isSummary: false },
    { seq: 8, tsMs: null, snippet: "", parts: [], isSummary: true },
  ];
  const html = renderToStaticMarkup(
    <TurnRuler turns={turns} strings={strings} initialTurn={0} onJump={() => {}} />,
  );
  expect(html).toContain('aria-label="轮 1"');
  expect(html).toContain('aria-label="轮 2"');
  expect(html).toContain('aria-label="轮 3"');
  expect(html.split("rounded-full").length - 1).toBe(3);
  expect(html).not.toContain(strings.sessionsRulerJumpHint);
  expect(html).toContain(strings.sessionsViewRuler);
});

it("详情视图：超长工具卡默认折叠，短卡不折叠", () => {
  const longResult = Array.from(
    { length: 30 },
    (_, i) => `line-${i + 1}-content`,
  ).join("\n");
  const longArgs = JSON.stringify({ cmd: "x".repeat(600) });
  const entries: TranscriptEntry[] = [
    {
      role: "assistant",
      tsMs: 1_000,
      model: null,
      blocks: [
        { kind: "toolCall", id: "t1", name: "bash", arguments: longArgs },
        { kind: "toolResult", callId: "t1", content: longResult, isError: false },
        { kind: "toolResult", callId: "t2", content: "很短的结果", isError: false },
      ],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 长卡默认收起：钳高样式 + 「展开」按钮 + 预览行数之外的内容不进 DOM。
  expect(html).toContain("max-h-40 overflow-hidden");
  expect(html).toContain(strings.transcriptExpand);
  expect(html).toContain("line-1-content");
  expect(html).not.toContain("line-30-content");
  // 短卡：无折叠按钮（收起按钮不该出现）。
  expect(html).not.toContain(strings.transcriptCollapse);
  expect(html).toContain("很短的结果");
});

it("详情视图：思考块默认收起，摘要行带字数与首行预览", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "assistant",
      tsMs: 1_000,
      model: null,
      blocks: [
        {
          kind: "thinking",
          text: "Both endpoints confirmed working.\n第二行的细节不应在收起态出现",
        },
      ],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 收起态：字数 + 首行预览可见，全文其余行不进 DOM。
  expect(html).toContain(
    strings.transcriptThinkingCount.replace("{count}", "48"),
  );
  expect(html).toContain("Both endpoints confirmed working.");
  expect(html).not.toContain("第二行的细节不应在收起态出现");
});

it("详情视图：空态与加载态", () => {
  const empty = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf([])}
    />,
  );
  expect(empty).toContain(strings.transcriptEmpty);

  const failed = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
    />,
  );
  expect(failed).toContain(strings.transcriptLoading);
});

it("详情视图：图片附件显示元数据卡，文件缺失时降级提示", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [
        { kind: "text", text: "看看这张图" },
        {
          kind: "image",
          path: "C:/imgs/a.png",
          filename: "Clipboard_Screenshot.png",
          size: 194_910,
          exists: true,
          dataUrl: null,
        },
        {
          kind: "image",
          path: "C:/imgs/gone.png",
          filename: "gone.png",
          size: null,
          exists: false,
          dataUrl: null,
        },
      ],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  expect(html).toContain(strings.transcriptImage);
  expect(html).toContain("Clipboard_Screenshot.png");
  expect(html).toContain("190.3 KB");
  expect(html).toContain(strings.transcriptImageMissing);
  // 原始路径与体积字节不进 DOM：只展示文件名与人类可读体积。
  expect(html).not.toContain("194910");
});

it("详情视图：旧后端载荷缺 dataUrl 字段时不渲染无 src 的裂图", () => {
  // 旧二进制返回的 image 块没有 dataUrl 字段（运行时 undefined）——
  // 必须落到 asset 路径/元数据卡，绝不能出现 <img> 无 src 的空裂图。
  const legacyBlock = {
    kind: "image",
    path: "C:/imgs/a.png",
    filename: "legacy.png",
    size: 1024,
    exists: true,
  } as unknown as TranscriptEntry["blocks"][number];
  const entries: TranscriptEntry[] = [
    { role: "user", tsMs: 1_000, model: null, blocks: [legacyBlock] },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 非 Tauri 环境 assetUrl 为 null：回退元数据卡，且没有任何 <img> 元素。
  expect(html).not.toContain("<img");
  expect(html).toContain("legacy.png");
});

it("详情视图：dataUrl 内嵌图渲染白底图卡，底栏无操作按钮", () => {
  // opencode 的内嵌图没有本地路径：渲染图卡本体，但"复制路径 / 打开"
  // 两个操作按钮不出现（没有路径可操作）。
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [
        {
          kind: "image",
          path: "",
          filename: "inline.png",
          size: 2048,
          exists: true,
          dataUrl: "data:image/png;base64,iVBORw0KGgo=",
        },
      ],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  expect(html).toContain("<img");
  expect(html).toContain("data:image/png;base64,iVBORw0KGgo=");
  expect(html).toContain("inline.png");
  expect(html).toContain("2 KB");
  expect(html).not.toContain(strings.transcriptImageCopyPath);
  expect(html).not.toContain(strings.transcriptImageOpen);
});

// 注入上下文呈现：每条气泡自带的 纯净/完整 切换 + 占比条。

const INJECTED_ENTRIES: TranscriptEntry[] = [
  {
    role: "user",
    tsMs: 1_000,
    model: null,
    blocks: [
      {
        // 完整原文：reminder 段 + <user_query> 标签与提问内容。
        kind: "injected",
        text: "<system-reminder>配置内容SECRET</system-reminder><user_query>查看项目内容</user_query>",
        injectedChars: 45,
      },
      { kind: "text", text: "查看项目内容" },
    ],
  },
  {
    role: "user",
    tsMs: 2_000,
    model: null,
    blocks: [
      { kind: "injected", text: "<system-reminder>模式切换</system-reminder>", injectedChars: 39 },
    ],
  },
];

it("详情视图：纯净口径隐藏原文，每条气泡自带占比条与切换", () => {
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(INJECTED_ENTRIES)}
    />,
  );
  // 每条含注入的消息都自带 纯净/完整/纯文本 切换（两条消息 → 各三枚按钮）。
  expect(html.split(`>${strings.transcriptViewPure}<`).length - 1).toBe(2);
  expect(html.split(`>${strings.transcriptViewFull}<`).length - 1).toBe(2);
  expect(html.split(`>${strings.transcriptViewText}<`).length - 1).toBe(2);
  // 正文照常；注入块携带的整条原文（配置内容与标签）不出现。
  expect(html).toContain("查看项目内容");
  expect(html).not.toContain("配置内容SECRET");
  expect(html).not.toContain("&lt;user_query&gt;");
  // 占比条：注入 45 字符 / 正文 6 字符 = 88%；纯注入轮 100%。
  expect(html).toContain(
    strings.transcriptInjectedRatio
      .replace("{percent}", "88")
      .replace("{injected}", "45")
      .replace("{total}", "51"),
  );
  expect(html).toContain(
    strings.transcriptInjectedRatio
      .replace("{percent}", "100")
      .replace("{injected}", "39")
      .replace("{total}", "39"),
  );
});

it("用户气泡：完整口径用整条原文替换正文（提问内容在标签内），纯净只显正文", () => {
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: "<system-reminder>配置内容SECRET</system-reminder><user_query>查看项目内容</user_query>",
      injectedChars: 45,
    },
    { kind: "text", text: "查看项目内容" },
  ];
  const pure = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} />,
  );
  expect(pure).not.toContain("配置内容SECRET");
  expect(pure).not.toContain("&lt;user_query&gt;");
  expect(pure).toContain(
    strings.transcriptInjectedRatio
      .replace("{percent}", "88")
      .replace("{injected}", "45")
      .replace("{total}", "51"),
  );

  const full = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
      initialFoldOverrides={{ "0.0": false }}
    />,
  );
  // 原文一字不差（标签内的提问内容），正文不再重复渲染。
  // 标签被拆成三色 token span，只断言文本片段与标签名。
  expect(full).toContain("配置内容SECRET");
  expect(full).toContain("system-reminder");
  expect(full).toContain("user_query");
  expect(full.split("查看项目内容").length - 1).toBe(1);
});

it("用户气泡：完整口径正文未被原文覆盖时补渲染（grok 吸收的独立 reminder 行）", () => {
  // grok 的事件广播是独立日志行，吸收进气泡后 Injected 原文只含 reminder
  // 本身、不含提问——完整口径要同时呈现注入原文与用户输入。
  const blocks: TranscriptEntry["blocks"] = [
    { kind: "text", text: "问题二" },
    {
      kind: "injected",
      text: "<system-reminder>工作流可用</system-reminder>",
      injectedChars: 33,
    },
    {
      kind: "injected",
      text: "<system-reminder>MCP 已连接</system-reminder>",
      injectedChars: 30,
    },
  ];
  const full = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
    />,
  );
  expect(full).toContain("system-reminder");
  expect(full).toContain("问题二");
  const pure = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} />,
  );
  expect(pure).toContain("问题二");
  expect(pure).not.toContain("工作流可用");
});

it("用户气泡：0 注入字符的 query 信封块不显占比条，裸提问与从前一致", () => {
  const blocks: TranscriptEntry["blocks"] = [
    { kind: "injected", text: "<user_query>裸提问</user_query>", injectedChars: 0 },
    { kind: "text", text: "裸提问" },
  ];
  const pure = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} />,
  );
  expect(pure).toContain("裸提问");
  expect(pure).not.toContain("user_query");
  expect(pure).not.toContain(
    strings.transcriptInjectedRatio
      .replace("{percent}", "0")
      .replace("{injected}", "0")
      .replace("{total}", "3"),
  );
  const full = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} initialMode="full" />,
  );
  expect(full).toContain("裸提问");
  expect(full).not.toContain("user_query");
});

it("用户气泡：吸收 reminder 后完整口径原文树含 <user_query> 元素（对齐 workbuddy）", () => {
  const blocks: TranscriptEntry["blocks"] = [
    { kind: "injected", text: "<user_query>查看项目内容</user_query>", injectedChars: 0 },
    { kind: "text", text: "查看项目内容" },
    {
      kind: "injected",
      text: "<system-reminder>MCP 已连接</system-reminder>",
      injectedChars: 30,
    },
  ];
  const full = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
    />,
  );
  expect(full).toContain("user_query");
  expect(full).toContain("system-reminder");
  // 提问由信封原文承载，正文不重复渲染。
  expect(full.split("查看项目内容").length - 1).toBe(1);
});

it("用户气泡：完整口径原文逐行语法高亮 + 嵌套缩进，层级线随深度出现", () => {
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: "<system-reminder>\n配置内容SECRET\n</system-reminder>\n<user_query>查看项目内容</user_query>",
      injectedChars: 52,
    },
    { kind: "text", text: "查看项目内容" },
  ];
  const full = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
      initialFoldOverrides={{ "0.0": false, "0.1": false }}
    />,
  );
  // 文本内容一字不差。
  expect(full).toContain("配置内容SECRET");
  expect(full).toContain("查看项目内容");
  // 标签名高亮（accent-ink 加粗）与正文（透明度弱一档）区分。
  expect(full).toContain("text-accent-ink font-medium");
  expect(full).toContain("text-accent-ink/85");
  // 嵌套行缩进（marginLeft 12px）+ 左侧层级线；闭合行落回开标签缩进。
  expect(full).toContain("margin-left:12px");
  expect(full).toContain("border-l");
  // 闭合行与开标签对齐：indent 0 的行直接跟闭合标签 token，缩进行只有内容行。
  expect(full).toContain(
    'style="margin-left:0"><span class="text-accent-ink font-medium">&lt;/system-reminder',
  );
  expect(full.split("margin-left:12px").length - 1).toBe(1);
});

it("完整口径原文按 XML 元素分组：开闭合配对、嵌套与行数计数", () => {
  const tree = buildOriginalTree(
    [
      {
        kind: "injected",
        text: "<system-reminder>\n<craft_mode>\n内容行\n</craft_mode>\n</system-reminder>\n<user_query>查看项目内容</user_query>",
        injectedChars: 66,
      },
    ],
    [],
  );
  // system-reminder 成元素；单行自足的 user_query 也是元素（整行 header）。
  expect(tree.map((node) => node.kind)).toEqual(["element", "element"]);
  const el = tree[0] as Extract<OriginalTree, { kind: "element" }>;
  expect(el.indent).toBe(0);
  expect(el.header).toBe("<system-reminder>");
  expect(el.footer).toBe("</system-reminder>");
  expect(el.children).toHaveLength(1);
  const nested = el.children[0] as Extract<OriginalTree, { kind: "element" }>;
  expect(nested.indent).toBe(1);
  expect(nested.footer).toBe("</craft_mode>");
  // 折叠外层隐藏 3 行（嵌套元素 2 行 + 闭合行）；嵌套元素收起隐藏 2 行。
  expect(el.foldedRows).toBe(3);
  expect(nested.foldedRows).toBe(2);
  // 单行自足元素：无 children/footer，收起 0 行（chip 不显行数提示）。
  const inline = tree[1] as Extract<OriginalTree, { kind: "element" }>;
  expect(inline.header).toBe("<user_query>查看项目内容</user_query>");
  expect(inline.children).toHaveLength(0);
  expect(inline.footer).toBeNull();
  expect(inline.foldedRows).toBe(0);
});

it("原文树：首尾空白行是换行残留，视图层裁掉（zcode 消息以 \\n 结尾）", () => {
  const tree = buildOriginalTree(
    [
      {
        kind: "harnessContext",
        tag: "system-reminder",
        text: "<system-reminder>\nagentsMd 内容\n</system-reminder>\n",
      },
    ],
    [],
  );
  // 尾随 \n 不再生成顶层空行；折叠行数只数内容行与闭合行。
  expect(tree).toHaveLength(1);
  const el = tree[0] as Extract<OriginalTree, { kind: "element" }>;
  expect(el.header).toBe("<system-reminder>");
  expect(el.footer).toBe("</system-reminder>");
  expect(el.foldedRows).toBe(2);
});

it("原文树：顶层信封之间的空行是拼接残留，不渲染成空行（codebuddy 信封以 \\n\\n 相连）", () => {
  const tree = buildOriginalTree(
    [
      {
        kind: "injected",
        text: "<user_info>\nOS win32\n</user_info>\n\n<user_query>查看项目内容</user_query>",
        injectedChars: 20,
      },
    ],
    [],
  );
  // 两个元素紧邻，中间没有空文本行。
  expect(tree).toHaveLength(2);
  const info = tree[0] as Extract<OriginalTree, { kind: "element" }>;
  expect(info.header).toBe("<user_info>");
  expect(info.footer).toBe("</user_info>");
  const query = tree[1] as Extract<OriginalTree, { kind: "element" }>;
  expect(query.header).toBe("<user_query>查看项目内容</user_query>");

  // 元素体内的空行是内容的一部分，保留不裁。
  const inner = buildOriginalTree(
    [
      {
        kind: "injected",
        text: "<project_context>\n第一段\n\n第二段\n</project_context>",
        injectedChars: 20,
      },
    ],
    [],
  );
  const el = inner[0] as Extract<OriginalTree, { kind: "element" }>;
  expect(el.children).toHaveLength(3);
  const middle = el.children[1];
  expect(middle?.kind).toBe("row");
});

it("原文树：带空格的标签名（codex 的 permissions instructions）也成元素", () => {
  // 曾经标签名正则不允许空格，<permissions instructions> 整段退化成平铺行。
  const tree = buildOriginalTree(
    [
      {
        kind: "injected",
        text: "<permissions instructions>\nFilesystem sandboxing defines rules.\n</permissions instructions>",
        injectedChars: 30,
      },
    ],
    [],
  );
  expect(tree).toHaveLength(1);
  const el = tree[0] as Extract<OriginalTree, { kind: "element" }>;
  expect(el.kind).toBe("element");
  expect(el.header).toBe("<permissions instructions>");
  expect(el.footer).toBe("</permissions instructions>");
  expect(el.foldedRows).toBe(2);
});

it("用户气泡：元素默认折叠成摘要 chip，展开后渲染完整原文", () => {
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: "<system-reminder>\n配置内容SECRET\n</system-reminder>\n<user_query>查看项目内容</user_query>",
      injectedChars: 52,
    },
    { kind: "text", text: "查看项目内容" },
  ];
  // 默认全折叠：children 与闭合行不渲染，chip 显示隐藏行数。
  const folded = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} initialMode="full" />,
  );
  expect(folded).not.toContain("配置内容SECRET");
  expect(folded).not.toContain("&lt;/system-reminder");
  expect(folded).toContain(
    strings.transcriptFoldedRows.replace("{count}", "2"),
  );
  // 用户展开后：完整原文可见，头部带收起按钮（此处 1 个元素）。
  const expanded = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
      initialFoldOverrides={{ "0.0": false }}
    />,
  );
  expect(expanded).toContain("配置内容SECRET");
  expect(
    expanded.split(`aria-label="${strings.transcriptCollapse}"`).length - 1,
  ).toBe(1);
  // 滚动容器带 pl-4：chevron 的悬挂空间在容器内，根元素按钮不被裁掉。
  expect(expanded).toContain("overflow-x-auto pl-4");
});

it("用户气泡：所有元素默认折叠，骨架 chip 全可见", () => {
  const longText = "很长的注入内容。".repeat(30);
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: `<system-reminder>\n<craft_mode>\n${longText}\n</craft_mode>\n</system-reminder>\n<system-reminder>\n短内容\n</system-reminder>`,
      injectedChars: 999,
    },
  ];
  const html = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} initialMode="full" />,
  );
  // 任何元素的内容默认不可见：长文本、短文本、闭合行都收进 chip。
  expect(html).not.toContain(longText);
  expect(html).not.toContain("短内容");
  expect(html).not.toContain("&lt;/craft_mode");
  expect(html).not.toContain("&lt;/system-reminder");
  // 两枚顶层元素 chip：外层容器收起 3 行，第二个 reminder 收起 2 行
  // （嵌套元素藏在容器 chip 里，展开后才出现）。
  expect(
    html.split(strings.transcriptFoldedRows.replace("{count}", "2")).length -
      1,
  ).toBe(1);
  expect(html).toContain(
    strings.transcriptFoldedRows.replace("{count}", "3"),
  );
  // 没有展开态的收起按钮。
  expect(html).not.toContain(
    `aria-label="${strings.transcriptCollapse}"`,
  );
});

it("用户气泡：单行自足的元素同样默认折叠成 chip", () => {
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: "<user_query>查看项目内容</user_query>",
      injectedChars: 20,
    },
  ];
  const folded = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} initialMode="full" />,
  );
  // 单行元素也成 chip（有底色、可点开），整行进预览，无行数提示。
  expect(folded).toContain("bg-accent-ink/10");
  expect(folded).toContain("查看项目内容");
  expect(folded).not.toContain(
    `aria-label="${strings.transcriptCollapse}"`,
  );
  // 展开后完整行渲染，带收起按钮。
  const expanded = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
      initialFoldOverrides={{ "0.0": false }}
    />,
  );
  expect(expanded).toContain("查看项目内容");
  expect(
    expanded.split(`aria-label="${strings.transcriptCollapse}"`).length - 1,
  ).toBe(1);
});

it("详情视图：纯注入消息落中性气泡，占比条在气泡内", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [
        {
          kind: "injected",
          text: "<system-reminder>模式切换</system-reminder>",
          injectedChars: 39,
        },
      ],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 中性气泡承载注入区：占比条 + 切换都在，原文不出现。
  expect(html).toContain("100%");
  expect(html).toContain(strings.transcriptViewPure);
  expect(html).not.toContain("模式切换");
});

it("用户气泡：完整口径把图片渲染在 @image 提及位置，而非末尾", () => {
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: "<user_query>前文 @image#1:shot.png 后文</user_query>",
      injectedChars: 12,
    },
    { kind: "text", text: "前文 @image#1:shot.png 后文" },
    {
      kind: "image",
      path: "C:/imgs/shot.png",
      filename: "shot.png",
      size: 1_234,
      exists: true,
      dataUrl: null,
    },
  ];
  const full = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="full"
      initialFoldOverrides={{ "0.0": false }}
    />,
  );
  // 非运行环境图片退元数据卡（1.2 KB 卡片）：夹在提及符号行与后文之间。
  const at = (needle: string) => full.indexOf(needle);
  expect(at("前文")).toBeGreaterThanOrEqual(0);
  expect(at("@image#1:shot.png")).toBeGreaterThan(at("前文"));
  expect(at("1.2 KB")).toBeGreaterThan(at("@image#1:shot.png"));
  expect(at("后文")).toBeGreaterThan(at("1.2 KB"));
});

it("用户气泡：纯文本口径渲染原文但不渲染任何图片", () => {
  const blocks: TranscriptEntry["blocks"] = [
    {
      kind: "injected",
      text: "<user_query>前文 @image#1:shot.png 后文</user_query>",
      injectedChars: 12,
    },
    { kind: "text", text: "前文 @image#1:shot.png 后文" },
    {
      kind: "image",
      path: "C:/imgs/shot.png",
      filename: "shot.png",
      size: 1_234,
      exists: true,
      dataUrl: null,
    },
  ];
  const text = renderToStaticMarkup(
    <UserTurnBubble
      blocks={blocks}
      strings={strings}
      initialMode="text"
      initialFoldOverrides={{ "0.0": false }}
    />,
  );
  // 原文在（提及标记保留为文字，落在独立行），图片完全不渲染。
  expect(text).toContain("前文");
  expect(text).toContain("@image#1:shot.png");
  expect(text).toContain("后文");
  expect(text).not.toContain("1.2 KB");
});

it("详情视图：任务通知渲染成系统事件条，不占用户气泡、不进目录", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 3_600_000,
      model: null,
      blocks: [
        {
          kind: "taskNotification",
          taskId: "9lfYLN",
          status: "completed",
          summary: "Background command pnpm-check completed",
          text: "<task-notification>…</task-notification>\n\nUse the TaskOutput tool…",
        },
      ],
    },
    {
      role: "user",
      tsMs: 3_600_100,
      model: null,
      blocks: [{ kind: "text", text: "看看结果" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 系统事件条：标题 + 状态徽章 + 摘要一行 + 可展开原文；右下角骑缝
  // 状态圆徽（completed → 绿勾 bg-ok）。
  expect(html).toContain(strings.transcriptTaskNotification);
  expect(html).toContain("completed");
  expect(html).toContain("Background command pnpm-check completed");
  expect(html).toContain(strings.transcriptExpand);
  expect(html).toContain("bg-ok");
  // 不占用户角色标签（只有真正的提问轮有一枚），目录也不收录。
  expect(html.split(`>${strings.transcriptRoleUser}<`).length - 1).toBe(1);
  expect(html.split("Background command pnpm-check completed").length - 1).toBe(1);
});

it("详情视图：只贴图不打字的用户消息套右对齐用户气泡", () => {
  // opencode 的 file part 不带文字：曾经只认 text 块，这类消息被当成
  // 工具结果平铺到左侧。
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 3_600_000,
      model: null,
      blocks: [
        { kind: "text", text: "带文字的提问" },
        {
          kind: "image",
          path: "C:/imgs/shot.png",
          filename: "shot.png",
          size: 1_234,
          exists: true,
          dataUrl: null,
        },
      ],
    },
    {
      role: "user",
      tsMs: 3_600_100,
      model: null,
      blocks: [
        {
          kind: "image",
          path: "",
          filename: "image",
          size: 94_532,
          exists: true,
          dataUrl: "data:image/png;base64,iVBORw0KGgo=",
        },
      ],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "opencode" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 第二条（纯图）也是用户气泡：角色标签行右对齐（justify-end 两枚），
  // data URL 图照常渲染。
  expect(html.split("justify-end").length - 1).toBeGreaterThanOrEqual(2);
  expect(html).toContain("data:image/png;base64,iVBORw0KGgo=");
});

it("详情视图：codex 注入上下文渲染成可展开的说明条，不占用户气泡", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 3_600_000,
      model: null,
      blocks: [
        {
          kind: "harnessContext",
          tag: "environment_context",
          text:
            "<environment_context>\n  <cwd>E:\\proj</cwd>\n  <timezone>Asia/Shanghai</timezone>\n</environment_context>",
        },
      ],
    },
    {
      role: "system",
      tsMs: 3_600_050,
      model: null,
      blocks: [
        {
          kind: "harnessContext",
          tag: "skills_instructions",
          text: "<skills_instructions> ## Skills …",
        },
      ],
    },
    {
      role: "user",
      tsMs: 3_600_080,
      model: null,
      blocks: [{ kind: "turnAborted" }],
    },
    {
      role: "user",
      tsMs: 3_600_100,
      model: null,
      blocks: [{ kind: "text", text: "真实提问" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "codex" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 两条注入卡在位：同一条消息的两个段共用一张卡（developer 2 段），
  // 卡头文案在，XML 元素默认折叠（标签 chip 可见、正文不可见）。
  expect(html.split(strings.transcriptHarnessContext).length - 1).toBe(2);
  expect(html.split("skills_instructions").length - 1).toBe(1);
  expect(html.split("environment_context").length - 1).toBe(1);
  expect(html).not.toContain("Asia/Shanghai");
  expect(html).toContain("真实提问");
  // 中断标记渲染成红色分隔线（均匀细线 + 裸文字，无胶囊底色），不再是注入说明条。
  expect(html).toContain(strings.transcriptTurnAborted);
  expect(html).toContain("bg-danger/25");
  expect(html).not.toContain("rounded-full bg-danger");
  expect(html).not.toContain("<turn_aborted>");
  // "用户"角色标签只有真实提问一枚——注入卡与中断线都不占用户气泡。
  expect(html.split(`>${strings.transcriptRoleUser}<`).length - 1).toBe(1);
});

it("详情视图：压缩摘要渲染分隔线与收起的摘要卡，目录给分节锚点", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000_000,
      model: null,
      blocks: [
        {
          kind: "historySummary",
          text: "# Conversation Summary\n\n用户只提出了一个明确请求。",
          hasContinue: true,
        },
      ],
    },
    {
      role: "user",
      tsMs: 1_000_100,
      model: null,
      blocks: [{ kind: "text", text: "继续干活" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={{
        ...initialDataOf(entries),
        turns: [
          { seq: 0, tsMs: 1_000_000, snippet: "", parts: [], isSummary: true },
          { seq: 1, tsMs: 1_000_100, snippet: "继续干活", parts: [{ kind: "text", text: "继续干活" }], isSummary: false },
        ],
      }}
    />,
  );
  // 分隔线 + 摘要卡标题 + 继续指令提示；摘要正文默认收起不可见。
  expect(html).toContain(strings.transcriptCompacted);
  expect(html).toContain(strings.transcriptHistorySummary);
  expect(html).toContain(strings.transcriptWithContinue);
  expect(html).not.toContain("用户只提出了一个明确请求");
  // 目录分节锚点：分隔文案出现两次（时间线卡片 + 目录条目）。
  expect(html.split(strings.transcriptCompacted).length - 1).toBe(2);
});

it("详情视图：未并入摘要的继续指令渲染弱化说明条", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [{ kind: "continueNotice" }],
    },
    {
      role: "user",
      tsMs: 2_000,
      model: null,
      blocks: [{ kind: "text", text: "真问题" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  expect(html).toContain(strings.transcriptContinueNotice);
  // 不占用户角色标签、不进目录（首条兜底也不行）。
  expect(html.split(`>${strings.transcriptRoleUser}<`).length - 1).toBe(1);
});

it("详情视图：助手文本卡走 Markdown 双口径（默认渲染）", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "assistant",
      tsMs: 1_000,
      model: null,
      blocks: [{ kind: "text", text: "结论是 **全局应用**。" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  expect(html).toContain("<strong>全局应用</strong>");
  expect(html).toContain(strings.markdownRender);
  expect(html).toContain(strings.markdownSource);
});

it("详情视图：目录不收录纯注入条目（首条兜底也不行）", () => {
  // 纯注入条目排第一：旧的兜底逻辑会把它当目录首项（摘要=角色标签"用户"）。
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [
        {
          kind: "injected",
          text: "<system-reminder>模式切换</system-reminder>",
          injectedChars: 39,
        },
      ],
    },
    {
      role: "user",
      tsMs: 2_000,
      model: null,
      blocks: [{ kind: "text", text: "查看项目内容" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture({ tool: "workbuddy" })}
      strings={strings}
      onBack={() => {}}
      initialData={initialDataOf(entries)}
    />,
  );
  // 纯注入轮不出角色标签（只留占比条），目录也不给它条目：
  // 全页"用户"标签只应出现一次（真实提问轮的行标）。
  expect(html.split(`>${strings.transcriptRoleUser}<`).length - 1).toBe(1);
  expect(html).toContain("查看项目内容");
});

it("详情视图：分页窗口只渲染已加载条目，目录来自轮次索引", () => {
  const entries: TranscriptEntry[] = [
    {
      role: "user",
      tsMs: 1_000,
      model: null,
      blocks: [{ kind: "text", text: "第一轮提问" }],
    },
    {
      role: "assistant",
      tsMs: 2_000,
      model: null,
      blocks: [{ kind: "text", text: "第一轮回答" }],
    },
  ];
  const html = renderToStaticMarkup(
    <SessionDetailView
      row={rowFixture()}
      strings={strings}
      onBack={() => {}}
      initialData={{
        total: 100,
        turns: [
          { seq: 0, tsMs: 1_000, snippet: "第一轮提问", parts: [{ kind: "text", text: "第一轮提问" }], isSummary: false },
          { seq: 30, tsMs: 5_000, snippet: "第三十轮提问", parts: [{ kind: "text", text: "第三十轮提问" }], isSummary: false },
        ],
        entries: entries.map((entry, seq) => ({ seq, entry })),
      }}
    />,
  );
  // 窗口内的条目照常渲染（气泡 + 目录各一次）；未加载的轮只出现在目录。
  expect(html.split("第一轮提问").length - 1).toBe(2);
  expect(html).toContain("第一轮回答");
  expect(html.split("第三十轮提问").length - 1).toBe(1);
});
