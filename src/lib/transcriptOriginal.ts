/**
 * 注入块原文的可折叠树：把整条原文按 XML 元素分组。纯逻辑（无 React），
 * 渲染见 pages/sessions/OriginalTextView.tsx。
 */

import type { TranscriptBlock } from "./api";

export type ImageBlock = Extract<TranscriptBlock, { kind: "image" }>;

/**
 * 标签行切分：一次匹配给出开/闭合括号、标签名、属性段与收尾；第 5 组
 * 是标签之间的纯正文。未闭合的孤立 `<` 落进空隙分支原样保留。
 * 标签名允许空格分段（codex 的 `<permissions instructions>` 是官方形态），
 * 但仍必须紧跟词字符再以 `>` 收尾——`a < b > c` 这类比较文本不误伤。
 */
export const TAG_TOKEN_RE = /(<\/?)([\w.:-]+(?: [\w.:-]+)*)((?:\s+[\w.:-]+="[^"]*")*)(\s*\/?>)|([^<]+)/g;

/**
 * 原文行：text 行（带缩进）或按 @image 提及插入的图片行。
 */
export type OriginalRow =
  | { readonly kind: "line"; readonly text: string; readonly indent: number }
  | { readonly kind: "image"; readonly block: ImageBlock; readonly indent: number };

/**
 * 原文树：平铺行，或一个 XML 元素块（开标签行 + 子节点 + 闭合行）。
 * 元素是折叠单元：收起时隐藏 children 与 footer，只留摘要 chip。
 */
export type OriginalTree =
  | { readonly kind: "row"; readonly row: OriginalRow }
  | {
      readonly kind: "element";
      readonly indent: number;
      readonly header: string;
      readonly children: readonly OriginalTree[];
      readonly footer: string | null;
      /** 收起时隐藏的行数（children 全部行 + footer，不含 header）。 */
      readonly foldedRows: number;
    };

/** 构建期的可变草稿，finalize 时转成只读 OriginalTree。 */
type TreeDraft =
  | { kind: "row"; row: OriginalRow }
  | {
      kind: "element";
      indent: number;
      header: string;
      children: TreeDraft[];
      footer: string | null;
    };

/**
 * 把注入块携带的整条原文按 XML 元素分组为可折叠树。不做 XML 解析——
 * 沿用行级深度记账（开标签 +1、闭合 -1；行首闭合标签的缩进取降级后
 * 的深度）：行留下未闭合开标签（depth > indent）即开元素，行首闭合
 * 标签且缩进等于栈顶元素缩进即闭合该元素；未闭合元素自然延伸到文本
 * 末尾，闭合对不上栈顶时退化为平铺行。@image 提及处先保留符号行再插
 * 图片行（纯文本传空图片队列，符号自然保留）；没有提及对应的图片追加
 * 在末尾。提及规则须与 transcript.rs 的 image_mentions 一致：`#` 与
 * `:` 之间是数字，文件名吃到空白或 `<`。
 */
export function buildOriginalTree(
  injectedBlocks: readonly TranscriptBlock[],
  imageBlocks: readonly TranscriptBlock[],
): readonly OriginalTree[] {
  const queue = [...imageBlocks].filter(
    (block): block is ImageBlock => block.kind === "image",
  );
  const top: TreeDraft[] = [];
  const stack: Extract<TreeDraft, { kind: "element" }>[] = [];
  let depth = 0;

  const pushRow = (row: OriginalRow) => {
    const list = stack.length > 0 ? stack[stack.length - 1]!.children : top;
    list.push({ kind: "row", row });
  };

  const processLine = (line: string) => {
    // 顶层空行是信封之间的拼接残留（codebuddy 的信封以 \n\n 相连，
    // workbuddy 的 reminder 与 user_query 之间同款），留着会在折叠 chip
    // 之间渲染出整行空文本；元素体内的空行（depth > 0）是内容的一部分，
    // 不裁。
    if (line.trim() === "" && depth === 0) return;
    let indent = depth;
    let firstSeen = false;
    let firstIsClose = false;
    let sawOpen = false;
    let sawClose = false;
    // 深度记账：开标签 +1、闭合 -1、自闭合与正文不动。行首若是闭合
    // 标签，缩进取降级后的深度，与对应开标签对齐。
    for (const match of line.matchAll(TAG_TOKEN_RE)) {
      if (match[5] !== undefined) continue;
      if (match[1] === "</") {
        depth -= 1;
        sawClose = true;
        if (!firstSeen) {
          indent = Math.max(0, depth);
          firstSeen = true;
          firstIsClose = true;
        }
      } else {
        sawOpen = true;
        firstSeen = true;
        if (!(match[4] ?? "").includes("/")) depth += 1;
      }
    }
    depth = Math.max(0, depth);
    if (
      firstIsClose &&
      stack.length > 0 &&
      stack[stack.length - 1]!.indent === indent
    ) {
      // 闭合行归档栈顶元素（缩进相同即配对，容错于标签名不一致）。
      stack.pop()!.footer = line;
      return;
    }
    if (depth > indent) {
      // 行留下未闭合开标签：开一个元素，后续行收进它的 children。
      const element: Extract<TreeDraft, { kind: "element" }> = {
        kind: "element",
        indent,
        header: line,
        children: [],
        footer: null,
      };
      const list = stack.length > 0 ? stack[stack.length - 1]!.children : top;
      list.push(element);
      stack.push(element);
      return;
    }
    if (sawOpen && sawClose) {
      // 单行自足元素（<tag>…</tag> 同行开闭）：整行成元素节点，同样
      // 默认折叠；无 children/footer，行数提示为 0（chip 上隐藏）。
      const list = stack.length > 0 ? stack[stack.length - 1]!.children : top;
      list.push({
        kind: "element",
        indent,
        header: line,
        children: [],
        footer: null,
      });
      return;
    }
    pushRow({ kind: "line", text: line, indent });
  };

  const processLines = (text: string) => {
    for (const line of text.split("\n")) {
      processLine(line);
    }
  };

  const mentionRe = /@image#\d+:([^\s<]+)/g;
  for (const block of injectedBlocks) {
    // 注入块（workbuddy 的 system-reminder 原文）与工具注入的会话上下文块
    // （codex 的 environment_context 等）同构：都按整段原文展开成树。
    if (block.kind !== "injected" && block.kind !== "harnessContext") continue;
    // 首尾空白行是日志换行残留（zcode 的提醒消息以 \n 结尾），视图层裁掉，
    // 不然折叠 chip 下多一行隐形空文本；text 数据本身保持原文不动。
    const text = block.text.replace(/^(?:\s*\n)+|(?:\n\s*)+$/g, "");
    if (text === "") continue;
    let cursor = 0;
    for (const match of text.matchAll(mentionRe)) {
      const start = match.index ?? 0;
      if (start > cursor) {
        processLines(text.slice(cursor, start));
      }
      const filename = match[1] ?? "";
      const found = queue.findIndex(
        (b) => b.kind === "image" && b.filename === filename,
      );
      const image = queue.splice(found === -1 ? 0 : found, 1)[0];
      // 提及符号本身是原文的一部分：先保留符号行，再插图片。
      processLines(match[0]);
      if (image) {
        pushRow({ kind: "image", block: image, indent: depth });
      }
      cursor = start + match[0].length;
    }
    if (cursor < text.length) {
      processLines(text.slice(cursor));
    }
  }
  for (const image of queue) {
    pushRow({ kind: "image", block: image, indent: depth });
  }

  const rowsIn = (node: TreeDraft): number =>
    node.kind === "row"
      ? 1
      : node.children.reduce((n, child) => n + rowsIn(child), 0) +
        (node.footer !== null ? 1 : 0);
  const finalize = (nodes: readonly TreeDraft[]): OriginalTree[] =>
    nodes.map((node) => {
      if (node.kind === "row") return node;
      return {
        kind: "element",
        indent: node.indent,
        header: node.header,
        footer: node.footer,
        children: finalize(node.children),
        foldedRows:
          node.children.reduce((n, child) => n + rowsIn(child), 0) +
          (node.footer !== null ? 1 : 0),
      };
    });
  return finalize(top);
}
