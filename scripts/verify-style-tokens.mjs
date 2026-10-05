#!/usr/bin/env node
/**
 * 样式语义守卫：扫描 src 下的 tsx/ts，拦截游离灰阶与非语义圆角，
 * 对应 src/styles/tokens.css 顶部的「底色语义表」注释。
 *
 * 规则：
 *   1. bg-ink/N —— 游离灰阶，唯一例外是浮层遮罩 bg-ink/40（半透明压暗）。
 *   2. rounded-sm/md/lg/xl/2xl/3xl —— 非语义圆角，统一三档：
 *      rounded-card（容器）/ rounded-control（控件）/ rounded-full（胶囊）。
 *   3. 裸 rounded —— 应写 rounded-control。
 * dashboard.css 里遗留的裸 border-radius 不在本次扫描范围（CSS 文件）。
 */
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const ROOT = "src";
const EXTS = [".tsx", ".ts"];
const SKIP = /\.test\./;

const RULES = [
  {
    name: "游离灰阶 bg-ink/N（浮层遮罩 bg-ink/40 除外）",
    re: /bg-ink\/\d+/g,
    allow: (line) => line.includes("bg-ink/40"),
  },
  {
    name: "非语义圆角（统一 rounded-card / rounded-control / rounded-full）",
    re: /rounded-(?:sm|md|lg|xl|2xl|3xl)\b/g,
  },
  {
    name: "裸 rounded（应写 rounded-control）",
    re: /(?<![\w-])rounded(?![\w-])/g,
  },
];

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

let violations = 0;
for (const file of walk(ROOT)) {
  const lines = readFileSync(file, "utf8").split("\n");
  lines.forEach((line, index) => {
    for (const rule of RULES) {
      if (rule.allow?.(line) === true) continue;
      const hits = line.match(rule.re);
      if (hits !== null) {
        console.error(`✗ ${file}:${index + 1} ${rule.name}: ${hits.join(", ")}`);
        violations += hits.length;
      }
    }
  });
}

if (violations > 0) {
  console.error(
    `\n样式语义检查失败：${violations} 处。规则见 src/styles/tokens.css 顶部「底色语义表」。`,
  );
  process.exit(1);
}
console.log("OK: 样式语义检查通过（无游离灰阶 / 非语义圆角）");
