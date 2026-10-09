import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";

import { stringsFor } from "../../i18n/strings";
import type { TranscriptEntry } from "../../lib/api";
import { StreamEntry, UserTurnBubble } from "./TranscriptStream";

// SSR 直渲：单条消息按内容形态分派。折叠交互（展开/收起、口径切换）在
// effect 之外靠状态 + 回调，SSR 只断言各形态首帧的结构性文案。

const strings = stringsFor();

function entry(overrides: Partial<TranscriptEntry> = {}): TranscriptEntry {
  return { role: "user", tsMs: 1_760_000_000_000, model: null, blocks: [], ...overrides };
}

it("用户轮：强调气泡 + 角色标签 + 正文", () => {
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({
        blocks: [{ kind: "text", text: "帮我看看这个报错" }],
      })}
      strings={strings}
    />,
  );

  expect(html).toContain(strings.transcriptRoleUser);
  expect(html).toContain("帮我看看这个报错");
});

it("助手消息：模型名入标签行；文本、工具调用卡、工具结果卡各归其位", () => {
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({
        role: "assistant",
        model: "claude-sonnet-4-5",
        blocks: [
          { kind: "text", text: "我先看一下文件" },
          {
            kind: "toolCall",
            id: "call-1",
            name: "read_file",
            arguments: '{"path":"src/main.rs"}',
          },
          { kind: "toolResult", callId: "call-1", content: "fn main() {}", isError: false },
        ],
      })}
      strings={strings}
    />,
  );

  expect(html).toContain(strings.transcriptRoleAssistant);
  expect(html).toContain("claude-sonnet-4-5");
  expect(html).toContain("我先看一下文件");
  expect(html).toContain(strings.transcriptToolCall);
  expect(html).toContain("read_file");
  expect(html).toContain(strings.transcriptParams);
  expect(html).toContain("fn main() {}");
});

it("思考块：字数徽标 + 首行预览", () => {
  const text = "需要先检查状态机\n第二行";
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({
        role: "assistant",
        blocks: [{ kind: "thinking", text }],
      })}
      strings={strings}
    />,
  );

  expect(html).toContain(
    strings.transcriptThinkingCount.replace("{count}", String(text.length)),
  );
  expect(html).toContain("需要先检查状态机");
});

it("系统消息渲染成居中弱化说明，无角色标签", () => {
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({ role: "system", blocks: [{ kind: "text", text: "会话已结束" }] })}
      strings={strings}
    />,
  );

  expect(html).toContain("会话已结束");
  expect(html).not.toContain(strings.transcriptRoleSystem);
});

it("任务通知渲染为事件卡：标题 + 摘要", () => {
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({
        blocks: [
          {
            kind: "taskNotification",
            taskId: "task-1",
            status: "completed",
            summary: "测试全部通过",
            text: "<task-notification>原文</task-notification>",
          },
        ],
      })}
      strings={strings}
    />,
  );

  expect(html).toContain(strings.transcriptTaskNotification);
  expect(html).toContain("测试全部通过");
});

it("中断标记渲染为红色分隔线徽章", () => {
  const html = renderToStaticMarkup(
    <StreamEntry entry={entry({ blocks: [{ kind: "turnAborted" }] })} strings={strings} />,
  );

  expect(html).toContain(strings.transcriptTurnAborted);
});

it("工具注入上下文渲染为一张卡：说明文案 + 字符数", () => {
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({
        blocks: [{ kind: "harnessContext", tag: "environment_context", text: "<environment_context>x</environment_context>" }],
      })}
      strings={strings}
    />,
  );

  expect(html).toContain(strings.transcriptHarnessContext);
  expect(html).toContain("environment_context");
});

it("纯注入消息：中性气泡 + 占比条 + 三个口径切换", () => {
  const html = renderToStaticMarkup(
    <StreamEntry
      entry={entry({
        blocks: [{ kind: "injected", text: "<system-reminder>x</system-reminder>", injectedChars: 30 }],
      })}
      strings={strings}
    />,
  );

  expect(html).toContain(
    strings.transcriptInjectedRatio.replace("{percent}", "100").replace("{injected}", "30").replace("{total}", "30"),
  );
  expect(html).toContain(strings.transcriptViewPure);
  expect(html).toContain(strings.transcriptViewFull);
  expect(html).toContain(strings.transcriptViewText);
});

it("UserTurnBubble：纯净口径只出正文，完整口径出现原文树", () => {
  const blocks = [
    { kind: "injected" as const, text: "<user_query>hello</user_query>", injectedChars: 27 },
    { kind: "text" as const, text: "帮我修这个 bug" },
  ];
  const pure = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} initialMode="pure" />,
  );

  expect(pure).toContain("帮我修这个 bug");
  expect(pure).not.toContain("<user_query>".replace("<", "&lt;"));

  const full = renderToStaticMarkup(
    <UserTurnBubble blocks={blocks} strings={strings} initialMode="full" />,
  );

  // 完整口径：原文树按 XML 元素呈现（元素默认折叠，chip 出现标签名）。
  expect(full).toContain("user_query");
});
