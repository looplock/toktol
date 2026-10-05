#!/usr/bin/env node
/**
 * 体检：找出"在 Cargo.toml 里声明了、但代码里根本没提到"的依赖——声明会先于代码到位，
 * 用不上的依赖白付编译时间与供应链面。词法启发式，先观察误报，稳定后接 --strict 进 CI。
 * 用法：node scripts/verify-declared-deps.mjs [--strict]   # 默认只告警，--strict 非 0 退出
 */

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/**
 * 白名单：确属"声明了但扫描不到"的例外，形如 "成员路径:依赖名"。
 *
 * 典型场景：依赖只通过宏展开进入代码（如某 derive 生成的代码里才出现 crate 名）。
 * 加白名单时必须写清原因，否则这个口子会变成新的僵尸依赖收容所。
 */
const ALLOWLIST = [
  // 架构铁律 1 要求的依赖方向：toktol-core ← toktol-gateway。
  // 阶段 4 之前网关确实没有使用它的代码，但这条边必须留在 Cargo.toml 里——
  // 一旦有人反向加上 `toktol-core -> toktol-gateway`，Cargo 会当场报循环依赖。
  // 删掉它等于撤掉这道唯一的机器检查，只靠文档和评审记性。
  "crates/toktol-gateway:toktol-core",
];

/** Cargo.toml 依赖表里这些是配置项，不是依赖名。 */
const RESERVED_KEYS = new Set([
  "version",
  "features",
  "default-features",
  "default_features",
  "path",
  "optional",
  "git",
  "branch",
  "rev",
  "tag",
  "package",
  "registry",
]);

/** 从根清单读出 workspace 成员，避免在这里再写死一份。 */
function readWorkspaceMembers(relativePath) {
  const text = readFileSync(join(ROOT, relativePath), "utf8");
  const match = /^members\s*=\s*\[([^\]]*)\]/m.exec(text);
  if (!match) return [];

  return [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

/** 读出某个成员清单里 [dependencies] / [dev-dependencies] / [build-dependencies] 的依赖名。 */
function readDeclaredDeps(relativePath) {
  const lines = readFileSync(join(ROOT, relativePath), "utf8").split(/\r?\n/);
  const sections = new Set(["[dependencies]", "[dev-dependencies]", "[build-dependencies]"]);
  const deps = [];
  let inSection = false;

  for (const line of lines) {
    const trimmed = line.trim();

    if (trimmed.startsWith("[")) {
      inSection = sections.has(trimmed);
      continue;
    }
    if (!inSection || trimmed === "" || trimmed.startsWith("#")) continue;

    // `serde.workspace = true` / `serde = "1.0"` / `serde = { ... }`
    const match = /^([A-Za-z0-9_-]+)\s*[.=]/.exec(trimmed);
    if (!match) continue;
    if (RESERVED_KEYS.has(match[1])) continue;

    deps.push(match[1]);
  }

  return deps;
}

/** 递归收集一个目录下的 .rs 文件。 */
function collectRustFiles(dir, out = []) {
  if (!existsSync(dir)) return out;

  for (const entry of readdirSync(dir)) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      collectRustFiles(path, out);
    } else if (path.endsWith(".rs")) {
      out.push(path);
    }
  }

  return out;
}

/** 剥掉块注释、行注释与字符串字面量——这三种位置里的名字都不算"使用"。 */
function stripNonCode(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/"(?:\\.|[^"\\])*"/g, '""')
    .replace(/r#*"[\s\S]*?"#/g, '""')
    .replace(/\/\/[^\n]*/g, " ");
}

/** 收集成员的全部 Rust 代码（含测试与 build.rs），并剥掉注释与字符串。 */
function readMemberCode(member) {
  const files = [
    ...collectRustFiles(join(ROOT, member, "src")),
    ...collectRustFiles(join(ROOT, member, "tests")),
    ...collectRustFiles(join(ROOT, member, "benches")),
    join(ROOT, member, "build.rs"),
  ].filter((path) => existsSync(path));

  return files.map((path) => stripNonCode(readFileSync(path, "utf8"))).join("\n");
}

/**
 * crate 名在 Cargo.toml 里用连字符（`tauri-plugin-x`），在 Rust 代码里只能用下划线
 * （`tauri_plugin_x`）——标识符不允许连字符。所以只需按下划线形式匹配。
 */
function isUsed(dep, code) {
  return new RegExp(`\\b${dep.replace(/-/g, "_")}\\b`).test(code);
}

function main() {
  const strict = process.argv.includes("--strict");
  const members = readWorkspaceMembers("Cargo.toml");

  // 自我保护：成员清单解析失败必须炸出来，否则"零问题"可能只是"什么都没查"。
  if (members.length === 0) {
    console.error("错误：未能从根 Cargo.toml 解析到 workspace members");
    process.exit(1);
  }

  const unused = [];

  for (const member of members) {
    const deps = readDeclaredDeps(join(member, "Cargo.toml"));
    const code = readMemberCode(member);

    if (deps.length === 0) {
      console.error(`错误：${member}/Cargo.toml 未解析到任何依赖，正则可能已失效`);
      process.exit(1);
    }

    console.log(`${member}  (${deps.length} 个声明)`);

    for (const dep of deps) {
      const allowed = ALLOWLIST.includes(`${member}:${dep}`);
      const used = isUsed(dep, code);

      if (used) {
        console.log(`  ok     ${dep}`);
      } else if (allowed) {
        console.log(`  skip   ${dep}（白名单）`);
      } else {
        console.log(`  UNUSED ${dep}`);
        unused.push(`${member} -> ${dep}`);
      }
    }
  }

  if (unused.length === 0) {
    console.log("\nOK: 每个声明的依赖都能在代码里找到引用");
    return;
  }

  console.error(`\nWARN: ${unused.length} 个依赖声明后未被使用：`);
  for (const item of unused) {
    console.error(`  - ${item}`);
  }
  console.error(
    "\n这些依赖只是增加编译时间与供应链面，没有支撑任何代码。" +
      "\n要么删掉声明（用到时再加），要么确认确实是误报后加进 ALLOWLIST。",
  );

  if (strict) {
    console.error("\n--strict 已指定：以非 0 退出码结束");
    process.exit(1);
  }

  console.log("\n（宽松模式：退出码为 0。确认无误报后可改用 --strict 升级为硬门禁）");
}

main();
