//! workbuddy 适配器：解析 `~/.workbuddy/projects/**/*.jsonl` 会话日志。
//! 每行自带 `sessionId`，逐行解析即可。同级的 `credentials/`、`keyblob`、
//! `settings.yaml`、`mcp.json` 等配置与凭据不在扫描范围，绝不打开。
//!
//! 项目目录：日志行不带 cwd，但日志按项目分目录存放——目录名是编码后的项目
//! 路径（`d-Work-Demo-App` → `D:\Work\Demo\App`，
//! 见 [`decode_project_dir`]），从来源文件路径解码。
//!
//! 格式（实测）：`type:"message"`（顶层 role/content）与 `type:"function_call"`
//! （`message` 字段只装 usage）两类行携带用量；模型在 `providerData.model`。
//! 标题有两个来源：`type:"ai-title"` 行的 `aiTitle`（应用自己生成的会话标题），
//! 以及 user message 文本里 `<user_query>` 标签包裹的真实提问——user 行的
//! text 块以 harness 注入的 `<system-reminder>` 上下文开头，真实提问缀在末尾
//! 的标签里，直接取首文本会拿到一大段注入上下文。转录展示同口径：user 文本
//! 块分段（[`segment_user_text`]），注入段归 Injected 块、由前端按需呈现。
//! 同一请求会以两种风格各记
//! 一次、数字相同：
//! snake（`input_tokens`/`cache_read_input_tokens`）与 camel
//! （`inputTokens`/`inputTokensDetails[].cached_tokens`/`outputTokensDetails[].reasoning_tokens`）。
//! 两种风格互证出**同一口径**：`input_tokens` 含缓存读、`output_tokens` 含推理
//! （total = input + output 恒成立）。所以入库前要拆分：输入净量 = input − 缓存读，
//! 输出净量 = output − 推理——否则缓存读会按输入价二次计费。
//!
//! 耗时是推导值（日志不带原生计时）：usage 行时间戳是响应完成时刻，请求在上一个
//! 边界事件（上一 `function_call_result` 回写 / user message 提交）后立刻发出，
//! 差值即耗时。口径恒为高估：含 harness 开销（毫秒级），人工确认等待会算进下一
//! 请求。跨行状态由 [`Adapter::parse_file_at`] 的前缀重建兜住增量续扫的确定性。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Deserialize;

use super::transcript::{self, TranscriptBlock, TranscriptEntry, TranscriptRole};
use super::{Adapter, LineFacts, LineParse, UsageFacts};
use crate::error::Result;
use crate::model::{TokenUsage, Tool};

/// 会话标题取 ai-title 行或 user message 里的 `<user_query>` 提问，超长截断。
const TITLE_MAX_CHARS: usize = 200;

/// workbuddy 的适配器实现；耗时推导需要跨行边界状态，由 [`Adapter::parse_file_at`]
/// 文件级维护（单行契约的 [`Adapter::parse_line`] 恒无耗时）。`decoded` 缓存目录名→
/// 项目路径的解码结果（FS 验证有 stat 成本，同一编码只解一次）——`adapters()` 每轮
/// 新建适配器，缓存不跨轮。
pub struct WorkBuddyAdapter {
    pub(crate) decoded: Mutex<HashMap<String, Option<String>>>,
}

impl Adapter for WorkBuddyAdapter {
    fn tool(&self) -> Tool {
        Tool::WorkBuddy
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        match crate::paths::home_dir() {
            Some(home) => vec![home.join(".workbuddy").join("projects")],
            None => vec![],
        }
    }

    /// `.jsonl` 是会话日志；同目录的 `.meta.json`、`.file-rollback.ndjson` 不匹配。
    fn is_session_log(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
    }

    /// `.jsonl` + 同名的 `.meta.json` 与 `.file-rollback.ndjson`。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        let mut artifacts = vec![source_file.to_path_buf()];
        if let (Some(dir), Some(stem)) = (
            source_file.parent(),
            source_file.file_stem().and_then(|s| s.to_str()),
        ) {
            artifacts.push(dir.join(format!("{stem}.meta.json")));
            artifacts.push(dir.join(format!("{stem}.file-rollback.ndjson")));
        }
        artifacts
    }

    /// 单行契约：无跨行上下文，耗时不可推导，恒 `None`。文件级入口在
    /// [`Self::parse_file_at`]。
    fn parse_line(&self, line: &str) -> LineParse {
        parse_line_full(line, &mut None)
    }

    /// 项目目录编码在日志的存放目录名里（行内不带）：解析前先解码一次，
    /// 把本文件产出的全部事实都补上归属。耗时依赖最近边界事件（见
    /// [`parse_line_full`]）：前缀行先重建边界状态，再随逐行解析推进——
    /// 增量续扫的确定性由此成立（同一文件内容 + 同一 start ⇒ 同一结果）。
    fn parse_file_at(&self, source_file: &Path, lines: &[&str], start: usize) -> Vec<LineParse> {
        let project_dir = self.project_dir_of(source_file);
        let mut boundary = prefix_boundary_ts(&lines[..start]);
        lines[start..]
            .iter()
            .map(|line| {
                let parsed = parse_line_full(line, &mut boundary);
                match (parsed, project_dir.as_ref()) {
                    (LineParse::Facts(mut facts), Some(dir)) => {
                        facts.project_dir = Some(dir.clone());
                        LineParse::Facts(facts)
                    }
                    (parsed, _) => parsed,
                }
            })
            .collect()
    }

    /// 每行自带 sessionId：只保留目标会话的 message 行与 function_call 行。
    /// function_call 行的模型在 providerData（与用量行同一来源）。
    fn read_transcript(
        &self,
        source_file: &Path,
        external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let text = transcript::read_text(source_file)?;
        let mut entries = Vec::new();
        for line in text.lines() {
            let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
                continue;
            };
            if raw
                .session_id
                .as_ref()
                .is_none_or(|id| id.trim() != external_id)
            {
                continue;
            }
            match raw.kind.as_str() {
                "message" => {
                    let role = match raw.role.as_deref() {
                        Some("user") => TranscriptRole::User,
                        Some("assistant") => TranscriptRole::Assistant,
                        Some("system") => TranscriptRole::System,
                        _ => continue,
                    };
                    let blocks = raw
                        .content
                        .as_ref()
                        .map(transcript::blocks_from_content)
                        .unwrap_or_default();
                    // user 文本块先分段（注入归 Injected、正文留 Text）再重排
                    // 附件：@image 提及在提问正文里，分段后提及仍在，重排不受
                    // 影响；反过来先重排会把注入段切碎、剥不干净。
                    let blocks = if role == TranscriptRole::User {
                        blocks
                            .into_iter()
                            .flat_map(|block| match block {
                                TranscriptBlock::Text { text } => segment_user_text(&text),
                                kept => vec![kept],
                            })
                            .collect()
                    } else {
                        blocks
                    };
                    // 附件块在日志里统一记在消息尾部：按正文里的 @image#N 提及
                    // 重排回用户粘贴的位置。
                    let blocks = transcript::interleave_image_mentions(blocks);
                    if blocks.is_empty() {
                        continue;
                    }
                    entries.push(TranscriptEntry {
                        role,
                        ts_ms: raw.timestamp,
                        model: None,
                        blocks,
                    });
                }
                "function_call" => {
                    if raw.name.is_none() && raw.arguments.is_none() {
                        continue;
                    }
                    entries.push(TranscriptEntry {
                        role: TranscriptRole::Assistant,
                        ts_ms: raw.timestamp,
                        model: raw
                            .provider_data
                            .as_ref()
                            .and_then(|pd| pd.model.clone())
                            .filter(|m| !m.trim().is_empty()),
                        blocks: vec![TranscriptBlock::ToolCall {
                            id: raw.id.clone(),
                            name: raw.name.clone(),
                            arguments: raw.arguments.as_ref().map(transcript::arguments_text),
                        }],
                    });
                }
                _ => {}
            }
        }
        // 压缩摘要后的固定"继续对话"指令并入摘要条目：同一事件（时间戳
        // 相差毫秒级），不该在时间线上单独占一个用户气泡、也不进目录。
        // 没有前导摘要可并时原样保留。
        let mut merged: Vec<TranscriptEntry> = Vec::with_capacity(entries.len());
        for entry in entries {
            let is_continue = entry.blocks.len() == 1
                && matches!(entry.blocks[0], TranscriptBlock::ContinueNotice);
            let mut absorbed = false;
            if is_continue
                && let Some(last) = merged.last_mut()
                && let Some(TranscriptBlock::HistorySummary { has_continue, .. }) =
                    last.blocks.last_mut()
            {
                *has_continue = true;
                absorbed = true;
            }
            if !absorbed {
                merged.push(entry);
            }
        }
        Ok(merged)
    }
}

impl WorkBuddyAdapter {
    /// 日志文件 → 项目目录：取存放目录（`projects/<编码路径>/`）的目录名解码。
    fn project_dir_of(&self, source_file: &Path) -> Option<String> {
        let encoded = source_file.parent()?.file_name()?.to_str()?;
        let mut cache = self.decoded.lock().ok()?;
        cache.get(encoded).cloned().unwrap_or_else(|| {
            let decoded = decode_project_dir(encoded);
            cache.insert(encoded.to_string(), decoded.clone());
            decoded
        })
    }
}

/// 边界事件：下一个 LLM 请求可能立刻发出的时刻。`function_call_result` 是上一
/// 工具的结果回写，user message 是用户提交；同一响应组内的 reasoning/message/
/// function_call 行毫秒级连发，绝不能当边界——否则耗时全塌成毫秒。
fn is_boundary_kind(kind: &str, role: Option<&str>) -> bool {
    kind == "function_call_result" || (kind == "message" && role == Some("user"))
}

/// 前缀行 → 最近边界事件时间戳（增量续扫时重建跨行状态）。前缀行不产出事实，
/// 只看 type/role/timestamp 三个字段，避免完整解析（content 的 Value 化）为
/// 多 MB 正文白付费。
fn prefix_boundary_ts(lines: &[&str]) -> Option<i64> {
    let mut boundary = None;
    for line in lines {
        let Ok(raw) = serde_json::from_str::<RawBoundary>(line) else {
            continue;
        };
        if is_boundary_kind(&raw.kind, raw.role.as_deref()) {
            boundary = raw.timestamp;
        }
    }
    boundary
}

/// 单行完整解析，顺带推进边界状态：本行是边界事件则状态更新为本行时间戳，
/// 否则原样保留。耗时的锚是**本行之前**的边界（`*boundary` 取旧值再推进），
/// 不是本行自己。
fn parse_line_full(line: &str, boundary: &mut Option<i64>) -> LineParse {
    let raw: RawLine = match serde_json::from_str(line) {
        Ok(raw) => raw,
        Err(_) => return LineParse::Malformed,
    };
    let prev_boundary = *boundary;
    if is_boundary_kind(&raw.kind, raw.role.as_deref()) {
        *boundary = raw.timestamp;
    }
    parse_raw_line(raw, prev_boundary)
}

fn parse_raw_line(raw: RawLine, prev_boundary: Option<i64>) -> LineParse {
    let Some(session_external_id) = raw
        .session_id
        .filter(|id| !id.trim().is_empty())
        .map(|id| id.trim().to_string())
    else {
        return LineParse::Skip;
    };

    // 标题：ai-title 行是应用自己生成的官方标题；user message 里真实提问
    // 包在 <user_query> 标签中（text 块开头是 harness 注入的上下文）。
    let title = match raw.kind.as_str() {
        "ai-title" => raw.ai_title.clone().filter(|t| !t.trim().is_empty()),
        "message" if raw.role.as_deref() == Some("user") => first_text(&raw.content),
        _ => None,
    }
    .map(truncate_title);

    // 用量挂在 function_call 行的 message 字段里；模型在 providerData。
    let usage = raw
        .message
        .as_ref()
        .and_then(|m| m.usage.as_ref())
        .and_then(|u| {
            let model = raw
                .provider_data
                .as_ref()
                .and_then(|pd| pd.model.as_deref())
                .unwrap_or("");
            usage_facts(u, model, raw.timestamp, prev_boundary)
        });

    if title.is_none() && usage.is_none() {
        return LineParse::Skip;
    }

    LineParse::Facts(Box::new(LineFacts {
        dedup_suffix: None,
        request_count: 1,
        session_external_id,
        title,
        project_dir: None,
        ts_ms: raw.timestamp,
        usage,
    }))
}

/// 编码目录名 → 项目路径。编码规则（实测）：路径分隔符（`\` 与 `/`）替换成
/// `-`，首段是盘符（`d-Work-Demo-App` → `D:\Work\Demo\App`）。
/// 名字里的 `-` 与分隔符不可区分（`Client-Alpha`、时间戳目录），纯文本解码有
/// 歧义——用文件系统消歧：从左到右贪心匹配真实存在的目录，整段（保留 `-`）
/// 优先于切分；验证失败（项目已删、盘不在了）退回朴素解码：全部 `-` 视作
/// 分隔符。退化结果可能不精确，但稳定可读，足够分组与筛选。
fn decode_project_dir(encoded: &str) -> Option<String> {
    let (drive, rest) = encoded.split_once('-')?;
    let letter = drive.chars().next()?;
    if drive.len() != 1 || !letter.is_ascii_alphabetic() {
        return None;
    }
    // 编码用小写盘符，真实路径是大写（`e-…` → `E:\…`），FS 验证与展示都要对上。
    let root = PathBuf::from(format!("{}:\\", letter.to_ascii_uppercase()));
    match decode_segments(rest, &root) {
        Some(path) => Some(path.to_string_lossy().into_owned()),
        None => Some(format!("{}{}", root.display(), rest.replace('-', "\\"))),
    }
}

/// 在 `current` 下匹配 `rest` 的真实目录段：每个 `-` 都是候选切点，最长段
/// （少切分、保留名字里的 `-`）优先；切点验证不过就回溯换短的。
fn decode_segments(rest: &str, current: &Path) -> Option<PathBuf> {
    if rest.is_empty() {
        return Some(current.to_path_buf());
    }
    let mut splits: Vec<usize> = rest.match_indices('-').map(|(i, _)| i).collect();
    splits.push(rest.len()); // 整段作为最后一级目录也参与验证。
    for i in splits.into_iter().rev() {
        let (segment, remainder) = if i == rest.len() {
            (rest, "")
        } else {
            (&rest[..i], &rest[i + 1..])
        };
        if segment.is_empty() {
            continue;
        }
        let candidate = current.join(segment);
        if candidate.is_dir()
            && let Some(done) = decode_segments(remainder, &candidate)
        {
            return Some(done);
        }
    }
    None
}

/// 全零用量无事实价值，跳过；时间戳缺失的用量不落库（schema 非空，绝不编造）。
/// 耗时 = 行时间戳 − 上一个边界事件时间戳；负差值（乱序行）不可信，连同无边界
/// 一起保持 `None`，绝不编造。
fn usage_facts(
    u: &RawUsage,
    model_raw: &str,
    ts_ms: Option<i64>,
    prev_boundary: Option<i64>,
) -> Option<UsageFacts> {
    let model_raw = model_raw.trim();
    if model_raw.is_empty() {
        return None;
    }

    // input 含缓存读（OpenAI 口径）：缓存读 = snake 字段或 camel 明细之和。
    let cache_read = u
        .cache_read_snake
        .unwrap_or_else(|| u.input_details.iter().map(|d| d.cached_tokens).sum())
        .max(0);
    // output 含推理（camel 明细）：推理拆出后按输出价计费。
    let reasoning = u
        .output_details
        .iter()
        .map(|d| d.reasoning_tokens)
        .sum::<i64>();
    let reasoning = (reasoning > 0).then_some(reasoning);

    let usage = TokenUsage {
        input_tokens: (u.input() - cache_read).max(0),
        output_tokens: (u.output() - reasoning.unwrap_or(0)).max(0),
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        reasoning_tokens: reasoning,
    };
    if usage.input_tokens == 0 && usage.output_tokens == 0 && usage.cache_read_tokens == 0 {
        return None;
    }
    Some(UsageFacts {
        ts_ms: ts_ms?,
        model_raw: model_raw.to_string(),
        model: model_raw.to_string(),
        duration_ms: prev_boundary
            .zip(ts_ms)
            .map(|(boundary, ts)| ts - boundary)
            .filter(|ms| *ms >= 0),
        usage,
    })
}

/// user message 文本 → 标题候选。真实提问在 `<user_query>…</user_query>` 标签里
/// （text 块开头是 harness 注入的上下文）；没有标签的老格式回退到非注入文本。
fn first_text(content: &Option<serde_json::Value>) -> Option<String> {
    match content {
        Some(serde_json::Value::String(text)) => title_candidate(text),
        Some(serde_json::Value::Array(blocks)) => blocks.iter().find_map(|block| {
            let obj = block.as_object()?;
            // 用户的文本块是 input_text；兜底兼容通用 text。
            let kind = obj.get("type")?.as_str()?;
            if kind != "input_text" && kind != "text" {
                return None;
            }
            title_candidate(obj.get("text")?.as_str()?)
        }),
        _ => None,
    }
}

fn title_candidate(text: &str) -> Option<String> {
    if let Some(query) = transcript::user_query_of(text) {
        return Some(query);
    }
    // 纯图片附件引用（<image_local_path> 标签）不是提问，不作标题。
    if is_injected_context(text) || transcript::image_local_path_of(text).is_some() {
        return None;
    }
    Some(text.to_string())
}

/// harness 注入的上下文包裹文本：system-reminder（环境/指令注入）与
/// teammate-message（多智能体协作里的同伴消息）。两者都不是用户的提问。
fn is_injected_context(text: &str) -> bool {
    let head = text.trim_start();
    head.starts_with("<system-reminder") || head.starts_with("<teammate-message")
}

/// 系统生成的 user 角色消息：后台任务完成通知与上下文压缩摘要。都不经过
/// reminder/user_query 分段——它们没有用户写的话，前端渲染成系统事件条 /
/// 压缩分隔卡而不是用户气泡。
fn classify_meta_user_text(text: &str) -> Option<TranscriptBlock> {
    if let Some(inner) = tag_wrapped(text, "<task-notification>", "</task-notification>") {
        // 通知体可能带 HTML 实体（&quot; &amp; …），展示字段反转义；
        // 原文不动。解析不到的字段留空，前端有兜底。
        let untagged = |name: &str| field_of(inner, name).map(|value| unescape_entities(&value));
        return Some(TranscriptBlock::TaskNotification {
            task_id: untagged("task-id").unwrap_or_default(),
            status: untagged("status").unwrap_or_default(),
            summary: untagged("summary").unwrap_or_default(),
            text: text.to_string(),
        });
    }
    if let Some(inner) = tag_wrapped(
        text,
        "<conversation_history_summary>",
        "</conversation_history_summary>",
    ) {
        return Some(TranscriptBlock::HistorySummary {
            text: inner.trim().to_string(),
            has_continue: false,
        });
    }
    // 压缩后自动插入的固定模板指令（实测无标签壳、全文即模板）。
    if text.starts_with(CONTINUE_NOTICE_PREFIX) {
        return Some(TranscriptBlock::ContinueNotice);
    }
    None
}

/// 文本以 `head` 开头、其后出现 `tail` 时取两标签之间的内容；`tail` 之后
/// 的附着文本不算（任务通知尾部的指令模板留在原文里）。
fn tag_wrapped<'a>(text: &'a str, head: &str, tail: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(head)?;
    let end = rest.find(tail)?;
    Some(&rest[..end])
}

/// `<name>…</name>` 单层字段提取，取第一处。
fn field_of(text: &str, name: &str) -> Option<String> {
    let head = format!("<{name}>");
    let tail = format!("</{name}>");
    let start = text.find(&head)? + head.len();
    let end = start + text[start..].find(&tail)?;
    Some(text[start..end].to_string())
}

/// 反转义 XML/HTML 基础实体；`&amp;` 最后处理避免二次解码。
fn unescape_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// 压缩后自动插入的"继续对话"指令模板前缀（实测固定英文文案）。
const CONTINUE_NOTICE_PREFIX: &str =
    "Please continue with the conversation based on the summarized context above";

/// user 文本块的展示分段：harness 注入的 `<system-reminder>`（环境/身份/
/// 项目配置，往往整段是配置文件内容）与用户写的话分开建模——有注入时产
/// [`TranscriptBlock::Injected`]，携带**整条消息的完整原文**（前端默认只渲染
/// 正文，完整口径一字不差渲染原文，提问内容就在 `<user_query>` 标签内，
/// 不拆"注入/正文"两截；目录不由此开启轮次）与注入段字符数（占比条分子），
/// 正文进 [`TranscriptBlock::Text`]（有 `<user_query>` 标签取标签内提问，
/// 同伴消息剥掉包裹标签——是真实协作内容）。纯注入消息只产 Injected 块
/// （不再整条丢弃，存在感由前端占比条呈现）。
fn segment_user_text(text: &str) -> Vec<TranscriptBlock> {
    if let Some(block) = classify_meta_user_text(text.trim()) {
        return vec![block];
    }
    let (cleaned, spans) = transcript::split_system_reminders(text);
    if spans.is_empty() {
        // 无注入：不打占比条。有 <user_query> 标签时正文取标签内提问
        // （与标题口径一致，目录摘要不吃标签）；否则正文即清洗后的全文。
        if let Some(query) = transcript::user_query_of(text) {
            if query.trim().is_empty() {
                return vec![];
            }
            return vec![TranscriptBlock::Text { text: query }];
        }
        let cleaned = unwrap_teammate_message(cleaned.trim()).trim();
        if cleaned.is_empty() {
            return vec![];
        }
        return vec![TranscriptBlock::Text {
            text: cleaned.to_string(),
        }];
    }
    let mut blocks = vec![TranscriptBlock::Injected {
        text: text.to_string(),
        injected_chars: spans.iter().map(|s| s.chars().count() as i64).sum(),
    }];
    if let Some(query) = transcript::user_query_of(text) {
        if !query.trim().is_empty() {
            blocks.push(TranscriptBlock::Text { text: query });
        }
    } else {
        let cleaned = unwrap_teammate_message(cleaned.trim()).trim();
        if !cleaned.is_empty() {
            blocks.push(TranscriptBlock::Text {
                text: cleaned.to_string(),
            });
        }
    }
    blocks
}

/// 整段就是一个同伴消息包裹时剥掉 `<teammate-message …>` 标签留正文；
/// 混在更长文本里的标签是正文的一部分，不动。
fn unwrap_teammate_message(text: &str) -> &str {
    const HEAD: &str = "<teammate-message";
    const TAIL: &str = "</teammate-message>";
    let trimmed = text.trim();
    if trimmed.starts_with(HEAD) && trimmed.ends_with(TAIL) {
        match trimmed.find('>') {
            // 属性段到第一个 '>' 为止；没有内层正文按空处理。
            Some(gt) if gt < trimmed.len() - TAIL.len() => {
                trimmed[gt + 1..trimmed.len() - TAIL.len()].trim()
            }
            _ => "",
        }
    } else {
        text
    }
}

fn truncate_title(mut text: String) -> String {
    let trimmed = text.trim();
    if trimmed.len() < text.len() {
        text = trimmed.to_string();
    }
    if text.chars().count() > TITLE_MAX_CHARS {
        text = text.chars().take(TITLE_MAX_CHARS).collect();
    }
    text
}

/// [`prefix_boundary_ts`] 的 slim 输入：前缀扫描只看三个字段，不触碰正文。
#[derive(Deserialize)]
struct RawBoundary {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    timestamp: Option<i64>,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    /// epoch ms。
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    // ── function_call 行的工具字段（用量解析不触碰）。
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<RawMessage>,
    // ── ai-title 行的标题字段。
    #[serde(rename = "aiTitle", default)]
    ai_title: Option<String>,
    #[serde(rename = "providerData", default)]
    provider_data: Option<RawProviderData>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawProviderData {
    #[serde(default)]
    model: Option<String>,
}

/// 两种风格的并集：snake（Anthropic 命名）与 camel（OpenAI 命名）。
/// 同一请求两种风格数字相同，见模块文档的口径结论。
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(rename = "output_tokens", default)]
    output_tokens_snake: Option<i64>,
    #[serde(rename = "cache_read_input_tokens", default)]
    cache_read_snake: Option<i64>,
    #[serde(rename = "inputTokens", default)]
    input_tokens_camel: Option<i64>,
    #[serde(rename = "outputTokens", default)]
    output_tokens_camel: Option<i64>,
    #[serde(rename = "inputTokensDetails", default)]
    input_details: Vec<CacheDetail>,
    #[serde(rename = "outputTokensDetails", default)]
    output_details: Vec<ReasoningDetail>,
}

impl RawUsage {
    fn input(&self) -> i64 {
        self.input_tokens
            .or(self.input_tokens_camel)
            .unwrap_or(0)
            .max(0)
    }

    fn output(&self) -> i64 {
        self.output_tokens_snake
            .or(self.output_tokens_camel)
            .unwrap_or(0)
            .max(0)
    }
}

#[derive(Deserialize)]
struct CacheDetail {
    #[serde(rename = "cached_tokens", default)]
    cached_tokens: i64,
}

#[derive(Deserialize)]
struct ReasoningDetail {
    #[serde(rename = "reasoning_tokens", default)]
    reasoning_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> WorkBuddyAdapter {
        WorkBuddyAdapter {
            decoded: Mutex::new(HashMap::new()),
        }
    }

    fn parse(line: &str) -> LineFacts {
        match adapter().parse_line(line) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        }
    }

    /// 实测样本：function_call 行，message 只装 usage，模型在 providerData。
    const FUNCTION_CALL: &str = r#"{"id":"01a0c959","parentId":"01a0c959","timestamp":1790084494341,"type":"function_call","sessionId":"1a2b3c4d-075f","providerData":{"model":"hy4-preview-f","conversationRequestId":"01a0c959"},"name":"bash","arguments":{},"message":{"usage":{"input_tokens":31828,"output_tokens":292,"total_tokens":32120,"cache_read_input_tokens":14080}}}"#;

    /// camel 风格的同一请求：cached ⊆ input、reasoning ⊆ output。
    const CAMEL_STYLE: &str = r#"{"type":"function_call","sessionId":"1a2b3c4d-075f","timestamp":1790084494341,"providerData":{"model":"hy4-preview-f"},"message":{"usage":{"requests":1,"inputTokens":31828,"outputTokens":292,"totalTokens":32120,"inputTokensDetails":[{"cached_tokens":14080}],"outputTokensDetails":[{"reasoning_tokens":202}]}}}"#;

    #[test]
    fn function_call_line_yields_usage_with_model_from_provider_data() {
        let facts = parse(FUNCTION_CALL);
        assert_eq!(facts.session_external_id, "1a2b3c4d-075f");
        assert_eq!(facts.ts_ms, Some(1_790_084_494_341));
        assert_eq!(facts.title, None);

        let usage = facts.usage.expect("function_call 行必须有用量");
        assert_eq!(usage.model_raw, "hy4-preview-f");
        // input 含缓存读：净输入 = 31828 − 14080。
        assert_eq!(usage.usage.input_tokens, 17_748);
        assert_eq!(usage.usage.cache_read_tokens, 14_080);
        assert_eq!(usage.usage.output_tokens, 292);
        assert_eq!(usage.usage.reasoning_tokens, None);
        // 单行契约无跨行上下文：耗时不可推导。
        assert_eq!(usage.duration_ms, None);
    }

    #[test]
    fn camel_style_splits_cache_and_reasoning_out_of_totals() {
        let facts = parse(CAMEL_STYLE);
        let usage = facts.usage.expect("camel 风格必须有用量");
        assert_eq!(usage.usage.input_tokens, 17_748, "缓存读从 input 里拆出");
        assert_eq!(usage.usage.cache_read_tokens, 14_080);
        assert_eq!(usage.usage.output_tokens, 90, "推理从 output 里拆出");
        assert_eq!(usage.usage.reasoning_tokens, Some(202));
    }

    /// 实测时序还原：响应组内行毫秒级连发，耗时必须锚到上一个边界事件
    /// （上一 function_call_result / user message）而非紧邻行——按"上一事件
    /// 差值"算会全塌成毫秒。轮末 assistant message 的 usage 同口径。
    #[test]
    fn file_parse_derives_duration_from_boundary_events() {
        let lines = [
            r#"{"type":"message","role":"user","sessionId":"s1","timestamp":1000000,"content":[{"type":"input_text","text":"<user_query>问</user_query>"}]}"#,
            r#"{"type":"reasoning","sessionId":"s1","timestamp":1007000,"content":[]}"#,
            r#"{"type":"message","role":"assistant","sessionId":"s1","timestamp":1007030,"content":[]}"#,
            r#"{"type":"function_call","sessionId":"s1","timestamp":1007050,"providerData":{"model":"glm-5.3-flash"},"name":"Bash","arguments":{},"message":{"usage":{"input_tokens":100,"output_tokens":20}}}"#,
            r#"{"type":"function_call_result","sessionId":"s1","timestamp":1007100,"name":"Bash","callId":"c1","status":"completed","output":"ok"}"#,
            r#"{"type":"reasoning","sessionId":"s1","timestamp":1009000,"content":[]}"#,
            r#"{"type":"function_call","sessionId":"s1","timestamp":1009040,"providerData":{"model":"glm-5.3-flash"},"name":"Grep","arguments":{},"message":{"usage":{"input_tokens":100,"output_tokens":30}}}"#,
            r#"{"type":"message","role":"assistant","sessionId":"s1","timestamp":1010000,"providerData":{"model":"glm-5.3-flash"},"message":{"usage":{"input_tokens":200,"output_tokens":4000}}}"#,
        ];
        let facts_of = |start: usize| {
            adapter()
                .parse_file_at(Path::new("s1.jsonl"), &lines, start)
                .into_iter()
                .filter_map(|p| match p {
                    LineParse::Facts(f) => f.usage.map(|u| (f.ts_ms.unwrap(), u.duration_ms)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            facts_of(0),
            vec![
                (1_007_050, Some(7_050)), // 锚在 user message（紧邻 reasoning 只差 20ms）
                (1_009_040, Some(1_940)), // 锚在 function_call_result
                (1_010_000, Some(2_900)), // 轮末 assistant message 同口径
            ]
        );
        // 增量续扫：前缀已含首个边界时，从 start=4 续算与全量一致（确定性契约）。
        assert_eq!(facts_of(4), facts_of(0)[1..]);
    }

    /// 文件首个边界出现前的 usage 行无锚可依：耗时 `None`，不编造。
    #[test]
    fn usage_before_any_boundary_has_no_duration() {
        let lines = [
            r#"{"type":"function_call","sessionId":"s1","timestamp":1000,"providerData":{"model":"m"},"name":"Bash","arguments":{},"message":{"usage":{"input_tokens":10,"output_tokens":5}}}"#,
        ];
        match adapter()
            .parse_file_at(Path::new("s1.jsonl"), &lines, 0)
            .remove(0)
        {
            LineParse::Facts(facts) => {
                assert_eq!(facts.usage.unwrap().duration_ms, None);
            }
            other => panic!("期望 Facts，实际 {other:?}"),
        }
    }

    #[test]
    fn user_message_becomes_title_but_tool_blocks_do_not() {
        let user = r#"{"type":"message","role":"user","sessionId":"s1","timestamp":1790084484321,"content":[{"type":"input_text","text":"  检查 figma 画板  "}]}"#;
        let facts = parse(user);
        assert_eq!(facts.title.as_deref(), Some("检查 figma 画板"));
        assert_eq!(facts.usage, None);

        let tool_block = r#"{"type":"message","role":"user","sessionId":"s1","timestamp":1790084484321,"content":[{"type":"function_call_result","content":"x"}]}"#;
        match adapter().parse_line(tool_block) {
            LineParse::Skip => {}
            other => panic!("工具结果行无事实，实际 {other:?}"),
        }
    }

    /// 实测：user 行 text 块开头是 harness 注入的 <system-reminder> 上下文，
    /// 真实提问缀在末尾的 <user_query> 标签里——标题取标签内容。
    #[test]
    fn title_comes_from_user_query_tag_not_injected_context() {
        let text = "<system-reminder data-role=\"user-context\">\n<user_info>\nOS Version: win32\n</user_info>\n</system-reminder>\n<user_query>这里是一个skill，请你帮我来审计一下是否安全。</user_query>";
        let user = format!(
            r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}}]}}"#,
            serde_json::to_string(text).unwrap()
        );
        let facts = parse(&user);
        assert_eq!(
            facts.title.as_deref(),
            Some("这里是一个skill，请你帮我来审计一下是否安全。")
        );
    }

    /// 纯注入上下文（如模式切换提醒）没有提问，不作标题。
    #[test]
    fn injected_context_without_user_query_yields_no_title() {
        let user = r#"{"type":"message","role":"user","sessionId":"s1","content":[{"type":"input_text","text":"<system-reminder data-role=\"user-context\">\n<craft_mode>\nYou are now in Agent mode.\n</craft_mode>\n</system-reminder>"}]}"#;
        match adapter().parse_line(user) {
            LineParse::Skip => {}
            other => panic!("纯注入行无事实，实际 {other:?}"),
        }
        // 同伴消息（多智能体协作）同样是注入，不是用户提问。
        let teammate = r#"{"type":"message","role":"user","sessionId":"s1","content":[{"type":"input_text","text":"<teammate-message teammate_id=\"team-lead\" summary=\"任务\">先做这个</teammate-message>"}]}"#;
        match adapter().parse_line(teammate) {
            LineParse::Skip => {}
            other => panic!("同伴消息行无事实，实际 {other:?}"),
        }
    }

    /// ai-title 行携带应用自己生成的会话标题。
    #[test]
    fn ai_title_line_yields_title() {
        let line = r#"{"id":"59b0","timestamp":1790084665252,"type":"ai-title","aiTitle":"审计 示例技能","sessionId":"1a2b3c4d-075f","cwd":"c:\\Users\\dev"}"#;
        let facts = parse(line);
        assert_eq!(facts.title.as_deref(), Some("审计 示例技能"));
        assert_eq!(facts.usage, None);
        assert_eq!(facts.ts_ms, Some(1_790_084_665_252));
    }

    /// 只发了图片（无文字）的 user 消息：纯路径标签不是提问，不作标题。
    #[test]
    fn pure_image_reference_message_yields_no_title() {
        let user = r#"{"type":"message","role":"user","sessionId":"s1","content":[{"type":"input_text","text":"<image_local_path>C:\\Users\\me\\.workbuddy\\clipboard-images\\x.png</image_local_path>"}]}"#;
        match adapter().parse_line(user) {
            LineParse::Skip => {}
            other => panic!("纯图片引用行无事实，实际 {other:?}"),
        }
    }

    #[test]
    fn lines_without_session_or_usage_are_skipped() {
        assert_eq!(
            adapter().parse_line(r#"{"type":"session-meta","sessionId":"s1"}"#),
            LineParse::Skip
        );
        assert_eq!(
            adapter().parse_line(r#"{"type":"message","timestamp":1}"#),
            LineParse::Skip,
            "无 sessionId 不编造归属"
        );
        assert_eq!(adapter().parse_line("{ not json"), LineParse::Malformed);
    }

    #[test]
    fn title_is_trimmed_and_capped() {
        let long = "字".repeat(500);
        let line =
            format!(r#"{{"type":"message","role":"user","sessionId":"s1","content":"{long}"}}"#);
        let facts = parse(&line);
        assert_eq!(facts.title.as_deref().map(|t| t.chars().count()), Some(200));
    }

    #[test]
    fn only_jsonl_files_count_as_session_logs() {
        let adapter = adapter();
        assert!(adapter.is_session_log(Path::new("/p/sess.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/p/sess.meta.json")));
        assert!(!adapter.is_session_log(Path::new("/p/sess.file-rollback.ndjson")));
    }

    #[test]
    fn transcript_reads_messages_and_function_calls() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-tr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"type":"message","role":"user","sessionId":"s1","timestamp":1790084484321,"content":[{"type":"input_text","text":"检查 figma 画板"}]}"#,
                r#"{"type":"function_call","id":"01a0","sessionId":"s1","timestamp":1790084494341,"providerData":{"model":"hy4-preview-f"},"name":"bash","arguments":{"cmd":"ls"},"message":{"usage":{}}}"#,
                r#"{"type":"message","role":"user","sessionId":"s1","timestamp":1790084495000,"content":[{"type":"function_call_result","content":"ok"}]}"#,
                r#"{"type":"message","role":"user","sessionId":"other","content":"别的会话"}"#,
                "{ not json",
            ]
            .join("\n"),
        )
        .unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].blocks[0],
            transcript::TranscriptBlock::Text {
                text: "检查 figma 画板".into()
            }
        );
        assert_eq!(entries[1].model.as_deref(), Some("hy4-preview-f"));
        assert!(matches!(
            entries[1].blocks[0],
            TranscriptBlock::ToolCall { .. }
        ));
        assert!(matches!(
            entries[2].blocks[0],
            TranscriptBlock::ToolResult { .. }
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 实测：user 行 text 块 = 注入的 <system-reminder> 上下文 + 末尾
    /// <user_query> 里的真实提问——Injected 块携带整条完整原文（提问内容
    /// 在标签内，完整口径一字不差）+ 注入段字符数；正文只留提问。
    #[test]
    fn transcript_user_text_segments_injected_from_query() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-clean-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let reminder = "<system-reminder data-role=\"user-context\">\n<user_info>\nOS Version: win32\n</user_info>\n</system-reminder>";
        let text = format!("{reminder}\n<user_query>查看项目内容</user_query>");
        let line = format!(
            r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}}]}}"#,
            serde_json::to_string(&text).unwrap()
        );
        std::fs::write(&path, line.as_str()).unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].blocks,
            vec![
                transcript::TranscriptBlock::Injected {
                    text: text.clone(),
                    injected_chars: reminder.chars().count() as i64,
                },
                transcript::TranscriptBlock::Text {
                    text: "查看项目内容".into()
                },
            ]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 无注入的 user_query 消息（若有此形态）：正文即提问，不产 Injected 块、
    /// 不打占比条。
    #[test]
    fn transcript_plain_user_query_yields_no_injected_block() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let line = r#"{"type":"message","role":"user","sessionId":"s1","content":[{"type":"input_text","text":"<user_query>裸提问</user_query>"}]}"#;
        std::fs::write(&path, line).unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].blocks,
            vec![transcript::TranscriptBlock::Text {
                text: "裸提问".into()
            }]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 纯注入消息（模式切换提醒等）不丢弃：保留为仅含 Injected 块的条目，
    /// 存在感由前端呈现，目录不由此开启轮次。
    #[test]
    fn transcript_pure_injected_message_keeps_injected_block() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let reminder = "<system-reminder data-role=\"user-context\">\n<craft_mode>\nYou are now in Agent mode.\n</craft_mode>\n</system-reminder>";
        let line = format!(
            r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}}]}}"#,
            serde_json::to_string(&reminder).unwrap()
        );
        std::fs::write(&path, [
            line.as_str(),
            r#"{"type":"message","role":"user","sessionId":"s1","content":[{"type":"input_text","text":"真问题"}]}"#,
        ]
        .join("\n"))
        .unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].blocks,
            vec![transcript::TranscriptBlock::Injected {
                text: reminder.into(),
                injected_chars: reminder.chars().count() as i64,
            }]
        );
        assert_eq!(
            entries[1].blocks,
            vec![transcript::TranscriptBlock::Text {
                text: "真问题".into()
            }]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 任务通知：字段解析 + 实体反转义 + 原文保留（含尾部指令模板），
    /// 不走 reminder/user_query 分段。
    #[test]
    fn transcript_task_notification_parses_fields_and_keeps_raw() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let text = "<task-notification>\n<task-id>9lfYLN</task-id>\n<tool-use-id>call_1</tool-use-id>\n<status>completed</status>\n<summary>Background command &quot;pnpm check 2&gt;&amp;1&quot; completed</summary>\n</task-notification>\n\nUse the TaskOutput tool with task_id=\"9lfYLN\" to retrieve the full output.";
        let line = format!(
            r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}}]}}"#,
            serde_json::to_string(&text).unwrap()
        );
        std::fs::write(&path, line.as_str()).unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].blocks,
            vec![transcript::TranscriptBlock::TaskNotification {
                task_id: "9lfYLN".into(),
                status: "completed".into(),
                summary: "Background command \"pnpm check 2>&1\" completed".into(),
                text: text.into(),
            }]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 压缩摘要取标签内原文；紧随的固定"继续对话"指令并入摘要条目
    /// （has_continue 置位），不再单独成条目。
    #[test]
    fn transcript_history_summary_merges_continue_notice() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-sum-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let summary = "<conversation_history_summary>\n# Conversation Summary\n\n用户只提出了一个明确请求。\n</conversation_history_summary>";
        let continue_text = "Please continue with the conversation based on the summarized context above. Maintain the same level of detail and helpfulness as before the summarization.";
        let lines = [
            format!(
                r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}}]}}"#,
                serde_json::to_string(&summary).unwrap()
            ),
            format!(
                r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}}]}}"#,
                serde_json::to_string(&continue_text).unwrap()
            ),
        ];
        std::fs::write(&path, lines.join("\n").as_str()).unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].blocks,
            vec![transcript::TranscriptBlock::HistorySummary {
                text: "# Conversation Summary\n\n用户只提出了一个明确请求。".into(),
                has_continue: true,
            }]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 同伴消息剥掉包裹标签留正文；附件提及在提问里，清洗后仍按位置重排。
    #[test]
    fn transcript_teammate_unwraps_and_image_mention_survives_cleaning() {
        let dir = std::env::temp_dir().join(format!("toktol-wb-tm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let teammate = r#"{"type":"message","role":"user","sessionId":"s1","content":[{"type":"input_text","text":"<teammate-message teammate_id=\"team-lead\" summary=\"任务\">\n先做扫描\n</teammate-message>"}]}"#;
        let text = "<system-reminder data-role=\"user-context\">\n</system-reminder>\n<user_query>@image#1:shot.png 看看截图</user_query>";
        let with_image = format!(
            r#"{{"type":"message","role":"user","sessionId":"s1","content":[{{"type":"input_text","text":{}}},{{"type":"image_blob_ref","blob_path":"C:\\b\\shot.png","original_filename":"shot.png","size":1}},{{"type":"input_text","text":"<image_local_path>C:\\b\\shot.png</image_local_path>"}}]}}"#,
            serde_json::to_string(text).unwrap()
        );
        std::fs::write(&path, [teammate, with_image.as_str()].join("\n")).unwrap();

        let entries = adapter().read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].blocks,
            vec![transcript::TranscriptBlock::Text {
                text: "先做扫描".into()
            }]
        );
        // 注入段在前，提及处切开文本、图片插进去：分段没有破坏提及匹配。
        assert!(matches!(
            entries[1].blocks[0],
            transcript::TranscriptBlock::Injected { .. }
        ));
        assert!(matches!(
            entries[1].blocks[1],
            transcript::TranscriptBlock::Image { .. }
        ));
        assert_eq!(
            entries[1].blocks[2],
            transcript::TranscriptBlock::Text {
                text: " 看看截图".into()
            }
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 项目目录从存放目录名解码（真实目录验证消歧）。在临时目录里造
    /// projects/<编码名>/sess.jsonl 的真实结构：叶子项目名带 `-`，
    /// 贪心匹配必须把它整段留下而不是切成两层。
    /// FS 消歧的前提是解码出的真实目录在扫描机上存在——生产数据是
    /// Windows 形态（编码名由盘符路径而来），故本测试仅在 Windows 宿主
    /// 运行；其他宿主走朴素解码回退（见下方平台无关测试与
    ///
    /// FS 消歧的前提是解码出的真实目录在扫描机上存在——生产数据是 Windows
    /// 形态（编码名由盘符路径而来），故本测试仅在 Windows 宿主运行；其他
    /// 宿主走朴素解码回退（平台无关行为由 naive 回退测试覆盖）。
    #[cfg(windows)]
    /// [`decode_segments`] 的平台无关贪心测试）。
    #[cfg(windows)]
    #[test]
    fn project_dir_decodes_from_directory_name() {
        let projects = std::env::temp_dir().join(format!("toktol-wb-enc-{}", std::process::id()));
        let project = projects.join("wb-demo-proj");
        std::fs::create_dir_all(&project).unwrap();

        // 实测编码规则：盘符小写 + ":\" 折叠成单个 "-"，其余分隔符换成 "-"。
        let text = project.to_string_lossy();
        let encoded = format!(
            "{}-{}",
            text[..1].to_lowercase(),
            text[3..].replace(['\\', '/'], "-")
        );
        let session_dir = projects.join(&encoded);
        std::fs::create_dir_all(&session_dir).unwrap();
        let log = session_dir.join("s1.jsonl");

        let adapter = adapter();
        let line = r#"{"type":"message","role":"user","sessionId":"s1","timestamp":1790084484321,"content":[{"type":"input_text","text":"标题"}]}"#;
        let facts = match adapter.parse_file_at(&log, &[line], 0).remove(0) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(
            facts.project_dir.as_deref(),
            Some(project.to_string_lossy().as_ref()),
            "带 `-` 的叶子名靠 FS 验证整段保留"
        );

        std::fs::remove_dir_all(&projects).unwrap();
    }

    /// 目录验证失败（项目已删）退回朴素解码：全部 `-` 视作分隔符——可能不精确
    /// 但必须稳定可读，不能丢项目归属。
    #[test]
    fn project_dir_falls_back_to_naive_decode() {
        assert_eq!(
            decode_project_dir("d-Work-Client-Alpha"),
            Some("D:\\Work\\Client\\Alpha".to_string())
        );
        // 不是"盘符-"开头的目录名解不出盘符，返回 None 不编造。
        assert_eq!(decode_project_dir("not-a-drive"), None);
    }
}
