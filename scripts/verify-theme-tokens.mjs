#!/usr/bin/env node
/**
 * 校验设计令牌：
 *   1. 明暗两套的键集合一致；主题只有明暗一维，`data-theme` 只允许这两个值；
 *   2. 涨跌色的色相不许越出红/绿家族——数据语义不能被配色带跑；
 *      分类色不许落在涨跌色的色相上，否则会被读成涨跌；
 *   3. 两套的正文对比度过 WCAG AA；
 *   4. 强调色预设（data-accent）：明暗变体成对、只许覆盖 accent 三令牌、
 *      accent-ink 对比度过 AA、色相避开涨跌家族（green 是拍板例外，见
 *      DOWN_FAMILY_EXCEPTIONS），且与 theme.ts 的
 *      ACCENT_IDS / DEFAULT_ACCENT 和预览色板令牌三方对得上。
 * 色值选错在浏览器里只是"某个地方发白"，不会报错，所以只能靠脚本盯。
 * 用法：node scripts/verify-theme-tokens.mjs
 */

import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

/** 正文对比度门槛（WCAG AA 1.4.3）。 */
const AA_TEXT = 4.5;

/** 涨跌色相家族：红涨绿跌是本产品的数据语义，色相不许被改。 */
const HUE_FAMILIES = {
  up: [
    [340, 360],
    [0, 20],
  ],
  down: [[100, 175]],
};

/** 分类色与涨跌色之间至少隔开这么多度，否则同一个色会被读成两种含义。 */
const CHART_HUE_MARGIN = 20;

/** 允许落进跌绿家族的强调色白名单。green（正淡绿，~142°）是用户拍板的例外：
 * 按钮与选中态会与"跌"同色相，语义风险已知并接受；其余预设仍守住禁区。
 * 涨红家族没有例外——红色强调色与"涨"撞车比绿更难分辨。 */
const DOWN_FAMILY_EXCEPTIONS = new Set(["green"]);

const MODES = ["light", "dark"];

function parseBlocks(css) {
  const blocks = [];

  // 嵌套的 @media 块扫不进来，但它的内层块能——内块的"{前一段"正好是干净的选择器。
  for (const match of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    blocks.push({ selector: match[1].trim().replace(/\s+/g, " "), body: match[2] });
  }

  return blocks;
}

function readVars(body) {
  const vars = new Map();

  for (const match of body.matchAll(/(--tt-[a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    vars.set(match[1], match[2].trim());
  }

  return vars;
}

function parseHex(value) {
  const hex = value.replace("#", "");
  const full = hex.length === 3 ? [...hex].map((char) => char + char).join("") : hex;

  return [
    parseInt(full.slice(0, 2), 16),
    parseInt(full.slice(2, 4), 16),
    parseInt(full.slice(4, 6), 16),
  ];
}

function luminance([r, g, b]) {
  const channel = (raw) => {
    const value = raw / 255;

    return value <= 0.03928 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };

  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

function contrast(a, b) {
  const [light, dark] = [luminance(a), luminance(b)].sort((x, y) => y - x);

  return (light + 0.05) / (dark + 0.05);
}

function hue([r, g, b]) {
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const delta = max - min;

  if (delta === 0) return 0;

  let value;
  if (max === r) value = ((g - b) / delta) % 6;
  else if (max === g) value = (b - r) / delta + 2;
  else value = (r - g) / delta + 4;

  return ((value * 60) % 360 + 360) % 360;
}

/** 环形色相距离（0–180）。 */
function hueGap(a, b) {
  const delta = Math.abs(a - b) % 360;

  return Math.min(delta, 360 - delta);
}

/** 强调色块只许覆盖这三个外观令牌，多了说明在越权改语义/分层。 */
const ACCENT_BLOCK_VARS = ["--tt-accent", "--tt-accent-ink", "--tt-ring"];

const PREVIEW_PREFIX = "--tt-accent-preview-";

/** 从 theme.ts 提取预设清单与默认值：三个文件（css/ts/此脚本）靠它对齐。 */
function readAccentContract() {
  const text = readFileSync(join(ROOT, "src", "lib", "theme.ts"), "utf8");
  const ids = /\bexport const ACCENT_IDS = \[([^\]]*)\]/.exec(text)?.[1];
  const fallback = /\bexport const DEFAULT_ACCENT: AccentId = "([a-z]+)";/.exec(text)?.[1];

  if (ids === undefined || fallback === undefined) {
    return null;
  }

  return {
    ids: [...ids.matchAll(/"([a-z]+)"/g)].map((match) => match[1]),
    fallback,
  };
}

function main() {
  const failures = [];
  // 注释要先剥掉：块前面的说明性注释会粘进"选择器"，让 :root 匹配不上。
  const css = readFileSync(join(ROOT, "src/styles/tokens.css"), "utf8").replace(
    /\/\*[\s\S]*?\*\//g,
    "",
  );
  const blocks = parseBlocks(css);

  const bases = {};

  for (const mode of MODES) {
    bases[mode] = blocks.find(
      (block) =>
        block.selector.includes(`[data-theme="${mode}"]`) &&
        readVars(block.body).has("--tt-canvas"),
    );

    if (bases[mode] === undefined) {
      failures.push(`tokens.css: 找不到 ${mode} 基底块`);
    }
  }

  if (failures.length > 0) {
    console.error(`\nTOKEN MISMATCH (${failures.length} 项):`);
    for (const failure of failures) console.error(`  - ${failure}`);
    process.exit(1);
  }

  const vars = { light: readVars(bases.light.body), dark: readVars(bases.dark.body) };

  // ---- 1. 键集合，且 data-theme 只有明暗两态 -------------------------
  const diff = (a, b) => [...a.keys()].filter((key) => !b.has(key));

  for (const key of diff(vars.light, vars.dark)) {
    failures.push(`亮色独有、暗色缺失的令牌: ${key}`);
  }
  for (const key of diff(vars.dark, vars.light)) {
    failures.push(`暗色独有、亮色缺失的令牌: ${key}`);
  }

  for (const block of blocks) {
    for (const found of block.selector.matchAll(/\[data-theme="([^"]+)"\]/g)) {
      if (!MODES.includes(found[1])) {
        failures.push(
          `tokens.css: data-theme="${found[1]}" 不是明暗之一——主题只有这一维，别再引入主题 id`,
        );
      }
    }
  }

  // ---- 2 & 3. 逐个明暗：色相与对比度 --------------------------------
  const textPairs = [
    ["ink", "canvas"],
    ["ink-muted", "canvas"],
    ["ink", "surface-muted"],
    ["ink-muted", "surface-muted"],
    ["ink", "surface-subtle"],
    ["ink-muted", "surface-subtle"],
    ["accent-ink", "accent"],
  ];

  const summary = [];

  for (const mode of MODES) {
    const set = vars[mode];

    for (const [token, ranges] of Object.entries(HUE_FAMILIES)) {
      const value = set.get(`--tt-${token}`);
      const actual = hue(parseHex(value));
      const ok = ranges.some(([lo, hi]) => actual >= lo && actual <= hi);

      if (!ok) {
        failures.push(
          `${mode} 的 --tt-${token}=${value} 色相 ${Math.round(actual)}° 越出 ${JSON.stringify(ranges)}`,
        );
      }
    }

    for (const [token] of Object.entries(HUE_FAMILIES)) {
      const trend = hue(parseHex(set.get(`--tt-${token}`)));

      for (let index = 1; index <= 8; index += 1) {
        const value = set.get(`--tt-chart-${index}`);
        const gap = hueGap(hue(parseHex(value)), trend);

        if (gap < CHART_HUE_MARGIN) {
          failures.push(
            `${mode} 的 --tt-chart-${index}=${value} 与 --tt-${token} 只差 ${Math.round(gap)}°（须 ≥${CHART_HUE_MARGIN}°）`,
          );
        }
      }
    }

    let worst = Infinity;

    for (const [fg, bg] of textPairs) {
      const ratio = contrast(parseHex(set.get(`--tt-${fg}`)), parseHex(set.get(`--tt-${bg}`)));

      worst = Math.min(worst, ratio);

      if (ratio < AA_TEXT) {
        failures.push(`${mode}: ${fg} on ${bg} 对比度 ${ratio.toFixed(2)} < ${AA_TEXT}`);
      }
    }

    summary.push({ mode, worst });
  }

  // ---- 4. 强调色预设 ---------------------------------------------------
  // 收集 data-accent 块：明暗变体必须成对出现，裸 [data-accent]（无明暗前缀）
  // 直接判失败——它与 dark 基底特异性相同，压不住暗色基底。
  const accentVariants = new Map();

  for (const block of blocks) {
    const name = /\[data-accent="([a-z]+)"\]/.exec(block.selector)?.[1];

    if (name === undefined) continue;

    const mode = block.selector.includes('[data-theme="dark"]')
      ? "dark"
      : block.selector.includes('[data-theme="light"]')
        ? "light"
        : null;

    if (mode === null) {
      failures.push(`强调色 ${name}: 存在裸 [data-accent] 块，必须带 [data-theme] 前缀`);
      continue;
    }

    const variants = accentVariants.get(name) ?? {};
    variants[mode] = block;
    accentVariants.set(name, variants);
  }

  const previewNames = new Set();

  for (const block of blocks) {
    for (const key of readVars(block.body).keys()) {
      if (key.startsWith(PREVIEW_PREFIX)) previewNames.add(key.slice(PREVIEW_PREFIX.length));
    }
  }

  for (const [name, variants] of accentVariants) {
    for (const mode of MODES) {
      const block = variants[mode];

      if (block === undefined) {
        failures.push(`强调色 ${name}: 缺 ${mode} 变体`);
        continue;
      }

      const set = readVars(block.body);

      for (const key of ACCENT_BLOCK_VARS) {
        if (!set.has(key)) failures.push(`强调色 ${name}（${mode}）缺少 ${key}`);
      }
      for (const key of set.keys()) {
        if (!ACCENT_BLOCK_VARS.includes(key)) {
          failures.push(`强调色 ${name}（${mode}）越权覆盖了 ${key}，只允许 ${ACCENT_BLOCK_VARS.join(" / ")}`);
        }
      }

      const accentValue = set.get("--tt-accent");
      const inkValue = set.get("--tt-accent-ink");

      if (accentValue !== undefined && inkValue !== undefined) {
        const ratio = contrast(parseHex(inkValue), parseHex(accentValue));

        if (ratio < AA_TEXT) {
          failures.push(
            `强调色 ${name}（${mode}）: accent-ink on accent 对比度 ${ratio.toFixed(2)} < ${AA_TEXT}`,
          );
        }
      }

      if (accentValue !== undefined) {
        const actual = hue(parseHex(accentValue));

        for (const [token, ranges] of Object.entries(HUE_FAMILIES)) {
          if (token === "down" && DOWN_FAMILY_EXCEPTIONS.has(name)) {
            continue;
          }
          if (ranges.some(([lo, hi]) => actual >= lo && actual <= hi)) {
            failures.push(
              `强调色 ${name}（${mode}）色相 ${Math.round(actual)}° 落在涨跌 ${token} 家族里`,
            );
          }
        }
      }
    }
  }

  // 三方对齐：CSS 的 data-accent 块、预览色板令牌、theme.ts 的 ACCENT_IDS。
  const contract = readAccentContract();

  if (contract === null) {
    failures.push("theme.ts: 未能解析 ACCENT_IDS / DEFAULT_ACCENT，正则可能已失效");
  } else {
    const idSet = new Set(contract.ids);

    if (!idSet.has(contract.fallback)) {
      failures.push(`theme.ts: DEFAULT_ACCENT "${contract.fallback}" 不在 ACCENT_IDS 里`);
    }

    for (const name of contract.ids) {
      if (!previewNames.has(name)) {
        failures.push(`强调色 ${name}: 缺预览色板令牌 ${PREVIEW_PREFIX}${name}`);
      }
      // 默认色没有覆盖块（缺属性即默认），其余每个预设都必须有明暗两套。
      if (name !== contract.fallback && !accentVariants.has(name)) {
        failures.push(`强调色 ${name}: 缺 data-accent 覆盖块`);
      }
    }

    for (const name of previewNames) {
      if (!idSet.has(name)) {
        failures.push(`预览色板 ${PREVIEW_PREFIX}${name} 不在 theme.ts 的 ACCENT_IDS 里`);
      }
    }

    for (const name of accentVariants.keys()) {
      if (!idSet.has(name)) {
        failures.push(`data-accent="${name}" 不在 theme.ts 的 ACCENT_IDS 里`);
      }
    }
  }

  // ---- 收尾 ----------------------------------------------------------
  console.log("设计令牌（tokens.css）");
  console.log(`  令牌数                  亮 ${vars.light.size} / 暗 ${vars.dark.size}`);
  console.log(`  强调色预设              ${accentVariants.size + 1}（含默认）`);

  for (const item of summary) {
    console.log(`  ${item.mode.padEnd(8)} 最低正文对比度 ${item.worst.toFixed(2)}`);
  }

  if (failures.length > 0) {
    console.error(`\nTOKEN MISMATCH (${failures.length} 项):`);
    for (const failure of failures) console.error(`  - ${failure}`);
    process.exit(1);
  }

  console.log("\nOK: 令牌成对、data-theme 只有明暗、涨跌色相合法、正文对比度过 AA、强调色预设合法");
}

main();
