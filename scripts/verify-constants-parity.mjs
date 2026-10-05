#!/usr/bin/env node
/**
 * 校验跨语言契约：paths.rs 的 `pub const` ↔ constants.ts 的 `export const` 取值一致，
 * error.rs 的错误码符合来源前缀规范（core.* / gateway.*）。两侧各自的单测发现不了"一边改了另一边没改"。
 * 用法：node scripts/verify-constants-parity.mjs   # 任何不一致都以非 0 退出，让 CI 拦住
 */

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/**
 * 跨 IPC 边界的契约常量：两侧都必须存在，且取值相同。
 *
 * 这里刻意用显式清单，而不是"扫描两侧所有同名常量"——
 * MAIN_DB_FILE、PRICING_OVERRIDES_FILE 等只属于 Rust 侧，前端不该知道；
 * 反过来前端也会有纯 UI 常量。"哪些算契约"本身就是需要显式表达的知识。
 */
const SHARED_CONSTANTS = [
  "PRODUCT_NAME",
  "APP_IDENTIFIER",
  "DATA_DIR_NAME",
  "GATEWAY_TOKEN_PREFIX",
  "LS_KEY_LOCALE",
  "LS_KEY_THEME",
  "LS_KEY_ACCENT",
  "LS_KEY_TOOLS_DISABLED",
  "LS_KEY_INPUT_SCOPE",
  "LS_KEY_NUMBER_UNIT",
];

/**
 * index.html 防闪脚本里硬写的键名：内联脚本读不到 TS 常量，只能在这里比对。
 * 值为 JS 变量名与 Rust 常量名的对应关系。
 */
const INLINE_HTML_KEYS = [
  ["THEME_KEY", "LS_KEY_THEME"],
  ["ACCENT_KEY", "LS_KEY_ACCENT"],
];

/** 错误码的合法来源前缀：core.* 领域层、gateway.* 网关（阶段 4）；壳引入 shell.* 后再加。 */
const ERROR_CODE_PREFIXES = ["core.", "gateway."];

/**
 * 解析 `pub const NAME: &str = "value";`
 * 刻意不引入 Rust 解析器：常量声明格式固定，正则足够且无第三方依赖。
 */
function readRustConstants(relativePath) {
  const text = readFileSync(join(ROOT, relativePath), "utf8");
  const found = new Map();
  const pattern = /^pub const ([A-Z][A-Z0-9_]*): &str = "([^"]*)";$/gm;

  for (const match of text.matchAll(pattern)) {
    found.set(match[1], match[2]);
  }

  return found;
}

/** 解析 `export const NAME = "value";` */
function readTsConstants(relativePath) {
  const text = readFileSync(join(ROOT, relativePath), "utf8");
  const found = new Map();
  const pattern = /^export const ([A-Z][A-Z0-9_]*) = "([^"]*)";$/gm;

  for (const match of text.matchAll(pattern)) {
    found.set(match[1], match[2]);
  }

  return found;
}

/** 解析 ErrorCode 各变体上的 `#[serde(rename = "core.xxx")]`。 */
function readRustErrorCodes(relativePath) {
  const text = readFileSync(join(ROOT, relativePath), "utf8");
  const pattern = /#\[serde\(rename = "([^"]+)"\)\]/g;

  return [...text.matchAll(pattern)].map((match) => match[1]);
}

function main() {
  const failures = [];

  // ---- 常量：两侧逐一比对 ----------------------------------------------
  const rustConstants = readRustConstants("crates/toktol-core/src/paths.rs");
  const tsConstants = readTsConstants("src/constants.ts");

  // 自我保护：正则一旦因格式变动而失效，必须炸出来，而不是静默通过。
  if (rustConstants.size === 0) {
    failures.push("paths.rs: 未能解析到任何常量，正则可能已失效");
  }
  if (tsConstants.size === 0) {
    failures.push("constants.ts: 未能解析到任何常量，正则可能已失效");
  }

  console.log("共享常量（Rust paths.rs ↔ 前端 constants.ts）");

  for (const name of SHARED_CONSTANTS) {
    const rustValue = rustConstants.get(name);
    const tsValue = tsConstants.get(name);

    if (rustValue === undefined) {
      failures.push(`${name}: 缺失于 paths.rs`);
    } else if (tsValue === undefined) {
      failures.push(`${name}: 缺失于 constants.ts`);
    } else if (rustValue !== tsValue) {
      failures.push(`${name}: 取值不一致（paths.rs="${rustValue}" vs constants.ts="${tsValue}"）`);
    } else {
      console.log(`  ${name.padEnd(24)} ${rustValue}`);
    }
  }

  // ---- 首屏内联脚本的键名 --------------------------------------------
  // index.html 的防闪脚本读不到 TS 常量，只能在这里比对，否则改了常量首屏会静默读空。
  const html = readFileSync(join(ROOT, "index.html"), "utf8");
  console.log("\n首屏内联脚本（index.html ↔ paths.rs）");

  for (const [jsName, rustName] of INLINE_HTML_KEYS) {
    const found = new RegExp(`var ${jsName} = "([^"]*)";`).exec(html)?.[1];
    const expected = rustConstants.get(rustName);

    if (found === undefined) {
      failures.push(`index.html: 未找到 ${jsName} 声明（防闪脚本被改或删了？）`);
    } else if (found !== expected) {
      failures.push(`index.html: ${jsName}="${found}" 与 paths.rs 的 ${rustName}="${expected}" 不一致`);
    } else {
      console.log(`  ${rustName.padEnd(24)} ${found}`);
    }
  }

  // ---- 错误码：前缀、命名、唯一性 ----------------------------------------
  const errorCodes = readRustErrorCodes("crates/toktol-core/src/error.rs");

  if (errorCodes.length === 0) {
    failures.push("error.rs: 未能解析到任何错误码，正则可能已失效");
  }

  console.log(
    `\n错误码（error.rs，须为 ${ERROR_CODE_PREFIXES.map((p) => `${p}*`).join(" 或 ")}）`,

  );

  const seen = new Set();

  for (const code of errorCodes) {
    const problems = [];

    if (!ERROR_CODE_PREFIXES.some((prefix) => code.startsWith(prefix))) {
      problems.push(`缺少前缀之一 ${ERROR_CODE_PREFIXES.join(" / ")}`);
    }
    if (!/^[a-z][a-z0-9_]*(\.[a-z0-9_]+)*$/.test(code)) {
      problems.push("不符合 snake_case");
    }
    if (seen.has(code)) {
      problems.push("重复");
    }
    seen.add(code);

    if (problems.length > 0) {
      failures.push(`错误码 "${code}": ${problems.join("、")}`);
    } else {
      console.log(`  ${code}`);
    }
  }

  // ---- 收尾 -------------------------------------------------------------
  if (failures.length > 0) {
    console.error(`\nCONTRACT MISMATCH (${failures.length} 项):`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }

  console.log(
    `\nOK: ${SHARED_CONSTANTS.length} shared constants agree, ${errorCodes.length} error codes valid`,
  );
}

main();
