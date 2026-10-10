#!/usr/bin/env node
/**
 * i18n 门禁：界面文案只许住 src/i18n/strings.ts。
 *
 * 用 TypeScript AST 找字符串字面量 / 模板字面量 / JSX 文本里的 CJK——注释与
 * 变量名里的中文是合法的（这个仓库就是用中文注释的），只有会渲染出来的
 * 字面量才算文案。strings.ts 本体、测试文件、文件级豁免表与行级
 * `i18n-exempt` 标记不算违规；豁免必须带理由。
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import ts from "typescript";

const ROOT = "src";
const EXTS = [".tsx", ".ts"];
const SKIP = /\.test\./;
// CJK 统一表意文字 + 扩展 A：界面文案可能出现的全部汉字区间。
const CJK = /[\u3400-\u4dbf\u4e00-\u9fff]/;
// 行级豁免标记：该行字面量有正当理由，跟着写清楚为什么。
const EXEMPT_MARKER = "i18n-exempt";

/** 文件级豁免：整份文件不扫。每条注明为什么里面的中文不是界面文案。 */
const EXEMPT_FILES = new Set([
  // 文案表本体：全应用的界面文案都住这里。
  "src/i18n/strings.ts",
  // 浏览器 harness 的 mock 数据（生产 Tauri 不经过），布局调试用。
  "src/lib/overview/mock.ts",
  // 挂载点缺失的开发者诊断：React/i18n 尚未就绪，没有文案表可用。
  "src/main.tsx",
  // 中文单位制后缀是 NumberUnit="chinese" 偏好的语义本体（同 K/M/B/T 之于
  // "english"），用户选中这套单位就永远显示它们，不随界面语言切换。
  "src/lib/numberUnit.ts",
]);

function walk(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(path, out);
    } else if (EXTS.some((ext) => entry.name.endsWith(ext)) && !SKIP.test(entry.name)) {
      out.push(path);
    }
  }
  return out;
}

function checkText(text, file, node, sf, lines, violations) {
  if (!CJK.test(text)) return;
  const start = node.getStart(sf);
  const { line } = sf.getLineAndCharacterOfPosition(start);
  const source = lines[line] ?? "";
  if (source.includes(EXEMPT_MARKER)) return;
  violations.push(`${file}:${line + 1} ${text.trim().slice(0, 60)}`);
}

function visit(sf, node, file, lines, violations) {
  if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)) {
    checkText(node.text, file, node, sf, lines, violations);
  } else if (ts.isTemplateExpression(node)) {
    checkText(node.head.text, file, node, sf, lines, violations);
    for (const span of node.templateSpans) {
      checkText(span.literal.text, file, span, sf, lines, violations);
    }
  } else if (ts.isJsxText(node) && node.text.trim() !== "") {
    checkText(node.text, file, node, sf, lines, violations);
  }
  ts.forEachChild(node, (child) => visit(sf, child, file, lines, violations));
}

let violations = [];
for (const file of walk(ROOT)) {
  if (EXEMPT_FILES.has(file.replaceAll("\\", "/"))) continue;
  const source = readFileSync(file, "utf8");
  const lines = source.split("\n");
  const sf = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true);
  visit(sf, sf, file, lines, violations);
}

if (violations.length > 0) {
  console.error(`✗ 界面文案泄漏（应进 src/i18n/strings.ts）：`);
  for (const v of violations) console.error(`  ${v}`);
  console.error(
    `\ni18n 检查失败：${violations.length} 处。豁免用行级「// ${EXEMPT_MARKER}: 理由」或改 EXEMPT_FILES（须带理由）。`,
  );
  process.exit(1);
}
console.log("OK: i18n 检查通过（文案只在 strings.ts 与豁免表里）");
