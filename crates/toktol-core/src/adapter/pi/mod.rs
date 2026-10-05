//! pi 适配器：解析 `~/.pi/agent/sessions/**/*.jsonl` 会话日志。
//! 红线：只碰 `sessions/` 下的会话日志；同级的 `auth.json`、`settings.json`、
//! `models.json` 等配置与凭据文件不在扫描范围，绝不打开。
//!
//! 格式（实测）：首行 `{"type":"session","id":…,"cwd":…}`，之后是 `type:"message"`
//! 的 user/assistant 行——**消息行不带会话 id**，只能文件级解析（继承最近的
//! session 头，游标前缀里的也算，见 [`Adapter::parse_file`] 的契约）。
//! 用量天然分桶：`input` / `output` / `cacheRead` / `cacheWrite`，`reasoning`
//! 单列且不计入 `totalTokens`（实测 total = input + output + cache 之和），
//! 与统一模型"输出不含推理"的口径一致。内层 `message.timestamp` 直接是 epoch ms。
//! 请求耗时：内层时间戳是请求发起，外层行时间戳是响应完成落盘，差值即耗时
//! （实测全量分布合理）；负差值不编造。行内自洽，无需跨行状态。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::transcript::{self, TranscriptEntry};
use super::{Adapter, LineFacts, LineParse, UsageFacts};
use crate::error::Result;
use crate::model::{TokenUsage, Tool};

/// 会话标题取首条用户消息文本，超长截断——与 claude-code 适配器同一约定。
const TITLE_MAX_CHARS: usize = 200;

/// pi 的适配器实现；文件级解析的原因见模块文档。
pub struct PiAdapter;

impl Adapter for PiAdapter {
    fn tool(&self) -> Tool {
        Tool::Pi
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        match crate::paths::home_dir() {
            Some(home) => vec![home.join(".pi").join("agent").join("sessions")],
            None => vec![],
        }
    }

    fn is_session_log(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
    }

    /// 会话 = 整个目录（日志只是其中之一的入口文件）。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        source_file
            .parent()
            .map(|dir| vec![dir.to_path_buf()])
            .unwrap_or_default()
    }

    /// 会话 id 只在 session 头行里，逐行解析无从归属——覆盖为文件级解析。
    fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
        let mut ctx = SessionCtx::default();
        for line in &lines[..start] {
            ctx.absorb(line);
        }
        lines[start..]
            .iter()
            .map(|line| ctx.facts_for(line))
            .collect()
    }

    /// scan 走 [`Self::parse_file`]；本实现仅满足 trait 完整性。
    /// 没有会话头上下文时消息行无法归属会话，一律 Skip——绝不编造归属。
    fn parse_line(&self, line: &str) -> LineParse {
        SessionCtx::default().facts_for(line)
    }

    /// 转录提取与扫描同构：头行定归属，只保留目标会话的消息。
    fn read_transcript(
        &self,
        source_file: &Path,
        external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let text = transcript::read_text(source_file)?;
        let lines: Vec<&str> = text.lines().collect();
        Ok(transcript::pi_style_transcript(&lines, external_id))
    }
}

/// 文件内游标状态：最近的 session 头。
#[derive(Default)]
struct SessionCtx {
    session_id: Option<String>,
    cwd: Option<String>,
}

impl SessionCtx {
    /// 从游标前缀重建状态：只认 session 头行。
    fn absorb(&mut self, line: &str) {
        if let Ok(header) = serde_json::from_str::<SessionHeader>(line) {
            self.apply(&header.id, header.cwd);
        }
    }

    fn apply(&mut self, id: &str, cwd: Option<String>) {
        if !id.is_empty() {
            self.session_id = Some(id.to_string());
            if cwd.is_some() {
                self.cwd = cwd;
            }
        }
    }

    fn facts_for(&mut self, line: &str) -> LineParse {
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(_) => return LineParse::Malformed,
        };

        if raw.kind == "session" {
            if let Some(id) = &raw.id {
                self.apply(id, raw.cwd);
            }
            return LineParse::Skip; // 头行本身无事实；会话行等第一条事实再落库。
        }
        if raw.kind != "message" {
            return LineParse::Skip;
        }
        let Some(message) = raw.message else {
            return LineParse::Skip;
        };
        // 消息行不带会话 id；文件里从未出现过 session 头时无法归属，静默跳过。
        let Some(session_external_id) = self.session_id.clone() else {
            return LineParse::Skip;
        };

        let title = (message.role == "user")
            .then(|| first_text(&message.content))
            .flatten()
            .map(truncate_title);
        let ts_ms = message
            .timestamp
            .or_else(|| parse_outer_ts(raw.timestamp.as_deref()));
        // 请求耗时：内层时间戳（请求发起）→ 外层行时间戳（响应完成落盘）。
        let duration_ms = match (message.timestamp, raw.timestamp.as_deref()) {
            (Some(inner), Some(outer)) => parse_outer_ts(Some(outer))
                .and_then(|outer| outer.checked_sub(inner))
                .filter(|delta| *delta >= 0),
            _ => None,
        };
        let usage = (message.role == "assistant")
            .then(|| {
                message.usage.and_then(|u| {
                    usage_facts(
                        u,
                        message.model.as_deref().unwrap_or(""),
                        ts_ms,
                        duration_ms,
                    )
                })
            })
            .flatten();

        if title.is_none() && usage.is_none() {
            return LineParse::Skip;
        }

        LineParse::Facts(Box::new(LineFacts {
            dedup_suffix: None,
            request_count: 1,
            session_external_id,
            title,
            project_dir: self.cwd.clone(),
            ts_ms,
            usage,
        }))
    }
}

/// 全零用量是真实存在的（部分网关不回计费数据），无事实价值，跳过。
/// 时间戳取自内层 `message.timestamp`（epoch ms）；缺失则不产出用量事实。
fn usage_facts(
    u: RawUsage,
    model_raw: &str,
    ts_ms: Option<i64>,
    duration_ms: Option<i64>,
) -> Option<UsageFacts> {
    let model_raw = model_raw.trim();
    if model_raw.is_empty() {
        return None;
    }
    let reasoning = u.reasoning.filter(|&n| n > 0);
    let usage = TokenUsage {
        input_tokens: u.input.max(0),
        output_tokens: u.output.max(0),
        cache_read_tokens: u.cache_read.max(0),
        cache_write_tokens: u.cache_write.max(0),
        reasoning_tokens: reasoning,
    };
    if usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_read_tokens == 0
        && usage.cache_write_tokens == 0
    {
        return None;
    }
    Some(UsageFacts {
        ts_ms: ts_ms?,
        model_raw: model_raw.to_string(),
        // pi 的模型名已是供应商规范形（如 deepseek-v4-flash），不归一。
        model: model_raw.to_string(),
        duration_ms,
        usage,
    })
}

fn parse_outer_ts(ts: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts?)
        .ok()
        .map(|t| t.timestamp_millis())
}

fn first_text(content: &Option<serde_json::Value>) -> Option<String> {
    match content {
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
struct SessionHeader {
    #[serde(default)]
    id: String,
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    /// 外层行时间戳（ISO 字符串）；内层 message.timestamp 是 epoch ms，优先。
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    /// 内层时间戳：epoch ms 整数。
    #[serde(default)]
    timestamp: Option<i64>,
}

#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input: i64,
    #[serde(default)]
    output: i64,
    #[serde(rename = "cacheRead", default)]
    cache_read: i64,
    #[serde(rename = "cacheWrite", default)]
    cache_write: i64,
    #[serde(default)]
    reasoning: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"type":"session","version":3,"id":"01a094ab-349f","timestamp":"2026-09-12T08:10:43.231Z","cwd":"E:\\proj\\demo"}"#;

    const USER: &str = r#"{"type":"message","id":"fac9f111","timestamp":"2026-09-12T08:10:45.296Z","message":{"role":"user","content":[{"type":"text","text":"查看当前项目内容"}],"timestamp":1789200645293}}"#;

    const ASSISTANT: &str = r#"{"type":"message","id":"9b7754be","timestamp":"2026-09-12T08:10:55.830Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"…"}],"api":"openai-completions","provider":"newapi","model":"deepseek-v4-flash","usage":{"input":20209,"output":150,"cacheRead":64768,"cacheWrite":0,"reasoning":29,"totalTokens":85127,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":1789200652741}}"#;

    fn parse_file(lines: &[&str]) -> Vec<LineParse> {
        PiAdapter.parse_file(lines, 0)
    }

    fn facts(parsed: &[LineParse], index: usize) -> &LineFacts {
        match &parsed[index] {
            LineParse::Facts(facts) => facts,
            other => panic!("第 {index} 行期望 Facts，实际 {other:?}"),
        }
    }

    #[test]
    fn session_header_attaches_id_and_cwd_to_message_lines() {
        let parsed = parse_file(&[HEADER, USER, ASSISTANT]);

        let user = facts(&parsed, 1);
        assert_eq!(
            user.session_external_id, "01a094ab-349f",
            "会话 id 来自头行"
        );
        assert_eq!(user.project_dir.as_deref(), Some("E:\\proj\\demo"));
        assert_eq!(user.title.as_deref(), Some("查看当前项目内容"));
        assert_eq!(user.ts_ms, Some(1_789_200_645_293), "优先内层 epoch ms");
        assert_eq!(user.usage, None);

        let assistant = facts(&parsed, 2);
        assert_eq!(assistant.title, None);
        let usage = assistant.usage.as_ref().expect("assistant 行必须有用量");
        assert_eq!(usage.model_raw, "deepseek-v4-flash");
        assert_eq!(usage.ts_ms, 1_789_200_652_741);
        assert_eq!(
            usage.duration_ms,
            Some(3_089),
            "外层落盘 55.830 − 内层发起 52.741"
        );
        assert_eq!(usage.usage.input_tokens, 20_209);
        assert_eq!(usage.usage.output_tokens, 150);
        assert_eq!(usage.usage.cache_read_tokens, 64_768);
        assert_eq!(usage.usage.cache_write_tokens, 0);
        assert_eq!(
            usage.usage.reasoning_tokens,
            Some(29),
            "reasoning 单列，不并入输出"
        );
    }

    #[test]
    fn incremental_start_rebuilds_context_from_prefix() {
        // 增量扫描：游标之前只有头行，新行只有 assistant 消息——归属不能丢。
        let parsed = PiAdapter.parse_file(&[HEADER, ASSISTANT], 1);
        assert_eq!(parsed.len(), 1);
        let assistant = facts(&parsed, 0);
        assert_eq!(assistant.session_external_id, "01a094ab-349f");
        assert_eq!(assistant.project_dir.as_deref(), Some("E:\\proj\\demo"));
    }

    #[test]
    fn header_line_after_prefix_updates_context() {
        let parsed = PiAdapter.parse_file(&[HEADER, ASSISTANT], 1);
        assert_eq!(parsed.len(), 1);

        // 文件中途换会话头（compaction 重写场景）：新头对后续行生效。
        let new_header =
            r#"{"type":"session","version":3,"id":"second-session","cwd":"E:\\other"}"#;
        let parsed = PiAdapter.parse_file(&[HEADER, new_header, ASSISTANT], 2);
        assert_eq!(facts(&parsed, 0).session_external_id, "second-session");
        assert_eq!(facts(&parsed, 0).project_dir.as_deref(), Some("E:\\other"));
    }

    #[test]
    fn lines_without_header_or_usage_are_skipped() {
        // 无头行：消息行无法归属，Skip 而不是编造会话。
        assert_eq!(parse_file(&[USER])[0], LineParse::Skip);
        // 头行自身、模型切换、纯 thinking 行都无事实。
        let parsed = parse_file(&[
            HEADER,
            r#"{"type":"model_change","model":"glm-5.3-flash"}"#,
            r#"{"type":"thinking","thinking":"…"}"#,
        ]);
        assert!(parsed.iter().all(|p| p == &LineParse::Skip));
    }

    #[test]
    fn zero_usage_and_garbage_are_handled() {
        let zero = r#"{"type":"message","timestamp":"2026-09-12T08:10:55.830Z","message":{"role":"assistant","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0}}}"#;
        assert_eq!(
            PiAdapter.parse_file(&[HEADER, zero], 1)[0],
            LineParse::Skip,
            "全零用量无事实价值"
        );
        assert_eq!(
            PiAdapter.parse_file(&[HEADER, "{ not json"], 1)[0],
            LineParse::Malformed
        );
    }

    #[test]
    fn title_is_trimmed_and_capped() {
        let long = "字".repeat(500);
        let line = format!(
            r#"{{"type":"message","message":{{"role":"user","content":"{long}","timestamp":1}}}}"#
        );
        let parsed = parse_file(&[HEADER, &line]);
        assert_eq!(
            facts(&parsed, 1)
                .title
                .as_deref()
                .map(|t| t.chars().count()),
            Some(200)
        );
    }

    #[test]
    fn negative_duration_and_missing_timestamps_yield_none() {
        // 内层晚于外层（异常时钟）：不编造耗时。
        let inverted = r#"{"type":"message","timestamp":"2026-09-12T08:10:50.000Z","message":{"role":"assistant","model":"m","usage":{"input":1,"output":1},"timestamp":1789200655000}}"#;
        let parsed = parse_file(&[HEADER, inverted]);
        assert_eq!(facts(&parsed, 1).usage.as_ref().unwrap().duration_ms, None);
        // 外层时间戳缺失：同样没有耗时。
        let no_outer = r#"{"type":"message","message":{"role":"assistant","model":"m","usage":{"input":1,"output":1},"timestamp":1789200655000}}"#;
        let parsed = parse_file(&[HEADER, no_outer]);
        assert_eq!(facts(&parsed, 1).usage.as_ref().unwrap().duration_ms, None);
    }

    #[test]
    fn only_jsonl_files_count_as_session_logs() {
        let adapter = PiAdapter;
        assert!(adapter.is_session_log(Path::new("/s/sess.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/s/auth.json")));
        assert!(!adapter.is_session_log(Path::new("/s/settings.json")));
    }
}
