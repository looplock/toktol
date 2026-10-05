//! codex 适配器：解析 `~/.codex/sessions/**/*.jsonl` rollout 日志。
//! 红线：只碰 `sessions/` 下的会话日志；`~/.codex/config.toml`、`auth.json` 等
//! 配置与凭据文件不在扫描范围，绝不打开。`archived_sessions/` 也不扫——文件被
//! 移入新路径后 dedup_key（含路径）会变，同一份用量要重复入账。
//!
//! 行格式（实测 0.153.x）：每行一个信封 `{timestamp, ordinal?, type, payload}`。
//! usage 行（`token_usage_record`）不带模型名，模型要从最近的 `turn_context` /
//! `thread_settings_applied` / `session_meta` 行取（实测顺序保证先于同回合的
//! usage 行）——跨行上下文，完整解析走 `parse_file`；`parse_line` 只有降级口径。
//! 主会话文件
//! 没有 `event_msg/user_message`，真实用户输入是 `response_item` 的 user 消息，
//! 注入的系统上下文以 `<environment_context>` 等 `<` 开头；子代理文件（source
//! 为对象、带 parent_thread_id）的消息是任务指派文本，不出标题。子代理文件的
//! usage `session_id` 就是父线程 id，用量自然归并主会话。分桶口径（实测恒成立）：
//! `total = input + output`，`cached_input_tokens` 是 input 的一部分（OpenAI 语义，
//! 同 grok / workbuddy，入库前拆出否则缓存读按输入价二次计费）；
//! `reasoning_output_tokens` 是输出的子集，输出记净量。
//! 请求耗时（实测 0.153.x）：每次 API 请求以 `token_usage_record` 行收尾（响应
//! 完成点），耗时 = 该行时间戳 − 触发行时间戳。触发行是请求边界——响应流
//! （`response_item` 的 reasoning/message/function_call 及 item_completed 的
//! Reasoning/AgentMessage 镜像）不算边界，工具输出、task_started、上一请求的
//! usage 行等簿记行是边界。负差值不编造。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::transcript::{self, TranscriptBlock, TranscriptEntry, TranscriptRole};
use super::{Adapter, LineFacts, LineParse, UsageFacts};
use crate::error::Result;
use crate::model::{TokenUsage, Tool};

/// 会话标题取首条真实用户消息文本，超长截断——标题是提示词，不是全文备份。
const TITLE_MAX_CHARS: usize = 200;

/// codex 的适配器实现；同一文件内容加同一 start 必须解析出同一结果（见 [`super`]）。
pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn tool(&self) -> Tool {
        Tool::Codex
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        match crate::paths::home_dir() {
            Some(home) => vec![home.join(".codex").join("sessions")],
            None => vec![],
        }
    }

    fn is_session_log(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-"))
    }

    /// 降级口径：单行模式没有跨行上下文，只报行内自足的事实（会话归属、项目
    /// 目录），模型与标题都拿不到；token_count 可能与 token_usage_record 重复
    /// 播报同一响应，一律跳过防双计。完整解析走 [`Self::parse_file`]。
    fn parse_line(&self, line: &str) -> LineParse {
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(_) => return LineParse::Malformed,
        };
        self.facts(raw, &Context::default(), true)
    }

    fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
        // 是否存在 token_usage_record 必须看全文件：旧版日志只有 event_msg/token_count
        // 一种用量行，而同一响应可能两种都发，事后决定才不会双计。
        let has_usage_records = lines.iter().any(|line| {
            line.contains(r#""token_usage_record""#)
                && serde_json::from_str::<RawLine>(line)
                    .is_ok_and(|raw| raw.kind == "token_usage_record")
        });

        let mut ctx = Context::default();
        for line in &lines[..start] {
            if let Ok(raw) = serde_json::from_str::<RawLine>(line) {
                absorb(&raw, &mut ctx);
            }
        }

        lines[start..]
            .iter()
            .map(|line| match serde_json::from_str::<RawLine>(line) {
                Ok(raw) => {
                    absorb(&raw, &mut ctx);
                    self.facts(raw, &ctx, has_usage_records)
                }
                Err(_) => LineParse::Malformed,
            })
            .collect()
    }

    /// 转录只认 response_item 的消息与工具类 payload；meta/turn_context/
    /// usage 等簿记行没有对话内容。文件按构造是单会话（子代理文件另立），
    /// 不做行级会话过滤。
    fn read_transcript(
        &self,
        source_file: &Path,
        _external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let text = transcript::read_text(source_file)?;
        let mut entries = Vec::new();
        for line in text.lines() {
            let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
                continue;
            };
            if raw.kind != "response_item" {
                continue;
            }
            let ts_ms = raw.timestamp.as_deref().and_then(parse_ts_ms);
            let Some(entry) = transcript_entry(&raw.payload, ts_ms) else {
                continue;
            };
            entries.push(entry);
        }
        Ok(entries)
    }
}

/// 跨行上下文：从文件前缀重建、随 emit 区间推进。usage 行取当前模型，标题行取会话 id；
/// 触发行时间戳供 [`Context::absorb`] 推导请求耗时（见模块文档）。
#[derive(Default)]
struct Context {
    session_id: Option<String>,
    is_subagent: bool,
    model: Option<String>,
    trigger_ts: Option<i64>,
    /// 最近一次 token_usage_record 推导出的耗时，facts 消费。
    derived_duration: Option<i64>,
}

/// 响应流的组成部分：不算请求边界（见模块文档的耗时口径）。
fn is_response_stream(raw: &RawLine) -> bool {
    match raw.kind.as_str() {
        "response_item" => matches!(
            raw.payload.kind.as_deref(),
            Some("reasoning" | "message" | "function_call")
        ),
        // item_completed 的 Reasoning/AgentMessage 是流式项的镜像；CommandExecution
        // 等工具执行事件与 UserMessage 回显是请求边界。
        "event_msg" => {
            raw.payload.kind.as_deref() == Some("item_completed")
                && raw
                    .payload
                    .item
                    .as_ref()
                    .and_then(|item| item.kind.as_deref())
                    .is_some_and(|t| t == "Reasoning" || t == "AgentMessage")
        }
        _ => false,
    }
}

/// 把一行的上下文增量并进去；模型取"最近一次声明"，触发行取最近的请求边界行。
fn absorb(raw: &RawLine, ctx: &mut Context) {
    if raw.kind == "token_usage_record" {
        // 每次请求以此行收尾：耗时 = 触发行 → 此行；此行自身是下一请求的边界。
        let ts = raw.timestamp.as_deref().and_then(parse_ts_ms);
        ctx.derived_duration = match (ctx.trigger_ts, ts) {
            (Some(trigger), Some(ts)) if ts >= trigger => Some(ts - trigger),
            _ => None,
        };
        ctx.trigger_ts = ts.or(ctx.trigger_ts);
        return;
    }
    match raw.kind.as_str() {
        "session_meta" => {
            ctx.session_id = external_id(&raw.payload, &raw.payload.id);
            ctx.is_subagent = raw.payload.parent_thread_id.is_some()
                || matches!(raw.payload.source, Some(serde_json::Value::Object(_)));
            if let Some(model) = clean_model(&raw.payload.model) {
                ctx.model = Some(model);
            }
        }
        "turn_context" => {
            if let Some(model) = clean_model(&raw.payload.model) {
                ctx.model = Some(model);
            }
        }
        "event_msg" if raw.payload.kind.as_deref() == Some("thread_settings_applied") => {
            if let Some(settings) = &raw.payload.thread_settings
                && let Some(model) = clean_model(&settings.model)
            {
                ctx.model = Some(model);
            }
        }
        _ => {}
    }
    if !is_response_stream(raw)
        && let Some(ts) = raw.timestamp.as_deref().and_then(parse_ts_ms)
    {
        ctx.trigger_ts = Some(ts);
    }
}

impl CodexAdapter {
    fn facts(&self, raw: RawLine, ctx: &Context, has_usage_records: bool) -> LineParse {
        let ts_ms = raw.timestamp.as_deref().and_then(parse_ts_ms);
        match raw.kind.as_str() {
            "session_meta" => {
                let Some(session_id) = external_id(&raw.payload, &raw.payload.id) else {
                    return LineParse::Skip;
                };
                LineParse::Facts(Box::new(LineFacts {
                    dedup_suffix: None,
                    request_count: 1,
                    session_external_id: session_id,
                    title: None,
                    project_dir: raw.payload.cwd,
                    ts_ms,
                    usage: None,
                }))
            }
            "response_item" => {
                let title = title_of(&raw.payload, ctx.is_subagent);
                let Some(session_id) = ctx.session_id.clone() else {
                    return LineParse::Skip;
                };
                if title.is_none() {
                    return LineParse::Skip;
                }
                LineParse::Facts(Box::new(LineFacts {
                    dedup_suffix: None,
                    request_count: 1,
                    session_external_id: session_id,
                    title,
                    project_dir: None,
                    ts_ms,
                    usage: None,
                }))
            }
            "token_usage_record" => {
                // 子代理文件的 session_id 是父线程：用量归并主会话。
                let Some(session_id) = external_id(&raw.payload, &raw.payload.thread_id) else {
                    return LineParse::Skip;
                };
                let usage = raw
                    .payload
                    .usage
                    .as_ref()
                    .zip(ctx.model.as_deref())
                    .zip(ts_ms)
                    .map(|((u, model), ts_ms)| usage_facts(u, model, ts_ms, ctx.derived_duration));
                LineParse::Facts(Box::new(LineFacts {
                    dedup_suffix: None,
                    request_count: 1,
                    session_external_id: session_id,
                    title: None,
                    project_dir: None,
                    ts_ms,
                    usage,
                }))
            }
            // 旧版兜底：没有 token_usage_record 的文件里，token_count 的
            // last_token_usage 是每次响应的增量。
            "event_msg"
                if raw.payload.kind.as_deref() == Some("token_count") && !has_usage_records =>
            {
                let Some(session_id) = ctx.session_id.clone() else {
                    return LineParse::Skip;
                };
                let usage = raw
                    .payload
                    .info
                    .as_ref()
                    .and_then(|info| info.last_token_usage.as_ref())
                    .zip(ctx.model.as_deref())
                    .zip(ts_ms)
                    // 旧版日志口径未勘探，不做耗时推导。
                    .map(|((u, model), ts_ms)| usage_facts(u, model, ts_ms, None));
                if usage.is_none() {
                    return LineParse::Skip;
                }
                LineParse::Facts(Box::new(LineFacts {
                    dedup_suffix: None,
                    request_count: 1,
                    session_external_id: session_id,
                    title: None,
                    project_dir: None,
                    ts_ms,
                    usage,
                }))
            }
            _ => LineParse::Skip,
        }
    }
}

/// response_item 的 payload → 转录消息；其余 payload 形状返回 `None`。
fn transcript_entry(payload: &RawPayload, ts_ms: Option<i64>) -> Option<TranscriptEntry> {
    match payload.kind.as_deref() {
        Some("message") => {
            let role = match payload.role.as_deref()? {
                "user" => TranscriptRole::User,
                "assistant" => TranscriptRole::Assistant,
                "system" | "developer" => TranscriptRole::System,
                _ => return None,
            };
            let blocks = payload
                .content
                .as_ref()
                .map(|content| harness_context_blocks(transcript::blocks_from_content(content)))
                .unwrap_or_default();
            (!blocks.is_empty()).then_some(TranscriptEntry {
                role,
                ts_ms,
                model: None,
                blocks,
            })
        }
        Some("function_call") => Some(TranscriptEntry {
            role: TranscriptRole::Assistant,
            ts_ms,
            model: None,
            blocks: vec![TranscriptBlock::ToolCall {
                id: payload.call_id.clone().or_else(|| payload.id.clone()),
                name: payload.name.clone(),
                arguments: payload.arguments.as_ref().map(transcript::arguments_text),
            }],
        }),
        Some("function_call_output") => Some(TranscriptEntry {
            role: TranscriptRole::Tool,
            ts_ms,
            model: None,
            blocks: vec![TranscriptBlock::ToolResult {
                call_id: payload.call_id.clone(),
                content: payload
                    .output
                    .as_ref()
                    .map(transcript::arguments_text)
                    .unwrap_or_default(),
                is_error: false,
            }],
        }),
        Some("reasoning") => {
            let blocks = payload
                .summary
                .as_ref()
                .map(transcript::blocks_from_content)
                .unwrap_or_default();
            (!blocks.is_empty()).then_some(TranscriptEntry {
                role: TranscriptRole::Assistant,
                ts_ms,
                model: None,
                blocks,
            })
        }
        _ => None,
    }
}

/// codex 开场注入的包裹标签：这些消息是工具塞给模型的指令/快照，不是
/// 用户说的话。文本以 `<标签>` 开头即整段归类为
/// [`TranscriptBlock::HarnessContext`]（列表之外的前缀仍是普通文本——
/// 恰好以 `<` 开头的真实输入不该被误伤）。`<turn_aborted>` 是用户中断
/// 事件而非指令上下文，单独归类为 [`TranscriptBlock::TurnAborted`]。
const HARNESS_TAGS: [&str; 4] = [
    "environment_context",
    "user_instructions",
    "skills_instructions",
    "permissions instructions",
];

/// 把内容块里的包裹文本归一成 [`TranscriptBlock::HarnessContext`] /
/// [`TranscriptBlock::TurnAborted`]。
fn harness_context_blocks(blocks: Vec<TranscriptBlock>) -> Vec<TranscriptBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            TranscriptBlock::Text { text } => {
                let trimmed = text.trim_start();
                if trimmed.starts_with("<turn_aborted>") {
                    return TranscriptBlock::TurnAborted;
                }
                match HARNESS_TAGS
                    .iter()
                    .find(|tag| trimmed.starts_with(&format!("<{tag}>")))
                {
                    Some(tag) => TranscriptBlock::HarnessContext {
                        tag: (*tag).to_string(),
                        text,
                    },
                    None => TranscriptBlock::Text { text },
                }
            }
            other => other,
        })
        .collect()
}

/// 会话归属 id：优先 payload 自带的 `session_id`（子代理文件里即父线程 id），
/// 缺了退回备用字段。usage 行的备用是 `thread_id`，meta 行是 `id`。
fn external_id(payload: &RawPayload, fallback: &Option<String>) -> Option<String> {
    payload
        .session_id
        .as_ref()
        .filter(|id| !id.is_empty())
        .or_else(|| fallback.as_ref().filter(|id| !id.is_empty()))
        .cloned()
}

/// 标题候选：主会话文件里第一条非注入（`<` 开头）的 user 消息文本。
fn title_of(payload: &RawPayload, is_subagent: bool) -> Option<String> {
    if is_subagent || payload.role.as_deref() != Some("user") {
        return None;
    }
    let text = match &payload.content {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Array(blocks)) => blocks.iter().find_map(|block| {
            let obj = block.as_object()?;
            obj.get("text")?.as_str().map(str::to_string)
        }),
        _ => None,
    }?;
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('<') {
        return None;
    }
    let mut text = trimmed.to_string();
    if text.chars().count() > TITLE_MAX_CHARS {
        text = text.chars().take(TITLE_MAX_CHARS).collect();
    }
    Some(text)
}

fn usage_facts(u: &RawUsage, model: &str, ts_ms: i64, duration_ms: Option<i64>) -> UsageFacts {
    let reasoning = u.reasoning_output_tokens.max(0);
    let output = u.output_tokens.max(0);
    // OpenAI 语义：cached ⊆ input，统一模型四桶互斥，输入记拆掉缓存读的净量。
    let cache_read = u.cached_input_tokens.max(0);
    UsageFacts {
        ts_ms,
        model_raw: model.to_string(),
        model: normalize_model(model),
        duration_ms,
        usage: TokenUsage {
            input_tokens: (u.input_tokens.max(0) - cache_read).max(0),
            output_tokens: output - reasoning.min(output),
            cache_read_tokens: cache_read,
            cache_write_tokens: u.cache_write_input_tokens.max(0),
            reasoning_tokens: (reasoning > 0).then_some(reasoning),
        },
    }
}

fn parse_ts_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn clean_model(model: &Option<String>) -> Option<String> {
    match model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => Some(model.to_string()),
        _ => None,
    }
}

/// `gpt-*`/`codex-*` 剥尾部日期后缀（`20250626` 一段，或 `2025-04-14` 三段），
/// 让价格目录按模型族归并；其它供应商的名字原样保留——不知道怎么归一就不乱动。
fn normalize_model(raw: &str) -> String {
    let prefix = ["gpt-", "codex-"].into_iter().find(|p| raw.starts_with(*p));
    let Some(prefix) = prefix else {
        return raw.to_string();
    };
    let segs: Vec<&str> = raw[prefix.len()..].split('-').collect();
    let strip = if segs.len() >= 2 && is_digits(segs[segs.len() - 1], 8) {
        1
    } else if segs.len() >= 4
        && is_digits(segs[segs.len() - 3], 4)
        && is_digits(segs[segs.len() - 2], 2)
        && is_digits(segs[segs.len() - 1], 2)
    {
        3
    } else {
        0
    };
    format!("{prefix}{}", segs[..segs.len() - strip].join("-"))
}

fn is_digits(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| b.is_ascii_digit())
}

/// 信封行。payload 各记录类型形状不同，拍平成一个宽松结构，按 `kind` 取用。
#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: RawPayload,
}

#[derive(Deserialize, Default)]
struct RawPayload {
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    parent_thread_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    /// 字符串（主会话）或对象（子代理）二态，只判是否对象。
    #[serde(default)]
    source: Option<serde_json::Value>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    // ── 以下是转录提取用的 response_item 字段（用量解析不触碰）。
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<serde_json::Value>,
    #[serde(default)]
    output: Option<serde_json::Value>,
    #[serde(default)]
    summary: Option<serde_json::Value>,
    #[serde(default)]
    thread_settings: Option<ThreadSettings>,
    /// item_completed 携带的流式项；耗时口径只看它的类型（Reasoning/AgentMessage）。
    #[serde(default)]
    item: Option<StreamItem>,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    info: Option<TokenCountInfo>,
}

#[derive(Deserialize)]
struct ThreadSettings {
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct StreamItem {
    #[serde(rename = "type", default)]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct TokenCountInfo {
    #[serde(default)]
    last_token_usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    cached_input_tokens: i64,
    #[serde(rename = "cache_write_input_tokens", default)]
    cache_write_input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(rename = "reasoning_output_tokens", default)]
    reasoning_output_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lines: &[&str]) -> Vec<LineParse> {
        CodexAdapter.parse_file(lines, 0)
    }

    fn facts(line: &str) -> LineFacts {
        match parse(&[line]).into_iter().next() {
            Some(LineParse::Facts(facts)) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        }
    }

    const META: &str = r#"{"timestamp":"2026-09-12T07:19:10.911Z","ordinal":0,"type":"session_meta","payload":{"id":"01a0947c-04d5","session_id":"01a0947c-04d5","timestamp":"2026-09-12T07:19:10.911Z","cwd":"E:\\proj\\demo","originator":"codex-cli","cli_version":"0.153.4","source":"cli","model":"deepseek-v4-flash"}}"#;

    const TURN: &str = r#"{"timestamp":"2026-09-12T07:20:00.000Z","ordinal":5,"type":"turn_context","payload":{"turn_id":"t1","cwd":"E:\\proj\\demo","model":"deepseek-v4-flash","effort":"high"}}"#;

    const USAGE: &str = r#"{"timestamp":"2026-09-12T07:20:10.000Z","ordinal":9,"type":"token_usage_record","payload":{"thread_id":"01a0947c-04d5","turn_id":"t1","session_id":"01a0947c-04d5","usage":{"input_tokens":14669,"cached_input_tokens":3840,"cache_write_input_tokens":0,"output_tokens":176,"reasoning_output_tokens":168,"total_tokens":14845}}}"#;

    #[test]
    fn session_meta_yields_session_and_project() {
        let f = facts(META);
        assert_eq!(f.session_external_id, "01a0947c-04d5");
        assert_eq!(f.project_dir.as_deref(), Some("E:\\proj\\demo"));
        assert_eq!(f.ts_ms, Some(1_789_197_550_911));
        assert_eq!(f.title, None);
        assert_eq!(f.usage, None, "meta 行没有用量");
    }

    #[test]
    fn usage_takes_model_from_preceding_turn_context() {
        let results = parse(&[META, TURN, USAGE]);
        let f = match &results[2] {
            LineParse::Facts(f) => *f.clone(),
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(f.session_external_id, "01a0947c-04d5");
        let usage = f.usage.expect("usage 行必须有用");
        assert_eq!(usage.model_raw, "deepseek-v4-flash");
        assert_eq!(usage.ts_ms, 1_789_197_610_000);
        assert_eq!(usage.usage.input_tokens, 10_829, "输入是拆掉缓存读的净量");
        assert_eq!(usage.usage.cache_read_tokens, 3840);
        assert_eq!(usage.usage.cache_write_tokens, 0);
        assert_eq!(usage.usage.output_tokens, 8, "输出记扣掉推理后的净量");
        assert_eq!(usage.usage.reasoning_tokens, Some(168));
    }

    #[test]
    fn usage_without_model_context_is_session_only() {
        let f = facts(USAGE);
        assert_eq!(f.session_external_id, "01a0947c-04d5");
        assert_eq!(f.usage, None, "拿不到模型就不记用量");
    }

    #[test]
    fn resume_rebuilds_model_context_from_prefix() {
        let lines = [META, TURN, USAGE];
        let resumed = match CodexAdapter.parse_file(&lines, 2).into_iter().next() {
            Some(LineParse::Facts(f)) => *f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        let whole = parse(&lines);
        let expected = match &whole[2] {
            LineParse::Facts(f) => (**f).clone(),
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(resumed, expected, "续扫与前缀重建必须得出同一结果");
    }

    #[test]
    fn thread_settings_switch_updates_model_for_later_usage() {
        let switched = r#"{"timestamp":"2026-09-12T07:25:00.000Z","ordinal":40,"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-5.6-luna","model_provider_id":"codeg"}}}"#;
        let usage_b = r#"{"timestamp":"2026-09-12T07:25:10.000Z","ordinal":45,"type":"token_usage_record","payload":{"session_id":"01a0947c-04d5","usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let results = parse(&[META, TURN, USAGE, switched, usage_b]);
        let first = match &results[2] {
            LineParse::Facts(f) => f.usage.as_ref().unwrap(),
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        let second = match &results[4] {
            LineParse::Facts(f) => f.usage.as_ref().unwrap(),
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(first.model_raw, "deepseek-v4-flash");
        assert_eq!(second.model_raw, "gpt-5.6-luna", "切换后按新模型入账");
    }

    #[test]
    fn harness_wrappers_become_harness_context_blocks() {
        fn payload_of(line: &str) -> RawPayload {
            serde_json::from_str::<RawLine>(line).unwrap().payload
        }
        let skills = r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<skills_instructions>\n## Skills\n清单"}]}}"#;
        let env = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>E:\\proj</cwd>\n</environment_context>"}]}}"#;
        let aborted = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<turn_aborted> 中断</turn_aborted>"}]}}"#;
        let real = r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<b>粗体</b>开头的真实输入"}]}}"#;

        let e = transcript_entry(&payload_of(skills), None).unwrap();
        assert!(matches!(
            &e.blocks[0],
            TranscriptBlock::HarnessContext { tag, .. } if tag == "skills_instructions"
        ));
        let e = transcript_entry(&payload_of(env), None).unwrap();
        assert!(matches!(
            &e.blocks[0],
            TranscriptBlock::HarnessContext { tag, .. } if tag == "environment_context"
        ));
        // 中断是事件不是指令上下文：单独归类。
        let e = transcript_entry(&payload_of(aborted), None).unwrap();
        assert!(matches!(&e.blocks[0], TranscriptBlock::TurnAborted));
        // 已知标签之外、恰好以 < 开头的输入仍是普通文本，不误伤。
        let e = transcript_entry(&payload_of(real), None).unwrap();
        assert!(matches!(&e.blocks[0], TranscriptBlock::Text { .. }));
    }

    #[test]
    fn title_comes_from_real_user_message_not_injected_wrappers() {
        let injected = r#"{"timestamp":"2026-09-12T07:19:11.000Z","ordinal":1,"type":"response_item","payload":{"type":"message","id":"msg_0","role":"user","content":[{"type":"input_text","text":"<environment_context>\n  <cwd>E:\\proj\\demo</cwd>\n</environment_context>"}]}}"#;
        let real = r#"{"timestamp":"2026-09-12T07:19:12.000Z","ordinal":2,"type":"response_item","payload":{"type":"message","id":"msg_1","role":"user","content":[{"type":"input_text","text":"  帮我看看这个 bug  "}]}}"#;
        let results = parse(&[META, injected, real]);
        assert_eq!(
            match &results[1] {
                LineParse::Facts(_) => panic!("注入上下文不该有事实"),
                other => other,
            },
            &LineParse::Skip
        );
        let f = match &results[2] {
            LineParse::Facts(f) => f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(f.title.as_deref(), Some("帮我看看这个 bug"));
        assert_eq!(f.usage, None);
    }

    #[test]
    fn subagent_files_merge_into_parent_without_title() {
        let sub_meta = r#"{"timestamp":"2026-09-12T07:22:30.000Z","type":"session_meta","payload":{"id":"sub-1","session_id":"01a0947c-04d5","parent_thread_id":"01a0947c-04d5","cwd":"E:\\proj\\demo","source":{"subagent":{"other":"guardian"}}}}"#;
        let assignment = r#"{"timestamp":"2026-09-12T07:22:31.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"评审这段历史"}]}}"#;
        let sub_usage = r#"{"timestamp":"2026-09-12T07:22:40.000Z","type":"token_usage_record","payload":{"thread_id":"sub-1","session_id":"01a0947c-04d5","usage":{"input_tokens":100,"output_tokens":10}}}"#;
        let results = parse(&[sub_meta, assignment, sub_usage]);
        let meta_f = match &results[0] {
            LineParse::Facts(f) => f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(meta_f.session_external_id, "01a0947c-04d5", "meta 归父会话");
        assert!(
            matches!(&results[1], LineParse::Skip),
            "子代理的消息不当标题"
        );
        let usage_f = match &results[2] {
            LineParse::Facts(f) => f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(
            usage_f.session_external_id, "01a0947c-04d5",
            "用量归并主会话"
        );
    }

    #[test]
    fn token_count_fallback_applies_only_without_usage_records() {
        let old_turn = r#"{"timestamp":"2026-09-12T07:20:00.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.1-codex"}}"#;
        let count = r#"{"timestamp":"2026-09-12T07:20:10.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":37},"last_token_usage":{"input_tokens":100,"cached_input_tokens":0,"output_tokens":37,"reasoning_output_tokens":30,"total_tokens":137}}}}"#;

        let results = parse(&[
            META.replace("deepseek-v4-flash", "gpt-5.1-codex").as_str(),
            old_turn,
            count,
        ]);
        let f = match &results[2] {
            LineParse::Facts(f) => f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        let usage = f.usage.as_ref().expect("旧版文件用 token_count 兜底");
        assert_eq!(usage.usage.output_tokens, 7);
        assert_eq!(usage.usage.reasoning_tokens, Some(30));

        // 同文件存在 token_usage_record 时，token_count 是重复播报，跳过防双计。
        let results = parse(&[META, TURN, USAGE, count]);
        assert!(matches!(&results[3], LineParse::Skip));
    }

    #[test]
    fn unknown_types_skip_and_garbage_is_malformed() {
        let world = r#"{"timestamp":"2026-09-12T07:20:00.000Z","type":"world_state","payload":{"full":true,"state":{}}}"#;
        assert_eq!(parse(&[world])[0], LineParse::Skip);
        assert_eq!(parse(&["{ not json"])[0], LineParse::Malformed);
        let no_ids = r#"{"timestamp":"2026-09-12T07:20:00.000Z","type":"token_usage_record","payload":{"usage":{"input_tokens":1}}}"#;
        assert_eq!(parse(&[no_ids])[0], LineParse::Skip, "无法归属会话的行跳过");
    }

    #[test]
    fn title_is_trimmed_and_capped() {
        let long = "字".repeat(500);
        let line = format!(
            r#"{{"timestamp":"2026-09-12T07:19:12.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"{long}"}}]}}}}"#
        );
        let results = parse(&[META, &line]);
        let f = match &results[1] {
            LineParse::Facts(f) => f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(f.title.as_ref().map(|t| t.chars().count()), Some(200));
    }

    #[test]
    fn duration_derived_from_boundary_to_usage_record_per_request() {
        let reasoning = r#"{"timestamp":"2026-09-12T07:20:12.000Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#;
        let tool_output = r#"{"timestamp":"2026-09-12T07:20:21.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"ok"}}"#;
        let mirror = r#"{"timestamp":"2026-09-12T07:20:25.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","id":"rs_1"}}}"#;
        let usage_b = r#"{"timestamp":"2026-09-12T07:20:30.000Z","ordinal":45,"type":"token_usage_record","payload":{"session_id":"01a0947c-04d5","usage":{"input_tokens":10,"output_tokens":5}}}"#;

        let results = parse(&[META, TURN, reasoning, USAGE, tool_output, mirror, usage_b]);
        // 第一请求：触发行是 TURN（07:20:00），usage 行 07:20:10 收尾。
        let first = match &results[3] {
            LineParse::Facts(f) => f.usage.as_ref().unwrap(),
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(first.duration_ms, Some(10_000));
        // 第二请求：触发行是工具输出（07:20:21）；item_completed 的 Reasoning
        // 镜像在响应流里，不算边界（否则耗时只有 5s）。
        let second = match &results[6] {
            LineParse::Facts(f) => f.usage.as_ref().unwrap(),
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(second.duration_ms, Some(9_000));
    }

    #[test]
    fn negative_delta_yields_none() {
        // usage 行早于触发行（乱序/回写）：负差值不编造。
        let early = r#"{"timestamp":"2026-09-12T07:19:05.000Z","type":"token_usage_record","payload":{"session_id":"01a0947c-04d5","usage":{"input_tokens":1,"output_tokens":1}}}"#;
        let results = parse(&[META, early]);
        let f = match &results[1] {
            LineParse::Facts(f) => f,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(f.usage.as_ref().unwrap().duration_ms, None);
    }

    #[test]
    fn normalize_strips_date_suffix_only_for_gpt_and_codex() {
        assert_eq!(normalize_model("gpt-5.1-20260101"), "gpt-5.1");
        assert_eq!(normalize_model("gpt-4.1-2025-04-14"), "gpt-4.1");
        assert_eq!(normalize_model("gpt-5.1-codex"), "gpt-5.1-codex");
        assert_eq!(normalize_model("codex-mini-20260101"), "codex-mini");
        assert_eq!(normalize_model("deepseek-v4-flash"), "deepseek-v4-flash");
        assert_eq!(normalize_model("gpt-5.6-luna"), "gpt-5.6-luna");
    }

    #[test]
    fn only_rollout_jsonl_files_count_as_session_logs() {
        let adapter = CodexAdapter;
        assert!(adapter.is_session_log(Path::new(
            "C:/Users/u/.codex/sessions/2026/09/12/rollout-2026-09-12T15-31-13-01a09487.jsonl"
        )));
        assert!(!adapter.is_session_log(Path::new("C:/Users/u/.codex/history.jsonl")));
        assert!(!adapter.is_session_log(Path::new("C:/Users/u/.codex/sessions/notes.jsonl")));
        assert!(!adapter.is_session_log(Path::new("C:/Users/u/.codex/config.toml")));
    }

    #[test]
    fn transcript_reads_response_items_in_order() {
        let dir = std::env::temp_dir().join(format!("toktol-codex-tr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-x.jsonl");
        std::fs::write(
            &path,
            [
                META,
                r#"{"timestamp":"2026-09-12T07:19:12.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>…</environment_context>"},{"type":"input_text","text":"帮我看看这个 bug"}]}}"#,
                r#"{"timestamp":"2026-09-12T07:19:20.000Z","type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"先定位"}]}}"#,
                r#"{"timestamp":"2026-09-12T07:19:21.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"结论"}]}}"#,
                r#"{"timestamp":"2026-09-12T07:19:25.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"bash","arguments":{"cmd":"ls"}}}"#,
                r#"{"timestamp":"2026-09-12T07:19:26.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"file.rs"}}"#,
                // 簿记行没有对话内容。
                TURN,
                USAGE,
            ]
            .join("\n"),
        )
        .unwrap();

        let entries = CodexAdapter.read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(entries[0].blocks.len(), 2, "注入上下文也如实展示");
        assert_eq!(entries[1].role, TranscriptRole::Assistant);
        assert_eq!(
            entries[1].blocks[0],
            TranscriptBlock::Thinking {
                text: "先定位".into()
            }
        );
        assert!(matches!(
            entries[3].blocks[0],
            TranscriptBlock::ToolCall { .. }
        ));
        assert_eq!(
            entries[4].blocks[0],
            TranscriptBlock::ToolResult {
                call_id: Some("c1".into()),
                content: "file.rs".into(),
                is_error: false,
            }
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
