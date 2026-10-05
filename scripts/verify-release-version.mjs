#!/usr/bin/env node
/**
 * 校验版本单一事实源三处一致：package.json、Cargo.toml [workspace.package]、
 * tauri.conf.json。打包前十分钟失败远好于打包后才发现。
 * 用法：node scripts/verify-release-version.mjs [--tag v0.1.0]   # 带 --tag 额外校验 tag 匹配
 */

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 从 JSON 文件里取 version 字段。 */
function readJsonVersion(relativePath) {
  const abs = join(ROOT, relativePath);
  const raw = readFileSync(abs, "utf8");
  const parsed = JSON.parse(raw);

  if (typeof parsed.version !== "string" || parsed.version.length === 0) {
    throw new Error(`${relativePath}: 缺少字符串类型的 version 字段`);
  }

  return parsed.version;
}

/**
 * 从 Cargo.toml 的 [workspace.package] 段里取 version。
 * 刻意不引入 TOML 解析依赖：段结构固定，逐行扫描足够且无第三方依赖。
 */
function readCargoWorkspaceVersion(relativePath) {
  const abs = join(ROOT, relativePath);
  const lines = readFileSync(abs, "utf8").split(/\r?\n/);

  let inSection = false;
  let sawVersionLine = false;

  for (const line of lines) {
    const trimmed = line.trim();

    if (trimmed.startsWith("[")) {
      // 进入/离开 [workspace.package] 段；离开后立即停止扫描。
      if (trimmed === "[workspace.package]") {
        inSection = true;
        continue;
      }
      if (inSection) break;
      continue;
    }

    if (!inSection) continue;

    if (/^version\s*[.=]/.test(trimmed)) sawVersionLine = true;

    // 兼容行尾注释（`version = "0.1.0" # 与 package.json 一致`）与单引号字面量字符串。
    // 两者原先都会匹配失败，进而抛错——方向是对的（fail-closed，不会静默放过错误版本），
    // 但报错说的是"未找到 version"，排查时会绕一圈。
    const match = /^version\s*=\s*(?:"([^"]+)"|'([^']+)')\s*(?:#.*)?$/.exec(trimmed);
    if (match) return match[1] ?? match[2];
  }

  throw new Error(
    `${relativePath}: 未在 [workspace.package] 段找到可解析的 version` +
      (sawVersionLine
        ? "（该段存在 version 行但格式不符：本脚本只支持双引号或单引号字符串，且会忽略行尾 # 注释）"
        : ""),
  );
}

/** 取出 --tag <value>（或 --tag=<value>）。 */
function readTagArg(argv) {
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--tag") return argv[i + 1] ?? null;
    if (arg?.startsWith("--tag=")) return arg.slice("--tag=".length);
  }
  return null;
}

function main() {
  const sources = [
    ["package.json", readJsonVersion("package.json")],
    ["Cargo.toml", readCargoWorkspaceVersion("Cargo.toml")],
    ["src-tauri/tauri.conf.json", readJsonVersion("src-tauri/tauri.conf.json")],
  ];

  const versions = new Set(sources.map(([, version]) => version));

  for (const [file, version] of sources) {
    console.log(`  ${file.padEnd(26)} ${version}`);
  }

  if (versions.size !== 1) {
    console.error(
      `\nVERSION MISMATCH: expected one single version, got ${[...versions].join(", ")}`,
    );
    process.exit(1);
  }

  const version = sources[0][1];
  console.log(`\nOK: all three sources agree on ${version}`);

  const tag = readTagArg(process.argv.slice(2));
  if (tag === null) return;

  // 约定：tag 形如 v0.1.0，去掉 v 后必须与版本完全一致（不放行 v0.1.0-beta 这类模糊匹配）。
  const tagVersion = tag.replace(/^v/, "");
  if (tagVersion !== version) {
    console.error(`\nTAG MISMATCH: tag ${tag} does not match version ${version}`);
    process.exit(1);
  }

  console.log(`OK: tag ${tag} matches version ${version}`);
}

main();
