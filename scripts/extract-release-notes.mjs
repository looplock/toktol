// 从 CHANGELOG.md 提取指定版本的段落作为发布正文（release.yml 的 releaseBody）。
// 版本段落是发布正文的单一事实源（见 CHANGELOG 头部契约）：tag 触发发布时
// 段落缺失即非零退出，拦住"发布出去一个空壳 Release"。用法：
//   node scripts/extract-release-notes.mjs v0.1.0   # tag 名，v 前缀可有可无

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const version = (process.argv[2] ?? "").replace(/^v/, "");
if (version === "") {
  console.error("usage: extract-release-notes.mjs <version|tag>");
  process.exit(2);
}

const changelogPath = join(dirname(fileURLToPath(import.meta.url)), "..", "CHANGELOG.md");
const lines = readFileSync(changelogPath, "utf8").split(/\r?\n/);

// 段落从 `## [x.y.z] - 日期` 标题的下一行开始，到下一个 `## ` 标题或文件尾。
const heading = lines.findIndex((line) =>
  new RegExp(`^## \\[?v?${version.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\]?($| )`).test(line),
);
if (heading === -1) {
  console.error(`CHANGELOG.md has no section for ${version} — write it before tagging.`);
  process.exit(1);
}

let end = lines.length;
for (let i = heading + 1; i < lines.length; i += 1) {
  if (lines[i].startsWith("## ")) {
    end = i;
    break;
  }
}

// 掐掉段落首尾空行：releaseBody 不需要多余的空行包装。
const body = lines.slice(heading + 1, end).join("\n").replace(/^\n+|\n+$/g, "");
if (body === "") {
  console.error(`CHANGELOG.md section for ${version} is empty.`);
  process.exit(1);
}
process.stdout.write(body);
