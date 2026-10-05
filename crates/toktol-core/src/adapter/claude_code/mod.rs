//! claude-code 适配器：解析 `~/.claude/projects/**/*.jsonl` 会话日志。
//! 红线：只碰 `projects/` 下的会话日志；`~/.claude.json`、`settings.json`、
//! `.credentials.json` 等配置与凭据文件不在扫描范围，绝不打开。
//!
//! 行格式（实测）：`type` 为 user / assistant / queue-operation / summary …；
//! assistant 行带 `message.model` 与 `message.usage`（缓存桶名是
//! `cache_creation_input_tokens` / `cache_read_input_tokens`）；经 OpenAI 兼容网关
//! 转发的行模型名可以是任意供应商（如 `deepseek/…`），且输出明细里带
//! `output_tokens_details.thinking_tokens`——那是输出内推理的拆分，要拆出来单独记。
//! 请求耗时：旧版日志在 assistant 行带顶层 `durationMs`，新版（≥2.1.x 实测）已
//! 停写——改为文件级推导：assistant 行按 `message.id` 分组（同组是同一次 API
//! 请求的流式块），组内末块时间戳 − 组前最近非 assistant 行（触发行）时间戳；
//! 原生字段仍存在时优先取原生值。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::transcript::{self, TranscriptEntry, TranscriptRole};
use super::{Adapter, LineFacts, LineParse, UsageFacts};
use crate::error::Result;
use crate::model::{TokenUsage, Tool};

/// 会话标题取首条用户消息文本，超长截断——标题是提示词，不是全文备份。
const TITLE_MAX_CHARS: usize = 200;

/// claude-code 的适配器实现；无状态，见 [`super`] 模块文档的契约。
pub struct ClaudeCodeAdapter;

impl ClaudeCodeAdapter {
    /// `derived` 是跨行推导的请求耗时（毫秒），见 [`Self::parse_file`]；
    /// 顶层 `durationMs` 原生字段优先（旧版工具自测值，比推导准）。
    fn parse_line_with(&self, line: &str, derived: Option<i64>) -> LineParse {
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(_) => return LineParse::Malformed,
        };
        let session_external_id = match raw.session_id {
            Some(id) if !id.is_empty() => id,
            _ => return LineParse::Skip,
        };

        // sidechain（subagent）行的首条消息是任务指派文本，不当标题。
        let title = if raw.kind == "user" && !raw.is_sidechain {
            first_user_text(&raw.message).map(truncate_title)
        } else {
            None
        };

        let duration = raw.duration_ms.map(|d| d.max(0.0) as i64).or(derived);
        let usage = match (raw.message.as_ref(), raw.timestamp.as_deref()) {
            (Some(message), Some(ts)) => message
                .usage
                .as_ref()
                .and_then(|u| usage_facts(u, message, duration, ts)),
            _ => None,
        };

        if title.is_none() && usage.is_none() {
            return LineParse::Skip;
        }

        LineParse::Facts(Box::new(LineFacts {
            dedup_suffix: None,
            request_count: 1,
            session_external_id,
            title,
            project_dir: raw.cwd,
            ts_ms: raw.timestamp.as_deref().and_then(parse_ts_ms),
            usage,
        }))
    }
}

impl Adapter for ClaudeCodeAdapter {
    fn tool(&self) -> Tool {
        Tool::ClaudeCode
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        match crate::paths::home_dir() {
            Some(home) => vec![home.join(".claude").join("projects")],
            None => vec![],
        }
    }

    fn is_session_log(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
    }

    /// 主文件 + 同名子目录（subagents）。子目录不存在时由编排层跳过。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        let mut artifacts = vec![source_file.to_path_buf()];
        if let Some(stem) = source_file.file_stem() {
            artifacts.push(source_file.with_file_name(stem));
        }
        artifacts
    }

    fn parse_line(&self, line: &str) -> LineParse {
        self.parse_line_with(line, None)
    }

    /// 请求耗时需要跨行推导（见模块文档），走文件级解析；推导对"完整文件
    /// 内容 + start"确定。扫描层游标增量重扫时，跨游标仍打开的分组按当前
    /// 文件末行先结算：同组后到的块会让新行拿到更完整的耗时，旧行保持已
    /// 入账值不动（dedup 拦住，不重复入账）。
    fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
        let derived = derived_durations(lines);
        lines[start..]
            .iter()
            .zip(derived[start..].iter())
            .map(|(line, duration)| self.parse_line_with(line, *duration))
            .collect()
    }

    /// 消息正文只在用户点开会话详情时现读，绝不落库（与扫描的"只入账用量
    /// 事实"互补）。user 行的内容块常是 tool_result，归一后如实展示。
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
            // fork/派生可能把别的会话写进同一文件：按行内 sessionId 过滤。
            if raw.session_id.as_ref().is_some_and(|id| id != external_id) {
                continue;
            }
            let role = match raw.kind.as_str() {
                "user" => TranscriptRole::User,
                "assistant" => TranscriptRole::Assistant,
                _ => continue,
            };
            let Some(message) = raw.message else {
                continue;
            };
            let empty = serde_json::Value::Null;
            let blocks =
                transcript::blocks_from_content(message.content.as_ref().unwrap_or(&empty));
            if blocks.is_empty() {
                continue; // 纯状态行（queue-operation 等）没有可展示内容。
            }
            entries.push(TranscriptEntry {
                role,
                ts_ms: raw.timestamp.as_deref().and_then(parse_ts_ms),
                model: message.model.filter(|m| {
                    let trimmed = m.trim();
                    !trimmed.is_empty() && !is_placeholder_model(trimmed)
                }),
                blocks,
            });
        }
        Ok(entries)
    }
}

fn usage_facts(
    u: &RawUsage,
    m: &RawMessage,
    duration_ms: Option<i64>,
    ts: &str,
) -> Option<UsageFacts> {
    let model_raw = m.model.as_deref()?.trim();
    if model_raw.is_empty() || is_placeholder_model(model_raw) {
        return None;
    }
    let ts_ms = parse_ts_ms(ts)?;

    // OpenAI 风格：output_tokens 含推理，thinking_tokens 是其拆分明细；
    // Claude 原生日志没有该字段，视为无推理。输出桶一律记扣掉推理后的净量。
    let thinking = u.details.as_ref().map_or(0, |d| d.thinking_tokens.max(0));
    let reasoning = (thinking > 0).then_some(thinking);

    Some(UsageFacts {
        ts_ms,
        model_raw: model_raw.to_string(),
        model: normalize_model(model_raw),
        duration_ms,
        usage: TokenUsage {
            input_tokens: u.input_tokens.max(0),
            output_tokens: (u.output_tokens.max(0) - thinking).max(0),
            cache_read_tokens: u.cache_read.max(0),
            cache_write_tokens: u.cache_creation.max(0),
            reasoning_tokens: reasoning,
        },
    })
}

/// 一次 API 请求的流式块分组：同一 `message.id` 的连续 assistant 行。
struct DurationGroup {
    id: String,
    last_ts: i64,
    members: Vec<usize>,
}

/// 文件级请求耗时推导（见模块文档）。返回每行的推导耗时：assistant 行按
/// `message.id` 分组，组内末块时间戳 − 组前最近非 assistant 行时间戳；负差值
/// （实测约 3% 行的时间戳轻微乱序，多来自 queue-operation 插行）不编造，留空。
fn derived_durations(lines: &[&str]) -> Vec<Option<i64>> {
    let mut out = vec![None; lines.len()];
    let mut trigger: Option<i64> = None;
    let mut group: Option<DurationGroup> = None;
    for (index, line) in lines.iter().enumerate() {
        let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
            continue;
        };
        let ts = raw.timestamp.as_deref().and_then(parse_ts_ms);
        if raw.kind != "assistant" {
            if let Some(g) = group.take() {
                settle_group(g, trigger, &mut out);
            }
            if let Some(ts) = ts {
                trigger = Some(ts);
            }
            continue;
        }
        let id = raw.message.as_ref().and_then(|m| m.id.as_deref());
        let (Some(id), Some(ts)) = (id, ts) else {
            // 无 message.id 或无时间的 assistant 行不成组，只打断当前分组。
            if let Some(g) = group.take() {
                settle_group(g, trigger, &mut out);
            }
            continue;
        };
        match &mut group {
            Some(g) if g.id == id => {
                g.last_ts = g.last_ts.max(ts);
                g.members.push(index);
            }
            _ => {
                if let Some(g) = group.take() {
                    settle_group(g, trigger, &mut out);
                }
                group = Some(DurationGroup {
                    id: id.to_string(),
                    last_ts: ts,
                    members: vec![index],
                });
            }
        }
    }
    if let Some(g) = group.take() {
        settle_group(g, trigger, &mut out);
    }
    out
}

/// 分组结算：耗时 = 组末块时间戳 − 触发行时间戳，派给组内每行。
fn settle_group(group: DurationGroup, trigger: Option<i64>, out: &mut [Option<i64>]) {
    if let Some(trigger) = trigger
        && group.last_ts >= trigger
    {
        let duration = group.last_ts - trigger;
        for index in group.members {
            out[index] = Some(duration);
        }
    }
}

/// `<synthetic>` 等尖括号占位"模型"是工具自己合成的提示/错误消息，不是真实请求。
fn is_placeholder_model(model: &str) -> bool {
    model.starts_with('<')
}

fn parse_ts_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// `claude-*-YYYYMMDD` 去掉日期后缀，让价格目录按模型族归并；其它供应商的名字
/// （如 `deepseek/…`）原样保留——不知道怎么归一就不乱动。
fn normalize_model(raw: &str) -> String {
    if let Some(base) = raw.strip_prefix("claude-")
        && let Some((head, tail)) = base.rsplit_once('-')
        && tail.len() == 8
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("claude-{head}");
    }
    raw.to_string()
}

fn first_user_text(message: &Option<RawMessage>) -> Option<String> {
    match &message.as_ref()?.content {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Array(blocks)) => blocks.iter().find_map(|block| {
            let obj = block.as_object()?;
            (obj.get("type")?.as_str()? == "text")
                .then(|| obj.get("text")?.as_str().map(str::to_string))?
        }),
        _ => None,
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

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    // f64 收数：个别版本写出小数毫秒时整行解析不至于失败。
    #[serde(rename = "durationMs", default)]
    duration_ms: Option<f64>,
    #[serde(default)]
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    content: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(rename = "cache_creation_input_tokens", default)]
    cache_creation: i64,
    #[serde(rename = "cache_read_input_tokens", default)]
    cache_read: i64,
    #[serde(rename = "output_tokens_details")]
    details: Option<OutputDetails>,
}

#[derive(Deserialize)]
struct OutputDetails {
    #[serde(rename = "thinking_tokens", default)]
    thinking_tokens: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> LineFacts {
        match ClaudeCodeAdapter.parse_line(line) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        }
    }

    const ASSISTANT: &str = r#"{"parentUuid":null,"isSidechain":false,"message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5-20250929","usage":{"input_tokens":1041,"cache_creation_input_tokens":5230,"cache_read_input_tokens":32095,"output_tokens":133}},"type":"assistant","timestamp":"2026-09-12T07:17:14.572Z","cwd":"E:\\proj\\demo","sessionId":"3bfaeb77"}"#;

    #[test]
    fn assistant_line_yields_usage_and_session() {
        let facts = parse(ASSISTANT);
        assert_eq!(facts.session_external_id, "3bfaeb77");
        assert_eq!(facts.project_dir.as_deref(), Some("E:\\proj\\demo"));
        assert_eq!(facts.title, None);

        let usage = facts.usage.expect("assistant 行必须有用量");
        assert_eq!(usage.model_raw, "claude-sonnet-4-5-20250929");
        assert_eq!(usage.model, "claude-sonnet-4-5", "日期后缀要去掉");
        assert_eq!(usage.ts_ms, 1_789_197_434_572);
        assert_eq!(usage.usage.input_tokens, 1041);
        assert_eq!(usage.usage.cache_write_tokens, 5230);
        assert_eq!(usage.usage.cache_read_tokens, 32095);
        assert_eq!(usage.usage.output_tokens, 133);
        assert_eq!(usage.usage.reasoning_tokens, None);
    }

    #[test]
    fn thinking_tokens_are_split_out_of_output() {
        let line = r#"{"type":"assistant","sessionId":"s","timestamp":"2026-01-01T00:00:00Z","message":{"model":"deepseek/deepseek-v4-flash-0731","usage":{"input_tokens":10,"output_tokens":37,"output_tokens_details":{"thinking_tokens":30}}}}"#;
        let facts = parse(line);
        let usage = facts.usage.unwrap();
        assert_eq!(
            usage.model, "deepseek/deepseek-v4-flash-0731",
            "非 claude 名不归一"
        );
        assert_eq!(usage.usage.output_tokens, 7, "输出记净量");
        assert_eq!(usage.usage.reasoning_tokens, Some(30));
    }

    #[test]
    fn first_user_text_from_content_array_becomes_title() {
        let user = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"  帮我看看这个 bug  "}]},"sessionId":"s1","timestamp":"2026-01-01T00:00:00Z"}"#;
        let facts = parse(user);
        assert_eq!(
            facts.title.as_deref(),
            Some("帮我看看这个 bug"),
            "去首尾空白"
        );
        assert_eq!(facts.usage, None, "用户行没有用量");
        assert_eq!(facts.ts_ms, Some(1_767_225_600_000));
    }

    #[test]
    fn lines_without_facts_are_skipped() {
        // sidechain 用户行与 tool_result 行既无用量也不作标题候选。
        let sidechain = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"子任务指派"},"sessionId":"s1"}"#;
        assert_eq!(ClaudeCodeAdapter.parse_line(sidechain), LineParse::Skip);
        let tool_result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]},"sessionId":"s1"}"#;
        assert_eq!(ClaudeCodeAdapter.parse_line(tool_result), LineParse::Skip);
    }

    #[test]
    fn non_usage_lines_are_skipped_and_garbage_is_malformed() {
        let queue = r#"{"type":"queue-operation","operation":"enqueue","timestamp":"2026-09-12T07:17:05.449Z","sessionId":"3bfaeb77"}"#;
        assert_eq!(ClaudeCodeAdapter.parse_line(queue), LineParse::Skip);
        assert_eq!(
            ClaudeCodeAdapter.parse_line("{ not json"),
            LineParse::Malformed
        );
        // 无 sessionId 的行无法归属会话，静默跳过。
        assert_eq!(
            ClaudeCodeAdapter
                .parse_line(r#"{"type":"assistant","message":{"model":"m","usage":{}}}"#),
            LineParse::Skip
        );
    }

    #[test]
    fn title_is_trimmed_and_capped() {
        let long = "字".repeat(500);
        let line = format!(
            r#"{{"type":"user","message":{{"role":"user","content":"{long}"}},"sessionId":"s"}}"#
        );
        assert_eq!(parse(&line).title.map(|t| t.chars().count()), Some(200));
    }

    #[test]
    fn synthetic_placeholder_models_are_not_usage() {
        let line = r#"{"type":"assistant","sessionId":"s","timestamp":"2026-01-01T00:00:00Z","message":{"model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":5}}}"#;
        assert_eq!(ClaudeCodeAdapter.parse_line(line), LineParse::Skip);
    }

    #[test]
    fn normalize_keeps_non_dated_claude_names_intact() {
        assert_eq!(
            normalize_model("claude-opus-4-1-20250801"),
            "claude-opus-4-1"
        );
        assert_eq!(normalize_model("claude-sonnet-4-5"), "claude-sonnet-4-5");
        assert_eq!(
            normalize_model("gpt-5.1-20260101"),
            "gpt-5.1-20260101",
            "只归一 claude 前缀"
        );
    }

    #[test]
    fn only_jsonl_files_count_as_session_logs() {
        let adapter = ClaudeCodeAdapter;
        assert!(adapter.is_session_log(Path::new("/p/sess.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/p/MEMORY.md")));
        assert!(!adapter.is_session_log(Path::new("/p/settings.json")));
    }

    #[test]
    fn transcript_reads_messages_in_order_and_filters_other_sessions() {
        let dir = std::env::temp_dir().join(format!("toktol-cc-tr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        std::fs::write(
            &path,
            [
                r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"帮我看看"}}"#,
                r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-01-01T00:00:05Z","message":{"model":"claude-sonnet-4-5","content":[{"type":"tool_use","id":"t1","name":"bash","input":{"cmd":"ls"}}]}}"#,
                // 占位"模型"是合成提示行，不作为模型标签展示。
                r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-01-01T00:00:07Z","message":{"model":"<synthetic>","content":[{"type":"text","text":"请求失败"}]}}"#,
                // 工具结果在 user 行里；另一会话与状态行应被过滤。
                r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-01T00:00:09Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#,
                r#"{"type":"user","sessionId":"other","message":{"role":"user","content":"别的会话"}}"#,
                r#"{"type":"queue-operation","sessionId":"s1","operation":"enqueue"}"#,
                "{ not json",
            ]
            .join("\n"),
        )
        .unwrap();

        let entries = ClaudeCodeAdapter.read_transcript(&path, "s1").unwrap();
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(
            entries[0].blocks,
            vec![transcript::TranscriptBlock::Text {
                text: "帮我看看".into()
            }]
        );
        assert_eq!(entries[1].role, TranscriptRole::Assistant);
        assert_eq!(entries[1].model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(matches!(
            entries[1].blocks[0],
            transcript::TranscriptBlock::ToolCall { .. }
        ));
        assert_eq!(
            entries[2].model, None,
            "占位'模型'不作为标签展示，但正文保留"
        );
        assert_eq!(
            entries[3].ts_ms,
            Some(1_767_225_609_000),
            "工具结果行带时间"
        );
        assert!(matches!(
            entries[3].blocks[0],
            transcript::TranscriptBlock::ToolResult { .. }
        ));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcript_missing_file_is_data_file_error() {
        let err = ClaudeCodeAdapter
            .read_transcript(Path::new("/nonexistent/s.jsonl"), "s1")
            .unwrap_err();
        assert_eq!(err.code().as_str(), "core.data_file");
    }

    fn assistant_line(mid: &str, ts: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"s","timestamp":"{ts}","message":{{"id":"{mid}","model":"claude-sonnet-4-5","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
        )
    }

    fn user_line(ts: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"s","timestamp":"{ts}","message":{{"role":"user","content":"问题"}}}}"#
        )
    }

    fn durations_of(lines: &[String]) -> Vec<Option<i64>> {
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        ClaudeCodeAdapter
            .parse_file(&refs, 0)
            .into_iter()
            .filter_map(|parsed| match parsed {
                LineParse::Facts(facts) => facts.usage.map(|u| u.duration_ms),
                _ => None,
            })
            .collect()
    }

    const T0: &str = "2026-01-01T00:00:00.000Z";
    const T1: &str = "2026-01-01T00:00:03.000Z";
    const T2: &str = "2026-01-01T00:00:07.000Z";
    const T3: &str = "2026-01-01T00:00:11.000Z";

    /// 耗时推导：同 message.id 的各块共享一次请求的耗时——组前最近触发行
    /// （用户提示/工具结果）到组内末块的时间差；工具结果行把触发行推进。
    #[test]
    fn duration_is_derived_from_trigger_to_last_block_per_message_group() {
        let lines = vec![
            user_line(T0),
            assistant_line("m1", T1),
            assistant_line("m1", T2),
            user_line(T3), // 工具结果也是 user 行：下一组的触发行。
            assistant_line("m2", "2026-01-01T00:00:15.000Z"),
        ];
        assert_eq!(
            durations_of(&lines),
            vec![Some(7_000), Some(7_000), Some(4_000)],
            "m1 两块共享 T2−T0；m2 从工具结果起算"
        );
    }

    /// 顶层 durationMs 原生字段（旧版日志）优先于推导值。
    #[test]
    fn native_duration_ms_wins_over_derived() {
        let lines = vec![
            user_line(T0),
            format!(
                r#"{{"type":"assistant","sessionId":"s","timestamp":"{T1}","durationMs":1234.0,"message":{{"id":"m1","model":"claude-sonnet-4-5","usage":{{"input_tokens":10,"output_tokens":5}}}}}}"#
            ),
        ];
        assert_eq!(durations_of(&lines), vec![Some(1_234)]);
    }

    /// 时间戳乱序导致负差值时不编造耗时（留空 = 前端显示无数据）。
    #[test]
    fn negative_delta_yields_no_duration() {
        let lines = vec![
            user_line(T2),
            assistant_line("m1", T1), // 早于触发行：乱序。
        ];
        assert_eq!(durations_of(&lines), vec![None]);
    }

    /// 增量扫描：跨游标仍打开的分组在前缀里重建，追加的后续块按新末块
    /// 结算耗时；旧行保持首次入账值（dedup 拦住重写）。
    #[test]
    fn incremental_scan_derives_duration_across_cursor() {
        struct ScopedClaudeCode(PathBuf);

        impl Adapter for ScopedClaudeCode {
            fn tool(&self) -> Tool {
                Tool::ClaudeCode
            }
            fn log_dirs(&self) -> Vec<PathBuf> {
                vec![self.0.clone()]
            }
            fn is_session_log(&self, path: &Path) -> bool {
                ClaudeCodeAdapter.is_session_log(path)
            }
            fn parse_line(&self, line: &str) -> LineParse {
                ClaudeCodeAdapter.parse_line(line)
            }
            fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
                ClaudeCodeAdapter.parse_file(lines, start)
            }
        }

        let dir = std::env::temp_dir().join(format!("toktol-cc-dur-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.jsonl");
        let block = |ts: &str| assistant_line("m1", ts);
        std::fs::write(&log, format!("{}\n{}\n", user_line(T0), block(T1))).unwrap();

        let path = crate::storage::open(
            &std::env::temp_dir().join(format!("toktol-cc-dur-{}.db", std::process::id())),
        )
        .unwrap();
        let scan = |report: &mut crate::scan::ScanReport| {
            crate::scan::scan_adapter(&path, &ScopedClaudeCode(dir.clone()), report).unwrap();
        };
        let mut report = crate::scan::ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 1);

        // 追加同组第二块：新行按新末块（T2）结算，旧行保持 T1 结算值。
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str(&block(T2));
        text.push('\n');
        std::fs::write(&log, text).unwrap();
        let mut report = crate::scan::ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 1);

        let durations: Vec<Option<i64>> = path
            .conn()
            .prepare("SELECT duration_ms FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(durations, vec![Some(3_000), Some(7_000)]);

        std::fs::remove_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(
            std::env::temp_dir().join(format!("toktol-cc-dur-{}.db", std::process::id())),
        );
    }
}
