//! dsh 适配器：解析 `~/.dsh/sessions/**/session*.jsonl.zstd`——zstd 压缩的 JSONL
//! 会话日志（见 [`Adapter::decode`]）。同级的 `settings.yaml`、`mcp.json`、
//! `.credentials.yaml`、`profiles/` 是配置与凭据，不在扫描范围，绝不打开。
//!
//! 格式（本机 33 个真实文件验证）：每个会话一个目录，内含一个 zstd 压缩的 JSONL；
//! 首行 `{"type":"session","id":…,"cwd":…,"createdAt":epoch ms}`，与 pi 同源
//! （目录布局、版本号习惯一致）。**本机的 dsh 只写会话头，从不写消息行**——
//! 消息行的解析按 pi 的行格式实现（`type:"message"` + `message.role/model/usage`，
//! usage 桶名 `input/output/cacheRead/cacheWrite/reasoning`，reasoning 不计入
//! output）：这是有依据的推测而非实测，将来 dsh 真写出用量行时本适配器自动
//! 生效；写不出就只登记会话、不计用量，无害。

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::transcript::{self, TranscriptEntry};
use super::{Adapter, LineFacts, LineParse, UsageFacts};
use crate::error::{Error, Result};
use crate::model::{TokenUsage, Tool};

/// 会话标题取首条用户消息文本，超长截断——与 pi 适配器同一约定。
const TITLE_MAX_CHARS: usize = 200;

/// dsh 的适配器实现；文件级解析 + zstd 解压，见模块文档。
pub struct DshAdapter;

impl Adapter for DshAdapter {
    fn tool(&self) -> Tool {
        Tool::Dsh
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        match crate::paths::home_dir() {
            Some(home) => vec![home.join(".dsh").join("sessions")],
            None => vec![],
        }
    }

    /// 只认 `.jsonl.zstd`：裸 `.jsonl` 不是本工具的会话日志形态。
    fn is_session_log(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".jsonl.zstd"))
    }

    /// 会话 = 整个目录。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        source_file
            .parent()
            .map(|dir| vec![dir.to_path_buf()])
            .unwrap_or_default()
    }

    /// zstd 解压；坏帧返回 `None`，扫描层跳过该文件、不推进游标。
    fn decode<'a>(&self, raw: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(raw).ok()?;
        let mut out = Vec::with_capacity(raw.len() * 8);
        decoder.read_to_end(&mut out).ok()?;
        Some(Cow::Owned(out))
    }

    /// 会话 id 与 cwd 在文件头的 session 行里，消息行不带——文件级解析，
    /// 与 pi 同构（游标前缀里的头行也算）。
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

    /// scan 走 [`Self::parse_file`]；无会话头上下文时消息行无法归属，一律 Skip。
    fn parse_line(&self, line: &str) -> LineParse {
        SessionCtx::default().facts_for(line)
    }

    /// 与 pi 同一条提取路径，先 zstd 解压（见 [`Self::decode`]）。
    fn read_transcript(
        &self,
        source_file: &Path,
        external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let bytes = std::fs::read(source_file).map_err(|source| Error::DataFile {
            path: source_file.to_path_buf(),
            source,
        })?;
        let Some(decoded) = Adapter::decode(self, &bytes) else {
            return Err(Error::Internal("session log decompression failed".into()));
        };
        let text = String::from_utf8_lossy(&decoded);
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
        let Some(session_external_id) = self.session_id.clone() else {
            return LineParse::Skip;
        };

        let title = (message.role == "user")
            .then(|| first_text(&message.content))
            .flatten()
            .map(truncate_title);
        // dsh 头行的 createdAt 是 epoch ms；消息行沿用 pi 的内层时间戳习惯。
        let ts_ms = message
            .timestamp
            .or_else(|| parse_outer_ts(raw.timestamp.as_deref()));
        let usage = (message.role == "assistant")
            .then(|| {
                message
                    .usage
                    .and_then(|u| usage_facts(u, message.model.as_deref().unwrap_or(""), ts_ms))
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

/// 全零用量无事实价值，跳过；时间戳缺失的用量不落库（schema 非空，绝不编造）。
fn usage_facts(u: RawUsage, model_raw: &str, ts_ms: Option<i64>) -> Option<UsageFacts> {
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
        // 与 pi 同源：模型名假定已是规范形，不做归一。
        model: model_raw.to_string(),
        duration_ms: None,
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

    const HEADER: &str = r#"{"type":"session","version":3,"id":"058e68ae-a931","createdAt":1789200656161,"cwd":"E:\\proj\\demo","delegationDepth":0}"#;

    const USER: &str = r#"{"type":"message","id":"m1","timestamp":"2026-09-12T08:10:45.296Z","message":{"role":"user","content":[{"type":"text","text":"帮我看看这个"}],"timestamp":1789200645293}}"#;

    const ASSISTANT: &str = r#"{"type":"message","id":"m2","timestamp":"2026-09-12T08:10:55.830Z","message":{"role":"assistant","model":"deepseek-v4-flash","usage":{"input":20209,"output":150,"cacheRead":64768,"cacheWrite":0,"reasoning":29,"totalTokens":85127},"timestamp":1789200652741}}"#;

    /// 手工构造一个 zstd 原始块帧（ruzstd 只解不压，测试用最小合法帧）：
    /// magic + 帧头(单段 + 4 字节内容大小) + 原始块头 + 载荷。
    fn zstd_frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0x28, 0xB5, 0x2F, 0xFD, 0xA0];
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let header = 1u32 | ((payload.len() as u32) << 3); // last_block | raw | size
        out.extend_from_slice(&header.to_le_bytes()[..3]);
        out.extend_from_slice(payload);
        out
    }

    fn parse_file(lines: &[&str]) -> Vec<LineParse> {
        DshAdapter.parse_file(lines, 0)
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
        assert_eq!(user.session_external_id, "058e68ae-a931");
        assert_eq!(user.project_dir.as_deref(), Some("E:\\proj\\demo"));
        assert_eq!(user.title.as_deref(), Some("帮我看看这个"));
        assert_eq!(user.usage, None);

        let assistant = facts(&parsed, 2);
        let usage = assistant.usage.as_ref().expect("assistant 行必须有用量");
        assert_eq!(usage.model_raw, "deepseek-v4-flash");
        assert_eq!(usage.ts_ms, 1_789_200_652_741);
        assert_eq!(usage.usage.input_tokens, 20_209);
        assert_eq!(usage.usage.output_tokens, 150);
        assert_eq!(usage.usage.cache_read_tokens, 64_768);
        assert_eq!(usage.usage.reasoning_tokens, Some(29));
    }

    #[test]
    fn compressed_file_decodes_to_lines() {
        let payload = format!("{HEADER}\n{ASSISTANT}\n");
        let frame = zstd_frame(payload.as_bytes());
        let decoded = DshAdapter.decode(&frame).expect("合法帧必须解出");
        let text = std::str::from_utf8(&decoded).unwrap();
        assert_eq!(text.lines().count(), 2);

        // 坏帧返回 None：扫描层跳过文件、不推进游标。
        assert_eq!(DshAdapter.decode(b"not a zstd frame"), None);
    }

    #[test]
    fn header_only_files_produce_no_facts() {
        // 本机实测的 dsh 现状：只有会话头，没有消息行——不产生任何事实。
        let parsed = parse_file(&[HEADER]);
        assert!(parsed.iter().all(|p| p == &LineParse::Skip));
    }

    #[test]
    fn lines_without_header_or_garbage_are_handled() {
        assert_eq!(parse_file(&[USER])[0], LineParse::Skip, "无头行不编造归属");
        let parsed = parse_file(&[HEADER, "{ not json"]);
        assert_eq!(parsed[1], LineParse::Malformed);
    }

    #[test]
    fn only_compressed_jsonl_counts_as_session_log() {
        let adapter = DshAdapter;
        assert!(adapter.is_session_log(Path::new("/s/session.v3.jsonl.zstd")));
        assert!(adapter.is_session_log(Path::new("/s/session.jsonl.zstd")));
        assert!(!adapter.is_session_log(Path::new("/s/notes.txt")));
        assert!(!adapter.is_session_log(Path::new("/s/settings.yaml")));
    }
}
