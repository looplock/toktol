#!/usr/bin/env node
/**
 * 校验跨语言契约：api.ts 的类型 ↔ 三 crate 的 serde 结构逐字段对齐
 * （字段名、类型粗归一、tagged 枚举变体），两侧各自的单测发现不了"一边改了另一边没改"。
 * 括号配平文本解析，只覆盖仓库用到的 serde 子集（rename_all / rename_all_fields /
 * rename / tag / skip_serializing_if）。已知盲区：TS 把 Rust string 收窄成字面量
 * 联合是单向放行，数值宽度不查——由 tsc 与运行时测试兜底。
 */

import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

/** TS 名 → Rust 名的镜像清单（rust 缺省与 ts 同名）。rust 在全仓唯一，重名即失败。 */
const PARITY = [
  // 扫描与用量
  { ts: "UsageTotals" },
  { ts: "ModelUsageRow" },
  { ts: "ScanReport" },
  { ts: "ScanBacklog" },
  { ts: "UsageFilters" },
  { ts: "UsageRecordsQuery" },
  { ts: "UsageRecordRow" },
  { ts: "UsageRecordsPage" },
  { ts: "UsageFilterOption" },
  { ts: "UsageFilterOptionsPayload" },
  // 总览仪表盘
  { ts: "DashboardTrendBucket" },
  { ts: "TrendGrain" },
  { ts: "DashboardModelSeries" },
  { ts: "DashboardBreakdownRow" },
  { ts: "DashboardFlowLink" },
  { ts: "DashboardActivityCell" },
  { ts: "DashboardDailyPoint" },
  { ts: "DashboardSpread" },
  { ts: "DashboardSessionPoint" },
  { ts: "DashboardCostComposition" },
  { ts: "DashboardTotals" },
  { ts: "DashboardPayload" },
  // 会话页与转录
  { ts: "SessionFilters" },
  { ts: "SessionsPageQuery" },
  { ts: "SessionRow" },
  { ts: "SessionsPagePayload", rust: "SessionsPage" },
  { ts: "BatchTrashReport" },
  { ts: "TrashReport" },
  { ts: "TranscriptEntry" },
  { ts: "TranscriptBlock" },
  { ts: "TranscriptPageEntry" },
  { ts: "TranscriptPagePayload" },
  { ts: "TranscriptTurn" },
  { ts: "TranscriptTurnPart" },
  { ts: "TranscriptTurnsPayload" },
  // 网关
  { ts: "UpstreamView" },
  { ts: "MappingView" },
  { ts: "UpstreamInput" },
  { ts: "RouteView" },
  { ts: "ConfigView" },
  { ts: "GatewayStatus" },
  { ts: "GatewayTotals" },
  { ts: "GatewayOverviewPayload" },
  { ts: "GatewayRequestRow" },
  { ts: "GatewayRequestsPage" },
  // 定价
  { ts: "PricingCatalogHint", rust: "CatalogHint" },
  { ts: "PricingVariant" },
  { ts: "PricingModelRow" },
  { ts: "CatalogModelBrief" },
  { ts: "PricingOverviewPayload" },
  { ts: "CatalogSyncPayload" },
  // 工具配置
  { ts: "McpServerView" },
  { ts: "SkillView" },
  { ts: "FileEntry" },
  { ts: "ConfigRoot" },
  { ts: "ToolConfigReport" },
  { ts: "FileContent" },
  // 壳与事件载荷
  { ts: "ShellConfig" },
  { ts: "ScanFinishedPayload" },
  { ts: "TranscriptProgressPayload" },
  // ModelPriceInput 不在清单：纯前端打包（set_model_price 的 Rust 命令收独立参数）。
];

const RUST_ROOTS = ["crates/toktol-core/src", "crates/toktol-gateway/src", "src-tauri/src"];
const TS_API = "src/lib/api.ts";

const PRIMITIVES = new Map([
  ["String", "string"],
  ["str", "string"],
  ["char", "string"],
  ["bool", "boolean"],
  ...["u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64"].map(
    (t) => [t, "number"],
  ),
]);

// ── 通用文本工具 ────────────────────────────────────────────

/** 去行注释（在字符串字面量外）；Rust/TS 通用的保守实现。 */
function stripLineComment(line) {
  let out = "";
  let inString = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (c === '"' && line[i - 1] !== "\\") inString = !inString;
    if (!inString && c === "/" && line[i + 1] === "/") break;
    out += c;
  }
  return out;
}

/** 括号净深度（() [] {} <> 都计；仅用于类型/属性文本，无比较运算符场景）。 */
function bracketDepth(text) {
  const pairs = { "(": ")", "[": "]", "{": "}", "<": ">" };
  const close = new Set(Object.values(pairs));
  let depth = 0;
  let inString = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"' && text[i - 1] !== "\\") inString = !inString;
    if (inString) continue;
    if (pairs[c] !== undefined) depth++;
    else if (close.has(c)) depth--;
  }
  return depth;
}

/**
 * 按顶层分隔符切分（括号/字符串内的分隔符不算）。
 * singleQuotes：是否把 ' 视为字符串定界（TS 需要；Rust 类型文本有生命周期 'a，不能开）。
 */
function splitTopLevel(text, sep, { singleQuotes = false } = {}) {
  const parts = [];
  let depth = 0;
  let buf = "";
  let quote = null;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    const prev = text[i - 1];
    if (quote !== null) {
      buf += c;
      if (c === quote && prev !== "\\") quote = null;
      continue;
    }
    if ((c === '"' || (singleQuotes && c === "'")) && prev !== "\\") {
      quote = c;
      buf += c;
      continue;
    }
    if ("([{<".includes(c)) depth++;
    else if (")]}".includes(c) || (c === ">" && depth > 0)) depth--;
    if (depth === 0 && sep.includes(c)) {
      parts.push(buf);
      buf = "";
      continue;
    }
    buf += c;
  }
  if (buf.trim() !== "") parts.push(buf);
  return parts.map((s) => s.trim());
}

/** 与 splitTopLevel 配套的括号配平：返回与 open 在 text 中匹配的收括号下标。 */
function matchBracket(text, openIdx) {
  const open = text[openIdx];
  const close = { "(": ")", "[": "]", "{": "}" }[open];
  let depth = 0;
  let inString = false;
  for (let i = openIdx; i < text.length; i++) {
    const c = text[i];
    if (c === '"' && text[i - 1] !== "\\") inString = !inString;
    if (inString) continue;
    if (c === open) depth++;
    else if (c === close) {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

/** serde camelCase：snake 与 PascalCase 变体名都归一到 camelCase。 */
const camelCase = (s) =>
  s
    .replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase())
    .replace(/^[A-Z]/, (c) => c.toLowerCase());

// ── Rust 侧解析 ─────────────────────────────────────────────

function listRustFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listRustFiles(p));
    else if (entry.name.endsWith(".rs")) out.push(p);
  }
  return out;
}

/** 解析 serde 属性块（可能多行、可能多条属性）。 */
function parseSerdeAttrs(attrTexts) {
  const serde = {
    rename: undefined,
    renameAll: undefined,
    renameAllFields: undefined,
    tag: undefined,
    skip: false,
    skipSerializingIf: false,
    flatten: false,
  };
  for (const text of attrTexts) {
    const single = text.replace(/\s+/g, " ");
    serde.rename ??= /(?<![a-zA-Z_])rename\s*=\s*"([^"]+)"/.exec(single)?.[1];
    serde.renameAll ??= /rename_all\s*=\s*"([^"]+)"/.exec(single)?.[1];
    serde.renameAllFields ??= /rename_all_fields\s*=\s*"([^"]+)"/.exec(single)?.[1];
    serde.tag ??= /(?<![a-zA-Z_])tag\s*=\s*"([^"]+)"/.exec(single)?.[1];
    if (/(?<![a-zA-Z_])skip_serializing_if\b/.test(single)) serde.skipSerializingIf = true;
    if (/(?<![a-zA-Z_])skip(?![a-zA-Z_])/.test(single)) serde.skip = true;
    if (/(?<![a-zA-Z_])flatten(?![a-zA-Z_])/.test(single)) serde.flatten = true;
  }
  return serde;
}

/** 从条目前取走连续的 #[...] 属性（配平取括号，支持多行）。 */
function takeLeadingAttrs(text) {
  const attrs = [];
  let rest = text.trim();
  while (rest.startsWith("#[")) {
    const close = matchBracket(rest, 1);
    if (close === -1) break;
    attrs.push(rest.slice(0, close + 1));
    rest = rest.slice(close + 1).trim();
  }
  return { attrs, rest };
}

/** 解析结构体 / 结构体变体的字段体。 */
function parseRustFields(body, file, typeName) {
  const fields = [];
  for (const piece of splitTopLevel(body, [",", ";"])) {
    if (piece === "") continue;
    const { attrs, rest } = takeLeadingAttrs(piece);
    const m = /^(?:pub(?:\([^)]*\))?\s+)?([a-z_][a-z0-9_]*)\s*:\s*([\s\S]+)$/.exec(rest.trim());
    if (m === null) {
      throw new Error(`${file}: ${typeName} 里出现无法解析的字段：${piece.slice(0, 80)}`);
    }
    const serde = parseSerdeAttrs(attrs);
    if (serde.flatten) {
      throw new Error(`${file}: ${typeName}.${m[1]} 用了 serde(flatten)，脚本不支持——请扩展或改建模`);
    }
    fields.push({ name: m[1], type: m[2].trim(), serde });
  }
  return fields;
}

/** 解析枚举变体（unit / tuple / struct 三态）。lenient 时形状不支持返回 null 而不抛错。 */
function parseRustVariants(body, file, typeName, { lenient = false } = {}) {
  const variants = [];
  for (const piece of splitTopLevel(body, [","])) {
    if (piece === "") continue;
    const { attrs, rest } = takeLeadingAttrs(piece);
    const serde = parseSerdeAttrs(attrs);
    const m = /^([A-Z][a-zA-Z0-9_]*)\s*(?:\(([\s\S]+)\)|\{([\s\S]*)\})?$/.exec(rest.trim());
    if (m === null) {
      if (lenient) return null;
      throw new Error(`${file}: ${typeName} 里出现无法解析的变体：${piece.slice(0, 80)}`);
    }
    if (m[2] !== undefined) {
      if (lenient) return null;
      throw new Error(`${file}: ${typeName}::${m[1]} 是 tuple 变体，脚本不支持——请扩展或改建模`);
    }
    variants.push({
      name: m[1],
      serde,
      fields: m[3] !== undefined ? parseRustFields(m[3], file, `${typeName}::${m[1]}`) : [],
      isStructVariant: m[3] !== undefined,
    });
  }
  return variants;
}

function collectRustTypes() {
  const types = new Map(); // 名字 → 定义
  const neededNames = new Set(PARITY.map((e) => e.rust ?? e.ts));
  for (const root of RUST_ROOTS) {
    for (const file of listRustFiles(join(ROOT, root))) {
      const lines = readFileSync(file, "utf8").split("\n");
      let pendingAttrs = [];
      for (let i = 0; i < lines.length; i++) {
        const stripped = stripLineComment(lines[i]);
        const trimmed = stripped.trim();
        if (trimmed === "") continue;
        if (trimmed.startsWith("#[")) {
          let buf = trimmed;
          while (bracketDepth(buf) > 0 && i + 1 < lines.length) {
            i++;
            buf += ` ${stripLineComment(lines[i]).trim()}`;
          }
          pendingAttrs.push(buf);
          continue;
        }
        const m = /^(?:pub(?:\([^)]*\))?\s+)?(struct|enum)\s+([A-Za-z_][a-zA-Z0-9_]*)/.exec(trimmed);
        if (m === null) {
          pendingAttrs = [];
          continue;
        }
        const [, kind, name] = m;
        // 找到定义后的第一个 `{`（泛型参数之间没有 `{`，本仓库无 where 子句）。
        let searchLine = i;
        while (!stripLineComment(lines[searchLine]).includes("{") && searchLine + 1 < lines.length) {
          searchLine++;
        }
        const openText = stripLineComment(lines[searchLine]);
        const openRel = openText.indexOf("{");
        const textFromOpen = openText.slice(openRel);
        // 逐行配平取 body
        let depth = 0;
        let body = "";
        let endLine = searchLine;
        let bodyDone = false;
        for (let j = searchLine; j < lines.length && !bodyDone; j++) {
          const lineText = j === searchLine ? textFromOpen : stripLineComment(lines[j]);
          for (let k = 0; k < lineText.length; k++) {
            const c = lineText[k];
            if (c === "{") {
              depth++;
              if (depth > 1) body += c; // 外层开括号不入 body
            } else if (c === "}") {
              depth--;
              if (depth === 0) {
                bodyDone = true;
                break;
              }
              body += c;
            } else if (depth > 0) {
              body += c;
            }
          }
          if (!bodyDone) body += "\n";
          endLine = j;
        }
        if (!bodyDone) {
          throw new Error(`${file}: ${name} 的 body 没配平`);
        }
        const existing = types.get(name);
        if (existing !== undefined) {
          // 只有清单内的名字要求全仓唯一；adapter 内部类型跨适配器重名无妨。
          if (neededNames.has(name)) {
            throw new Error(`Rust 类型重名：${name} 同时在 ${existing.file}:${existing.line} 与 ${file}:${i + 1}`);
          }
        } else {
          types.set(name, {
            kind,
            name,
            file,
            line: i + 1,
            attrs: pendingAttrs,
            body,
          });
        }
        pendingAttrs = [];
        i = endLine;
      }
    }
  }
  return types;
}

// ── TS 侧解析 ───────────────────────────────────────────────

function parseTsTypes(text) {
  const types = new Map();
  const noComments = text.replace(/\/\*[\s\S]*?\*\//g, "");
  const ifaceRe = /export\s+interface\s+([A-Za-z_]\w*)/g;
  let m;
  while ((m = ifaceRe.exec(noComments)) !== null) {
    const open = noComments.indexOf("{", m.index + m[0].length);
    const close = matchBracket(noComments, open);
    if (close === -1) throw new Error(`api.ts: interface ${m[1]} 花括号没配平`);
    types.set(m[1], { kind: "interface", body: noComments.slice(open + 1, close), line: noComments.slice(0, m.index).split("\n").length });
  }
  const typeRe = /export\s+type\s+([A-Za-z_]\w*)\s*=\s*/g;
  while ((m = typeRe.exec(noComments)) !== null) {
    let depth = 0;
    let end = -1;
    for (let i = m.index + m[0].length; i < noComments.length; i++) {
      const c = noComments[i];
      if (c === '"' && noComments[i - 1] !== "\\") {
        // 跳过字符串字面量（里面的 ; 不算结束）
        let j = i + 1;
        while (j < noComments.length && !(noComments[j] === '"' && noComments[j - 1] !== "\\")) j++;
        i = j;
        continue;
      }
      if ("([{".includes(c)) depth++;
      else if (")]}" .includes(c)) depth--;
      else if (c === ";" && depth === 0) {
        end = i;
        break;
      }
    }
    if (end === -1) throw new Error(`api.ts: type ${m[1]} 找不到收尾分号`);
    types.set(m[1], { kind: "alias", body: noComments.slice(m.index + m[0].length, end).trim(), line: noComments.slice(0, m.index).split("\n").length });
  }
  return types;
}

function parseTsFields(body) {
  const fields = [];
  for (const piece of splitTopLevel(body, [";", ","], { singleQuotes: true })) {
    if (piece === "") continue;
    const m = /^(?:readonly\s+)?([A-Za-z_]\w*)(\?)?\s*:\s*([\s\S]+)$/.exec(piece.trim());
    if (m === null) throw new Error(`api.ts: 无法解析的字段片段：${piece.slice(0, 80)}`);
    fields.push({ name: m[1], optional: m[2] !== undefined, type: m[3].trim() });
  }
  return fields;
}

/** 字面量联合："a" | "b"；否则 null。 */
function literalUnionOf(tsType) {
  const parts = tsType.trim().split("|").map((s) => s.trim());
  if (parts.length === 0 || !parts.every((s) => /^"[^"]*"$/.test(s))) return null;
  return parts.map((s) => s.slice(1, -1));
}

// ── 归一化与比较 ────────────────────────────────────────────

/** Rust 类型 → TS 归一形态。unitEnums：unit 枚举名 → 字面量联合字符串。 */
function normRust(type, unitEnums) {
  let t = type.trim().replace(/\s+/g, " ");
  if (t.startsWith("&")) t = t.replace(/^&\s*(?:'[^']+\s+)?/, "");
  if (t.startsWith("Option<") && t.endsWith(">")) return `${normRust(t.slice(7, -1), unitEnums)} | null`;
  if (t.startsWith("Vec<") && t.endsWith(">")) return `${normRust(t.slice(4, -1), unitEnums)}[]`;
  const map = /^(?:HashMap|BTreeMap)<\s*String\s*,\s*([\s\S]+)>$/.exec(t);
  if (map !== null) return `Record<string, ${normRust(map[1], unitEnums)}>`;
  if (PRIMITIVES.has(t)) return PRIMITIVES.get(t);
  if (t.includes("::")) t = t.split("::").pop();
  if (RUST_TO_TS_NAME.has(t)) t = RUST_TO_TS_NAME.get(t);
  if (unitEnums.has(t)) return unitEnums.get(t);
  return t;
}

const normTs = (type) => type.replace(/\breadonly\s+/g, "").replace(/\s+/g, " ").trim();

function unitEnumLiterals(def) {
  const serde = parseSerdeAttrs(def.attrs);
  const variants = parseRustVariants(def.body, def.file, def.name, { lenient: true });
  if (variants === null || variants.length === 0 || variants.some((v) => v.isStructVariant)) {
    return null;
  }
  return variants
    .map((v) => v.serde.rename ?? (serde.renameAll === "camelCase" ? camelCase(v.name) : serde.renameAll === "lowercase" ? v.name.toLowerCase() : v.name))
    .sort()
    .map((s) => `"${s}"`)
    .join(" | ");
}

/** 字面量联合 → 排序后的字面量集合串；不是纯字面量联合返回 null。 */
function literalSetOf(tsType) {
  const lits = literalUnionOf(tsType);
  return lits === null ? null : lits.slice().sort().join(",");
}

function fieldNames(rustFields, containerSerde, isVariantField) {
  const renameAll = isVariantField ? containerSerde.renameAllFields : containerSerde.renameAll;
  const out = new Map();
  for (const f of rustFields) {
    if (f.serde.skip) continue;
    const name = f.serde.rename ?? (renameAll === "camelCase" ? camelCase(f.name) : f.name);
    out.set(name, f);
  }
  return out;
}

/** 清单里 TS 名 → Rust 名不同时，类型引用也要跟着换名（如 CatalogHint ↔ PricingCatalogHint）。 */
const RUST_TO_TS_NAME = new Map(PARITY.filter((e) => e.rust !== undefined).map((e) => [e.rust, e.ts]));

/** 单字段类型比较。返回 null = 通过，否则错误描述。 */
function compareFieldType(rustField, tsField, unitEnums, ctx, tsTypes) {
  let r = normRust(rustField.type, unitEnums);
  let t = normTs(tsField.type);
  // TS 命名别名展开为字面量联合（与 Rust unit 枚举的归一形态对齐）。
  const named = /^[A-Z][a-zA-Z0-9_]*$/.exec(t);
  if (named !== null) {
    const alias = tsTypes.get(t);
    if (alias !== undefined && alias.kind === "alias") {
      const lits = literalUnionOf(normTs(alias.body));
      if (lits !== null) t = lits.slice().sort().map((s) => `"${s}"`).join(" | ");
    }
  }
  if (tsField.optional) {
    if (!rustField.serde.skipSerializingIf) {
      return `${ctx}: TS 侧 ${tsField.name} 是可选（?），但 Rust 侧不是 Option + skip_serializing_if（线上恒出现该键）`;
    }
    if (!r.endsWith(" | null")) {
      return `${ctx}: TS 侧 ${tsField.name} 可选，但 Rust 侧 ${rustField.type} 不是 Option`;
    }
    r = r.slice(0, -" | null".length);
  }
  if (r === t) return null;
  // 字面量联合按集合比较（顺序无关）。
  const rLits = literalSetOf(r);
  const tLits = literalSetOf(t);
  if (rLits !== null && tLits !== null) {
    return rLits === tLits ? null : `${ctx}: 字段 ${tsField.name} 字面量集合不齐 —— Rust ${rLits} vs TS ${tLits}`;
  }
  // 单向放行：Rust 线上是自由字符串、TS 自我收窄为字面量联合（合法细化）。
  if (r === "string" && tLits !== null) return null;
  return `${ctx}: 字段 ${tsField.name} 类型不齐 —— Rust「${rustField.type}」(→ ${r}) vs TS「${tsField.type}」`;
}

function compareStructWithInterface(entry, rustDef, tsDef, unitEnums, tsTypes, errors) {
  const serde = parseSerdeAttrs(rustDef.attrs);
  const rustFields = parseRustFields(rustDef.body, rustDef.file, rustDef.name);
  const tsFields = parseTsFields(tsDef.body);
  const rustByName = fieldNames(rustFields, serde, false);
  const tsByName = new Map(tsFields.map((f) => [f.name, f]));
  for (const [name, rf] of rustByName) {
    const tf = tsByName.get(name);
    if (tf === undefined) {
      errors.push(`${entry.ts}: TS 侧缺字段 ${name}（Rust ${rustDef.file}:${rustDef.line}）`);
      continue;
    }
    const err = compareFieldType(rf, tf, unitEnums, entry.ts, tsTypes);
    if (err !== null) errors.push(err);
  }
  for (const name of tsByName.keys()) {
    if (!rustByName.has(name)) {
      errors.push(`${entry.ts}: TS 侧多出字段 ${name}（Rust ${rustDef.name} 没有它）`);
    }
  }
}

function compareUnitEnumWithAlias(entry, rustDef, tsDef, errors) {
  const rustLits = unitEnumLiterals(rustDef);
  const tsLits = literalUnionOf(normTs(tsDef.body));
  if (rustLits === null || tsLits === null) {
    errors.push(`${entry.ts}: ${rustDef.kind === "enum" ? "Rust 枚举" : "TS 别名"}形状不符合 unit 枚举 ↔ 字面量联合`);
    return;
  }
  const r = literalSetOf(rustLits);
  const t = literalSetOf(normTs(tsDef.body));
  if (r !== t) {
    errors.push(`${entry.ts}: 字面量集合不齐 —— Rust ${r} vs TS ${t}`);
  }
}

function compareTaggedEnumWithAlias(entry, rustDef, tsDef, unitEnums, tsTypes, errors) {
  const serde = parseSerdeAttrs(rustDef.attrs);
  const tag = serde.tag;
  if (tag === undefined) {
    errors.push(`${entry.ts}: Rust 枚举没有 tag，无法与对象联合比对`);
    return;
  }
  const variants = parseRustVariants(rustDef.body, rustDef.file, rustDef.name);
  const tsMembers = splitTopLevel(tsDef.body, ["|"], { singleQuotes: true }).filter((s) => s !== "");
  const memberByTag = new Map();
  for (const member of tsMembers) {
    if (!member.startsWith("{")) {
      errors.push(`${entry.ts}: TS 联合成员不是对象字面量：${member.slice(0, 60)}`);
      continue;
    }
    const close = matchBracket(member, 0);
    const fields = parseTsFields(member.slice(1, close));
    const tagField = fields.find((f) => f.name === tag);
    const lit = tagField === undefined ? null : literalUnionOf(normTs(tagField.type));
    if (lit === null || lit.length !== 1) {
      errors.push(`${entry.ts}: 联合成员缺 ${tag} 字面量：${member.slice(0, 60)}`);
      continue;
    }
    memberByTag.set(lit[0], fields.filter((f) => f.name !== tag));
  }
  const variantByTag = new Map();
  for (const v of variants) {
    const tagValue = v.serde.rename ?? (serde.renameAll === "camelCase" ? camelCase(v.name) : v.name);
    variantByTag.set(tagValue, v);
  }
  for (const [tagValue, v] of variantByTag) {
    const tsFields = memberByTag.get(tagValue);
    if (tsFields === undefined) {
      errors.push(`${entry.ts}: TS 侧缺变体 ${tag}="${tagValue}"`);
      continue;
    }
    if (!v.isStructVariant) {
      if (tsFields.length > 0) {
        errors.push(`${entry.ts}: 变体 "${tagValue}" 在 Rust 是 unit、TS 侧却有字段`);
      }
      continue;
    }
    const rustByName = fieldNames(v.fields, serde, true);
    const tsByName = new Map(tsFields.map((f) => [f.name, f]));
    for (const [name, rf] of rustByName) {
      const tf = tsByName.get(name);
      if (tf === undefined) {
        errors.push(`${entry.ts}: 变体 "${tagValue}" TS 侧缺字段 ${name}`);
        continue;
      }
      const err = compareFieldType(rf, tf, unitEnums, entry.ts, tsTypes);
      if (err !== null) errors.push(err);
    }
    for (const name of tsByName.keys()) {
      if (!rustByName.has(name)) {
        errors.push(`${entry.ts}: 变体 "${tagValue}" TS 侧多出字段 ${name}`);
      }
    }
  }
  for (const tagValue of memberByTag.keys()) {
    if (!variantByTag.has(tagValue)) {
      errors.push(`${entry.ts}: TS 侧多出变体 ${tag}="${tagValue}"（Rust 侧没有）`);
    }
  }
}

// ── 主流程 ──────────────────────────────────────────────────

function main() {
  const errors = [];
  const rustTypes = collectRustTypes();
  const tsText = readFileSync(join(ROOT, TS_API), "utf8");
  const tsTypes = parseTsTypes(tsText);

  // unit 枚举索引（供字段类型归一化解析成字面量联合）。
  const unitEnums = new Map();
  for (const def of rustTypes.values()) {
    if (def.kind !== "enum") continue;
    const lits = unitEnumLiterals(def);
    if (lits !== null) unitEnums.set(def.name, lits);
  }

  for (const entry of PARITY) {
    const rustName = entry.rust ?? entry.ts;
    const rustDef = rustTypes.get(rustName);
    const tsDef = tsTypes.get(entry.ts);
    if (rustDef === undefined) {
      errors.push(`Rust 侧找不到类型 ${rustName}（清单项 ${entry.ts}）`);
      continue;
    }
    if (tsDef === undefined) {
      errors.push(`${TS_API} 里找不到 export 的 ${entry.ts}`);
      continue;
    }
    if (rustDef.kind === "struct" && tsDef.kind === "interface") {
      compareStructWithInterface(entry, rustDef, tsDef, unitEnums, tsTypes, errors);
    } else if (rustDef.kind === "enum" && tsDef.kind === "alias") {
      const serde = parseSerdeAttrs(rustDef.attrs);
      if (serde.tag !== undefined) {
        compareTaggedEnumWithAlias(entry, rustDef, tsDef, unitEnums, tsTypes, errors);
      } else compareUnitEnumWithAlias(entry, rustDef, tsDef, errors);
    } else {
      errors.push(
        `${entry.ts}: 形状不匹配 —— Rust ${rustDef.kind} vs TS ${tsDef.kind}（结构体应对 interface、枚举应对 type 别名）`,
      );
    }
  }

  if (errors.length > 0) {
    console.error(`api-parity 校验失败（${errors.length} 处）：\n`);
    for (const e of errors) console.error(`  ✗ ${e}`);
    console.error(
      `\napi.ts 与 Rust serde 结构已漂移：先改源头（Rust 结构体或 api.ts 镜像），保持两侧一致。`,
    );
    process.exit(1);
  }
  console.log(`api-parity: ${PARITY.length} 个类型逐字段校验通过`);
}

main();
