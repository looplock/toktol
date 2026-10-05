//! opencode 适配器：从 `~/.local/share/opencode/opencode.db`（工具自有的 SQLite，
//! WAL 模式）读取会话与用量。没有可切行的日志文件，走 [`Adapter::read_db_source`]
//! 每轮全量读取，指纹未变则整轮跳过，重扫入账靠行键去重。
//! 红线：库里只 SELECT `session` / `message` 两张表，`credential`、`account` 等
//! 凭据表绝不触碰；`auth.json`、`*.bak` 快照、`snapshot/`、`log/`、`bin/` 一律
//! 不打开。
//!
//! 实测口径（3.5k 条 assistant 消息验证）：`message.data` 是 JSON，assistant 行带
//! `tokens{input, output, reasoning, cache{read, write}}` 与 `modelID`，关系为
//! total = input + output + reasoning + cache.read——`input` 不含缓存读取，
//! `reasoning` 与 `output` 相加（与 codex 的子集关系相反），分桶原样映射，输出
//! 不扣减；`total` 在部分消息上缺失，永不依赖。`session` 表自带标题与项目目录；
//! 所有时间戳一律 epoch 毫秒。子会话（parent_id 非空）是独立会话，工具自己不
//! 向父会话归并，照原样分立。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::transcript::{self, TranscriptBlock, TranscriptEntry, TranscriptRole};
use super::{Adapter, DbSource, LineFacts, UsageFacts};
use crate::error::{Error, Result};
use crate::model::{TokenUsage, Tool};

/// 会话标题超长截断——标题是名字，不是全文备份。
const TITLE_MAX_CHARS: usize = 200;

/// 删除导出的白名单从表：随 `session_id` 级联的表（part 的外键在 message 上、
/// 靠它级联，但自带 session_id 列，直接按列导出）。会话行本身随删除语句单独
/// 导出，不在此列。只认名单——白名单之外，尤其凭据类表，绝不触碰。
/// 名单来自实测 schema（2026-10）；新版本加表时由级联兜底删行，导出略过即可。
const DELETE_EXPORT_TABLES: [(&str, &str); 7] = [
    ("message", "session_id"),
    ("part", "session_id"),
    ("session_context_epoch", "session_id"),
    ("session_input", "session_id"),
    ("session_message", "session_id"),
    ("session_share", "session_id"),
    ("todo", "session_id"),
];

/// opencode 的适配器实现。`db_path` 为 `None` 时取 home 下的默认库；测试注入
/// 临时库路径。
pub struct OpenCodeAdapter {
    pub(crate) db_path: Option<PathBuf>,
}

impl Adapter for OpenCodeAdapter {
    fn tool(&self) -> Tool {
        Tool::OpenCode
    }

    /// 没有可切行的日志文件；数据全部来自 [`Self::read_db_source`]。
    fn log_dirs(&self) -> Vec<PathBuf> {
        vec![]
    }

    /// 无文件来源，任何路径都不是会话日志。
    fn is_session_log(&self, _path: &std::path::Path) -> bool {
        false
    }

    /// 无行可解析：日志行来源不存在，事实全部走数据库通路。
    fn parse_line(&self, _line: &str) -> super::LineParse {
        super::LineParse::Skip
    }

    /// `source_file` 即 opencode.db（数据库类来源的扫描簿记路径）。只读打开、
    /// 只 SELECT message / part 表（红线同 [`Self::read_db_source`]）。结构不
    /// 认识的消息整条塞进 Raw 透传——内容绝不静默丢弃。
    fn read_transcript(
        &self,
        source_file: &std::path::Path,
        external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let db = Connection::open_with_flags(source_file, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| {
                Error::Internal(format!("opencode.db 打开失败 {source_file:?}: {err}"))
            })?;
        let mut stmt = db
            .prepare(
                "SELECT id, time_created, data FROM message
                 WHERE session_id = ?1 ORDER BY time_created, id",
            )
            .map_err(Error::from)?;
        let rows = stmt
            .query_map(params![external_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(Error::from)?;

        // 新版 opencode 把消息内容拆进独立的 part 表（message.data 只剩元
        // 数据）；按 message_id 归组，读不到该表（老库）时回退 data 内嵌形状。
        let mut parts_by_message = parts_of(&db, external_id);

        let mut entries = Vec::new();
        for row in rows {
            let (id, col_ts, data) = row.map_err(Error::from)?;
            // data 损坏只跳过该行：坏行是真实历史，不阻断整个转录。
            let Ok(msg) = serde_json::from_str::<RawTranscriptMessage>(&data) else {
                continue;
            };
            let role = match msg.role.as_deref() {
                Some("user") => TranscriptRole::User,
                Some("assistant") => TranscriptRole::Assistant,
                Some("system") => TranscriptRole::System,
                _ => continue,
            };
            let embedded_parts = parts_by_message.remove(&id);
            let mut blocks = match &embedded_parts {
                Some(parts) => {
                    transcript::blocks_from_content(&serde_json::Value::Array(parts.clone()))
                }
                None => match (&msg.parts, &msg.content) {
                    (Some(parts), _) => transcript::blocks_from_content(parts),
                    (None, Some(content)) => transcript::blocks_from_content(content),
                    _ => vec![],
                },
            };
            if blocks.is_empty() {
                if embedded_parts.is_some() {
                    // 内容在 part 表但全是 step 标记：没有可展示的正文。
                    continue;
                }
                blocks.push(TranscriptBlock::Raw { json: data.clone() });
            }
            entries.push(TranscriptEntry {
                role,
                ts_ms: Some(msg.time.as_ref().and_then(|t| t.created).unwrap_or(col_ts)),
                model: msg
                    .model_id
                    .filter(|m| !m.trim().is_empty())
                    .filter(|_| role == TranscriptRole::Assistant),
                blocks,
            });
        }
        Ok(entries)
    }

    fn db_source_path(&self) -> Option<PathBuf> {
        self.db_path()
    }

    fn read_db_source(&self) -> Result<Option<DbSource>> {
        let Some(path) = self.db_path() else {
            return Ok(None);
        };
        // 库不存在 = 工具没装，常态而非错误；已登记路径的消失由扫描层对账标记。
        if !path.exists() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| Error::Internal(format!("opencode.db 打开失败 {path:?}: {err}")))?;

        let fingerprint = {
            let session = counts(
                &db,
                "SELECT COUNT(*), COALESCE(MAX(time_updated), 0) FROM session",
            )?;
            let message = counts(
                &db,
                "SELECT COUNT(*), COALESCE(MAX(time_updated), 0) FROM message",
            )?;
            Sha256::digest(format!("{session:?}|{message:?}").as_bytes()).to_vec()
        };

        let mut facts = Vec::new();
        {
            let mut stmt = db
                .prepare("SELECT id, directory, title, time_updated FROM session")
                .map_err(Error::from)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .map_err(Error::from)?;
            for row in rows {
                let (id, directory, title, ts) = row.map_err(Error::from)?;
                facts.push((
                    id.clone(),
                    LineFacts {
                        dedup_suffix: None,
                        request_count: 1,
                        session_external_id: id,
                        title: title.map(truncate_title),
                        project_dir: directory,
                        ts_ms: Some(ts),
                        usage: None,
                    },
                ));
            }
        }

        {
            let mut stmt = db
                .prepare("SELECT id, session_id, time_created, data FROM message")
                .map_err(Error::from)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(Error::from)?;
            for row in rows {
                let (id, session_id, col_ts, data) = row.map_err(Error::from)?;
                // data 损坏只跳过该行：坏行是真实历史，不阻断整库扫描。
                let Ok(msg) = serde_json::from_str::<RawMessage>(&data) else {
                    continue;
                };
                let usage = msg
                    .tokens
                    .as_ref()
                    .filter(|_| msg.role.as_deref() == Some("assistant"))
                    .filter(|t| !tokens_all_zero(t))
                    .zip(msg.model_id.as_deref())
                    .zip(msg.time.as_ref().and_then(|t| t.created).or(Some(col_ts)))
                    .map(|((tokens, model), ts)| usage_facts(tokens, model, ts, duration_ms(&msg)));
                let Some(usage) = usage else {
                    continue; // 用户消息与无用量的行不入账。
                };
                facts.push((
                    id,
                    LineFacts {
                        dedup_suffix: None,
                        request_count: 1,
                        session_external_id: session_id,
                        title: None,
                        project_dir: None,
                        ts_ms: Some(usage.ts_ms),
                        usage: Some(usage),
                    },
                ));
            }
        }

        Ok(Some(DbSource {
            path,
            fingerprint,
            facts,
        }))
    }

    fn delete_db_session(
        &self,
        db_path: &Path,
        external_id: &str,
        bundle_dir: &Path,
        trash: &dyn Fn(&Path) -> Result<()>,
    ) -> Result<Option<PathBuf>> {
        super::dbdelete::delete_session_rows(
            db_path,
            external_id,
            bundle_dir,
            trash,
            "opencode",
            &DELETE_EXPORT_TABLES,
        )
    }
}

impl OpenCodeAdapter {
    fn db_path(&self) -> Option<PathBuf> {
        self.db_path.clone().or_else(|| {
            crate::paths::home_dir().map(|home| {
                home.join(".local")
                    .join("share")
                    .join("opencode")
                    .join("opencode.db")
            })
        })
    }
}

/// 单行查询两个整数（计数、最大时间戳）。
fn counts(db: &Connection, sql: &str) -> Result<(i64, i64)> {
    db.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(Error::from)
}

/// 读某会话全部 part 行，按 message_id 归组（行序即内容顺序）。part 表
/// 不存在（老版库）或行损坏时静默为空——调用方回退 message.data 内嵌形状。
fn parts_of(db: &Connection, session_id: &str) -> HashMap<String, Vec<serde_json::Value>> {
    let Ok(mut stmt) = db.prepare(
        "SELECT message_id, data FROM part
         WHERE session_id = ?1 ORDER BY time_created, id",
    ) else {
        return HashMap::new();
    };
    let rows = stmt.query_map(params![session_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    });
    let mut out: HashMap<String, Vec<serde_json::Value>> = HashMap::new();
    for row in rows.into_iter().flatten().flatten() {
        let (message_id, data) = row;
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&data) {
            out.entry(message_id).or_default().push(value);
        }
    }
    out
}

/// 全零用量是真实存在的：失败/中止的请求也会在库里留下带 `tokens` 的
/// assistant 行（五桶全 0，界面上 0 token、$0.00）。无事实价值，跳过——
/// 与其余适配器同口径。
fn tokens_all_zero(t: &RawTokens) -> bool {
    t.input <= 0 && t.output <= 0 && t.reasoning <= 0 && t.cache.read <= 0 && t.cache.write <= 0
}

/// 分桶映射：input 不含缓存读取，reasoning 与 output 相加，全部原样入桶、不扣减。
fn usage_facts(t: &RawTokens, model: &str, ts_ms: i64, duration_ms: Option<i64>) -> UsageFacts {
    UsageFacts {
        ts_ms,
        model_raw: model.to_string(),
        model: model.to_string(),
        duration_ms,
        usage: TokenUsage {
            input_tokens: t.input.max(0),
            output_tokens: t.output.max(0),
            cache_read_tokens: t.cache.read.max(0),
            cache_write_tokens: t.cache.write.max(0),
            reasoning_tokens: (t.reasoning > 0).then_some(t.reasoning),
        },
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

/// `message.data` 里用得到的字段；未知字段忽略（前向兼容）。
#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(rename = "modelID", default)]
    model_id: Option<String>,
    #[serde(default)]
    tokens: Option<RawTokens>,
    #[serde(default)]
    time: Option<RawTime>,
}

/// 转录用的宽松结构：`parts`（新版）/ `content`（旧版）二态；都没有时整条
/// data 以 Raw 透传。
#[derive(Deserialize)]
struct RawTranscriptMessage {
    #[serde(default)]
    role: Option<String>,
    #[serde(rename = "modelID", default)]
    model_id: Option<String>,
    #[serde(default)]
    parts: Option<serde_json::Value>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    time: Option<RawTime>,
}

#[derive(Deserialize)]
struct RawTokens {
    #[serde(default)]
    input: i64,
    #[serde(default)]
    output: i64,
    #[serde(default)]
    reasoning: i64,
    #[serde(default)]
    cache: RawCache,
}

#[derive(Deserialize, Default)]
struct RawCache {
    #[serde(default)]
    read: i64,
    #[serde(default)]
    write: i64,
}

#[derive(Deserialize)]
struct RawTime {
    #[serde(default)]
    created: Option<i64>,
    #[serde(default)]
    completed: Option<i64>,
}

/// 请求耗时 = completed - created；两端缺一或时钟倒挂则无计时。
fn duration_ms(msg: &RawMessage) -> Option<i64> {
    let time = msg.time.as_ref()?;
    match (time.created, time.completed) {
        (Some(created), Some(completed)) if completed >= created => Some(completed - created),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::LineParse;

    /// 建与真实库同构的最小 schema（适配器按列名查询，多余列无关）。
    fn test_db(tag: &str) -> (PathBuf, Connection) {
        let path =
            std::env::temp_dir().join(format!("toktol-opencode-{}-{}.db", std::process::id(), tag));
        let _ = std::fs::remove_file(&path);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, title TEXT,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);
             CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);",
        )
        .unwrap();
        (path, db)
    }

    fn insert_session(db: &Connection, id: &str, title: &str, dir: &str, updated: i64) {
        db.execute(
            "INSERT INTO session (id, directory, title, time_created, time_updated) VALUES (?1, ?2, ?3, ?4, ?4)",
            rusqlite::params![id, dir, title, updated],
        )
        .unwrap();
    }

    fn insert_message(db: &Connection, id: &str, session_id: &str, data: &str, ts: i64) {
        db.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?3, ?4)",
            rusqlite::params![id, session_id, ts, data],
        )
        .unwrap();
    }

    fn insert_part(
        db: &Connection,
        id: &str,
        message_id: &str,
        session_id: &str,
        data: &str,
        ts: i64,
    ) {
        db.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            rusqlite::params![id, message_id, session_id, ts, data],
        )
        .unwrap();
    }

    fn assistant_data(model: &str, input: i64, output: i64, reasoning: i64, read: i64) -> String {
        format!(
            r#"{{"role":"assistant","modelID":"{model}","providerID":"na","cost":0,
                "tokens":{{"input":{input},"output":{output},"reasoning":{reasoning},
                          "cache":{{"read":{read},"write":0}}}},
                "time":{{"created":1000,"completed":2000}}}}"#
        )
    }

    fn read(adapter: &OpenCodeAdapter) -> DbSource {
        adapter.read_db_source().unwrap().expect("有数据库来源")
    }

    fn usage_of(source: &DbSource, key: &str) -> UsageFacts {
        let (_, facts) = source.facts.iter().find(|(k, _)| k == key).unwrap();
        facts.usage.as_ref().expect("该行必须有用量").clone()
    }

    #[test]
    fn sessions_and_assistant_usage_are_collected() {
        let (path, db) = test_db("basic");
        insert_session(&db, "ses_a", "修 bug", "E:/proj/demo", 5_000);
        insert_message(
            &db,
            "msg_1",
            "ses_a",
            &assistant_data("glm-5.3-flash", 100, 56, 9, 400),
            1_234,
        );
        // 用户消息没有 tokens，不入账。
        insert_message(
            &db,
            "msg_2",
            "ses_a",
            r#"{"role":"user","model":{"modelID":"glm-5.3-flash"},"time":{"created":900}}"#,
            900,
        );
        drop(db);

        let adapter = OpenCodeAdapter {
            db_path: Some(path),
        };
        let source = read(&adapter);
        assert_eq!(source.facts.len(), 2, "会话事实 + assistant 用量事实");

        let (key, session_facts) = &source.facts[0];
        assert_eq!(key, "ses_a");
        assert_eq!(session_facts.session_external_id, "ses_a");
        assert_eq!(session_facts.title.as_deref(), Some("修 bug"));
        assert_eq!(session_facts.project_dir.as_deref(), Some("E:/proj/demo"));
        assert_eq!(session_facts.usage, None);

        let usage = usage_of(&source, "msg_1");
        assert_eq!(usage.ts_ms, 1_000, "时间取 data.time.created");
        assert_eq!(usage.model_raw, "glm-5.3-flash");
        assert_eq!(usage.usage.input_tokens, 100);
        assert_eq!(
            usage.usage.output_tokens, 56,
            "reasoning 相加口径，输出不扣减"
        );
        assert_eq!(usage.usage.cache_read_tokens, 400);
        assert_eq!(usage.usage.reasoning_tokens, Some(9));
    }

    #[test]
    fn missing_total_and_missing_model_do_not_block() {
        let (path, db) = test_db("edge");
        insert_session(&db, "ses_a", "t", "E:/p", 5_000);
        // total 缺失是常态（子会话行），只要有分桶字段就照常入账。
        insert_message(
            &db,
            "msg_1",
            "ses_a",
            r#"{"role":"assistant","modelID":"m","tokens":{"input":1,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
            700,
        );
        // 无 modelID 的 assistant 行：不产用量，静默跳过。
        insert_message(
            &db,
            "msg_2",
            "ses_a",
            r#"{"role":"assistant","tokens":{"input":1,"output":2,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
            800,
        );
        // data 损坏：跳过不阻断。
        insert_message(&db, "msg_3", "ses_a", "{ broken", 850);
        drop(db);

        let adapter = OpenCodeAdapter {
            db_path: Some(path),
        };
        let source = read(&adapter);
        assert_eq!(source.facts.len(), 2, "会话 + 一条用量");
        let usage = usage_of(&source, "msg_1");
        assert_eq!(usage.ts_ms, 700, "data.time 缺失时回退列时间");
        assert_eq!(usage.usage.reasoning_tokens, None, "零推理不记桶");
    }

    /// 失败/中止请求在工具库里留下的全零 tokens 行（界面上 0 token、$0.00）
    /// 不入账；任一分桶非零仍是真实用量。
    #[test]
    fn zero_usage_rows_from_failed_requests_are_not_ingested() {
        let (path, db) = test_db("zero");
        insert_session(&db, "ses_a", "t", "E:/p", 5_000);
        insert_message(
            &db,
            "msg_1",
            "ses_a",
            r#"{"role":"assistant","modelID":"m","tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":100,"completed":150}}"#,
            100,
        );
        insert_message(&db, "msg_2", "ses_a", &assistant_data("m", 0, 5, 0, 0), 200);
        drop(db);

        let adapter = OpenCodeAdapter {
            db_path: Some(path),
        };
        let source = read(&adapter);
        assert_eq!(source.facts.len(), 2, "会话事实 + 一条用量，全零行跳过");
        assert_eq!(usage_of(&source, "msg_2").usage.output_tokens, 5);
    }

    #[test]
    fn fingerprint_tracks_message_and_session_changes() {
        let (path, db) = test_db("fp");
        insert_session(&db, "ses_a", "t", "E:/p", 5_000);
        insert_message(&db, "msg_1", "ses_a", &assistant_data("m", 1, 2, 0, 0), 100);
        drop(db);

        let adapter = OpenCodeAdapter {
            db_path: Some(path.clone()),
        };
        let fp1 = read(&adapter).fingerprint;

        // 追加消息：指纹必须变。
        let db = Connection::open(&path).unwrap();
        insert_message(&db, "msg_2", "ses_a", &assistant_data("m", 1, 2, 0, 0), 200);
        // 只改标题：time_updated 变，指纹也要变。
        db.execute(
            "UPDATE session SET title = 't2', time_updated = 6_000 WHERE id = 'ses_a'",
            [],
        )
        .unwrap();
        drop(db);
        let fp2 = read(&adapter).fingerprint;

        assert_ne!(fp1, fp2);
        let fp3 = read(&adapter).fingerprint;
        assert_eq!(fp2, fp3, "内容不变指纹不变");
    }

    #[test]
    fn missing_db_means_no_source_not_error() {
        let adapter = OpenCodeAdapter {
            db_path: Some(std::env::temp_dir().join("toktol-opencode-missing-does-not-exist.db")),
        };
        // 库不存在 = 工具没装：没有来源，不是错误。
        assert!(adapter.read_db_source().unwrap().is_none());
    }

    #[test]
    fn no_line_or_file_sources() {
        let adapter = OpenCodeAdapter { db_path: None };
        assert!(adapter.log_dirs().is_empty());
        assert_eq!(adapter.parse_line("anything"), LineParse::Skip);
        assert!(!adapter.is_session_log(std::path::Path::new("/x/opencode.db")));
    }

    #[test]
    fn transcript_reads_session_messages_in_time_order() {
        let (path, db) = test_db("transcript");
        insert_session(&db, "ses_a", "t", "E:/p", 5_000);
        // parts 数组（新版形状）+ 时间列乱序插入，输出必须按 time_created 排。
        insert_message(
            &db,
            "msg_1",
            "ses_a",
            r#"{"role":"user","parts":[{"type":"text","text":"帮我查"}]}"#,
            2_000,
        );
        insert_message(
            &db,
            "msg_2",
            "ses_a",
            r#"{"role":"assistant","modelID":"m-a","parts":[{"type":"reasoning","text":"想一下"},{"type":"text","text":"查到了"}],"time":{"created":100}}"#,
            1_000,
        );
        // 无 parts/content 的旧形状：整条 data 以 Raw 透传。
        insert_message(
            &db,
            "msg_3",
            "ses_a",
            r#"{"role":"user","note":"legacy"}"#,
            3_000,
        );
        // 其他会话与坏行不得混入。
        insert_message(
            &db,
            "msg_4",
            "ses_b",
            r#"{"role":"user","parts":[{"type":"text","text":"别的会话"}]}"#,
            4_000,
        );
        insert_message(&db, "msg_5", "ses_a", "{ broken", 3_500);
        drop(db);

        let adapter = OpenCodeAdapter {
            db_path: Some(path),
        };
        let entries = adapter
            .read_transcript(std::path::Path::new(&adapter.db_path().unwrap()), "ses_a")
            .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].ts_ms,
            Some(100),
            "按 time_created 升序，不按插入序"
        );
        assert_eq!(entries[0].model.as_deref(), Some("m-a"));
        assert_eq!(
            entries[0].blocks[1],
            TranscriptBlock::Text {
                text: "查到了".into()
            }
        );
        assert!(matches!(entries[2].blocks[0], TranscriptBlock::Raw { .. }));
    }

    /// 新版 opencode：内容在独立 part 表（message.data 只剩元数据）。文本、
    /// 推理、工具调用与结果、内嵌图片按 part 顺序展开；纯 step 标记的消息
    /// 整条跳过。
    #[test]
    fn transcript_reads_parts_from_part_table() {
        let (path, db) = test_db("part-table");
        insert_session(&db, "ses_a", "t", "E:/p", 5_000);
        insert_message(
            &db,
            "msg_1",
            "ses_a",
            r#"{"role":"user","time":{"created":100}}"#,
            100,
        );
        insert_part(
            &db,
            "prt_1",
            "msg_1",
            "ses_a",
            r#"{"type":"text","text":"帮我查"}"#,
            90,
        );
        insert_part(
            &db,
            "prt_0",
            "msg_1",
            "ses_a",
            r#"{"type":"file","mime":"image/png","filename":"shot","url":"data:image/png;base64,iVBORw0KGgoAAAAN"}"#,
            80,
        );
        insert_message(
            &db,
            "msg_2",
            "ses_a",
            r#"{"role":"assistant","modelID":"m-a","tokens":{"input":1,"output":2,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":200}}"#,
            200,
        );
        insert_part(
            &db,
            "prt_2",
            "msg_2",
            "ses_a",
            r#"{"type":"step-start"}"#,
            210,
        );
        insert_part(
            &db,
            "prt_3",
            "msg_2",
            "ses_a",
            r#"{"type":"tool","tool":"bash","callID":"call_1","state":{"status":"error","input":{"command":"ls"},"output":"boom"}}"#,
            220,
        );
        insert_part(
            &db,
            "prt_4",
            "msg_2",
            "ses_a",
            r#"{"type":"reasoning","text":"想一下"}"#,
            230,
        );
        // 全是结构标记的 assistant 行：无内容，不产条目。
        insert_message(
            &db,
            "msg_3",
            "ses_a",
            r#"{"role":"assistant","modelID":"m-a","tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":300}}"#,
            300,
        );
        insert_part(
            &db,
            "prt_5",
            "msg_3",
            "ses_a",
            r#"{"type":"step-start"}"#,
            310,
        );
        insert_part(
            &db,
            "prt_6",
            "msg_3",
            "ses_a",
            r#"{"type":"step-finish","tokens":{"input":1}}"#,
            320,
        );
        drop(db);

        let adapter = OpenCodeAdapter {
            db_path: Some(path),
        };
        let entries = adapter
            .read_transcript(std::path::Path::new(&adapter.db_path().unwrap()), "ses_a")
            .unwrap();
        assert_eq!(entries.len(), 2, "纯 step 标记的消息跳过");

        let user = &entries[0];
        assert_eq!(user.role, TranscriptRole::User);
        assert_eq!(
            user.blocks,
            vec![
                TranscriptBlock::Image {
                    path: String::new(),
                    filename: "shot".into(),
                    size: Some(12),
                    exists: true,
                    data_url: Some("data:image/png;base64,iVBORw0KGgoAAAAN".into()),
                },
                TranscriptBlock::Text {
                    text: "帮我查".into()
                },
            ],
            "part 按行序展开，不按插入序"
        );

        let assistant = &entries[1];
        assert_eq!(assistant.model.as_deref(), Some("m-a"));
        assert_eq!(
            assistant.blocks,
            vec![
                TranscriptBlock::ToolCall {
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: Some(r#"{"command":"ls"}"#.into()),
                },
                TranscriptBlock::ToolResult {
                    call_id: Some("call_1".into()),
                    content: "boom".into(),
                    is_error: true,
                },
                TranscriptBlock::Thinking {
                    text: "想一下".into()
                },
            ]
        );
    }
}
