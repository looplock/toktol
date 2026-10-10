//! codebuddy 适配器：腾讯 CodeBuddy IDE（VS Code fork，数据在
//! `%APPDATA%/CodeBuddy CN/`，国际版为 `CodeBuddy/`，两处都探测）。两个来源各管一摊。
//!
//! **会话**：根目录 `codebuddy-sessions.vscdb` 的 ItemTable，只 SELECT `session:%`
//! 键（conversationId / cwd / title / createdAt / updatedAt）。同表混有
//! `secret://…` 凭据键（accessToken 等），按键前缀精确匹配是红线，绝不全表读。
//!
//! **用量**：`logs/<启动>/<窗口>/exthost/Tencent-Cloud.coding-copilot/*.log` 扩展
//! 日志里的 `notifyStepEnd` 行——每次模型请求一条 usage JSON，与明细页同粒度。
//! 消息正文在云端不落盘，日志只有请求步进、模型与工具调用；`apiKey` 等字段
//! 出现在模型元数据行里，解析只提取 `modelId=`，整行绝不入账。
//!
//! 归属（实测）：usage 行不带 conversationId，只带 requestId；同一轮的各步共用
//! 一个 requestId（step 递增）。conversationId 靠日志里密集出现的标记行（启动
//! 恢复、请求开始、工具上报都带 `conversationId=<hex32>`）——解析维持"当前会话"
//! 状态，工具上报行还直接给出 (requestId → conversationId) 对，优先用它。
//!
//! 口径（实测）：`inputTokens = cacheTokens(读) + cachedWriteTokens(写) +
//! cachedMissTokens(未命中)`，`totalTokens = inputTokens + outputTokens`。
//! `outputTokens` 含 `thinkingTokens`（Anthropic 语义，与 grok 的 OpenAI 语义
//! 相反），入库前拆出。modelId 取步进前最近的 `max_tokens resolved … modelId=`
//! 行；标题生成等旁路请求走不到 notifyStepEnd，自然不入账。耗时来自紧随步进
//! 行的 `Step execution metrics` 行（usage JSON 与步进行逐字节相同，以此为键
//! 配对）；子代理（code-explorer）步进没有 metrics 行，时长留空。
//!
//! **转录**：读本地历史缓存 `%LOCALAPPDATA%/CodeBuddyExtension/Data/<userId>/
//! CodeBuddyIDE/<userId>/history/<workspaceHash>/<conversationId>/`——
//! `index.json` 给消息顺序，`messages/<id>.json` 各存一条（`message`/`extra`
//! 是转义 JSON：content 块数组 + inputPhrase/modelId 等）。真实提问在
//! `extra.inputPhrase`（结构化；缺失时从 content 的 `<user_query>` 标签兜底，
//! 两者是同一句话的两份记录只取一份）；content 文本块是 harness 注入面：`<user_info>`/`<project_context>` 等信封 → HarnessContext 前置
//! 独立条目（codex 口径），`<system-reminder>`/`<additional_data>` → Injected
//! 占比条，与 grok 同一套分段；assistant 的 reasoning/text/tool-call 与 tool
//! 的 tool-result 直接映射。缓存缺失 / 无消息 → [`Error::Unsupported`] 占位
//! 文案，不合成空壳时间线；helper 消息（`isHelperMessage`，UI 生成的报错/
//! 提示）跳过；单个消息文件损坏只跳该条。缓存是共享文件，删除会话不动文件。
//!
//! **删除**（数据库类来源的写例外）：`session:<id>` 键读值导出 bundle、与
//! 历史缓存目录一并移入回收站，随后精确等值删键（同表 `secret://` 凭据键
//! 绝不触碰）。共享扩展日志多会话共用、绝不删——用量行留在日志里，复活
//! 由 toktol 侧的 `deleted_sessions` 墓碑拦住（扫描层建行前查）。
//!
//! 跨行上下文（当前会话、模型、requestId 映射）决定事实，故只实现
//! [`Adapter::parse_file`]，不支持流式；日志按应用启动分目录，单文件量级 MB。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::transcript::{
    self, TextPiece, TranscriptBlock, TranscriptEntry, TranscriptRole, top_level_pieces,
};
use super::{Adapter, DbSource, LineFacts, LineParse, UsageFacts};
use crate::error::{Error, Result};
use crate::model::{TokenUsage, Tool};

/// IDE 安装目录名候选：国内版在前（优先），国际版兜底。
const INSTALL_DIRS: [&str; 2] = ["CodeBuddy CN", "CodeBuddy"];
const SESSIONS_DB_FILE: &str = "codebuddy-sessions.vscdb";
/// 扩展日志的宿主目录名：is_session_log 靠它锁定，避免误吃其他扩展的日志。
const LOG_HOST_DIR: &str = "Tencent-Cloud.coding-copilot";

/// codebuddy 的适配器实现。`app_data_dir` 为 `None` 时取 `%APPDATA%`；
/// `local_data_dir` 为 `None` 时取 `%LOCALAPPDATA%/CodeBuddyExtension/Data`。
/// 两者都为测试注入（目录下直接放 `CodeBuddy CN/` 或 `CodeBuddy/`、`Data/`）。
pub struct CodeBuddyAdapter {
    /// 测试注入的数据根（含 `CodeBuddy CN/` 安装目录）；`None` 取 `%APPDATA%`。
    pub app_data_dir: Option<PathBuf>,
    /// 测试注入的本地历史缓存根（即 `CodeBuddyExtension/Data` 目录本体）；
    /// `None` 取 `%LOCALAPPDATA%`。
    pub local_data_dir: Option<PathBuf>,
}

impl CodeBuddyAdapter {
    /// 安装目录候选（存在与否不管，调用方按需 exists 过滤）。
    fn install_dirs(&self) -> Vec<PathBuf> {
        match &self.app_data_dir {
            Some(root) => INSTALL_DIRS.iter().map(|d| root.join(d)).collect(),
            None => match std::env::var_os("APPDATA") {
                Some(root) => INSTALL_DIRS
                    .iter()
                    .map(|d| PathBuf::from(&root).join(d))
                    .collect(),
                None => vec![],
            },
        }
    }

    /// 会话库：国内版优先，存在者胜出。
    fn sessions_db_path(&self) -> Option<PathBuf> {
        self.install_dirs()
            .into_iter()
            .map(|dir| dir.join(SESSIONS_DB_FILE))
            .find(|path| path.exists())
    }

    /// 日志根目录候选：`<install>/logs`。
    fn logs_roots(&self) -> Vec<PathBuf> {
        self.install_dirs()
            .into_iter()
            .map(|dir| dir.join("logs"))
            .collect()
    }

    /// 本地历史缓存根候选：`CodeBuddyExtension/Data`。
    fn history_roots(&self) -> Vec<PathBuf> {
        match &self.local_data_dir {
            Some(root) => vec![root.clone()],
            None => match std::env::var_os("LOCALAPPDATA") {
                Some(root) => vec![PathBuf::from(root).join("CodeBuddyExtension").join("Data")],
                None => vec![],
            },
        }
    }

    /// 在缓存根下定位会话目录：`Data/<userId>/CodeBuddyIDE/<userId>/history/
    /// <workspaceHash>/<conversationId>`。两段 userId 实测相同，但 userId 与
    /// workspaceHash 都无法从会话库的会话行推导，按目录逐层通配扫到命中为止。
    fn history_dir(&self, external_id: &str) -> Option<PathBuf> {
        for root in self.history_roots() {
            let Ok(users) = std::fs::read_dir(&root) else {
                continue;
            };
            for user in users.flatten() {
                let Ok(ide_users) = std::fs::read_dir(user.path().join("CodeBuddyIDE")) else {
                    continue;
                };
                for ide_user in ide_users.flatten() {
                    let Ok(workspaces) = std::fs::read_dir(ide_user.path().join("history")) else {
                        continue;
                    };
                    for workspace in workspaces.flatten() {
                        let candidate = workspace.path().join(external_id);
                        if candidate.is_dir() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
        None
    }
}

impl Adapter for CodeBuddyAdapter {
    fn tool(&self) -> Tool {
        Tool::CodeBuddy
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        self.logs_roots()
    }

    /// 只认扩展日志宿主目录下的 `.log`（含轮转的 `.1.log`——扩展名仍是 log）。
    fn is_session_log(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("log")
            && path
                .parent()
                .is_some_and(|dir| dir.file_name().and_then(|n| n.to_str()) == Some(LOG_HOST_DIR))
    }

    fn db_source_path(&self) -> Option<PathBuf> {
        self.sessions_db_path()
    }

    /// 会话库全量读取。指纹 = `session:%` 键的行数与总字节数（行只增、标题
    /// 更新会变长度，足以判定"有无新增"）。
    fn read_db_source(&self) -> Result<Option<DbSource>> {
        let Some(path) = self.sessions_db_path() else {
            return Ok(None);
        };
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| Error::Internal(format!("codebuddy 会话库打开失败 {path:?}: {err}")))?;
        let (count, total_len): (i64, i64) = db
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(LENGTH(value)), 0) FROM ItemTable
                 WHERE key LIKE 'session:%'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(Error::from)?;
        // 指纹混入事实语义版本：解析口径变更（字段补齐、修 bug）时递增，强制
        // 下轮全量重放——重放不重复入账（dedup 按行拦），但会话行的标题/项目
        // 目录补齐依赖事实重跑；只看数据文件的话，改解析永远追不上旧账。
        let fingerprint = Sha256::new()
            .chain_update(b"codebuddy-session-facts-v2")
            .chain_update(count.to_le_bytes())
            .chain_update(total_len.to_le_bytes())
            .finalize()
            .to_vec();

        let mut facts = Vec::new();
        let mut stmt = db
            .prepare("SELECT value FROM ItemTable WHERE key LIKE 'session:%'")
            .map_err(Error::from)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(Error::from)?;
        for row in rows {
            let raw = row.map_err(Error::from)?;
            let Ok(session) = serde_json::from_str::<SessionJson>(&raw) else {
                continue;
            };
            let title = session.title.trim();
            let project_dir = session.cwd.trim();
            // 会话 id 为空 = 库里的值不是预期的 JSON 形状（serde default 兜底），
            // 入账会产空 id 的幽灵会话行，宁可不采。
            if session.conversation_id.is_empty() || (title.is_empty() && project_dir.is_empty()) {
                continue;
            }
            facts.push((
                format!("session:{}", session.conversation_id),
                LineFacts {
                    dedup_suffix: None,
                    request_count: 1,
                    session_external_id: session.conversation_id,
                    title: non_empty(title),
                    project_dir: non_empty(project_dir),
                    ts_ms: Some(session.updated_at.unwrap_or(session.created_at)),
                    usage: None,
                },
            ));
        }
        Ok(Some(DbSource {
            path,
            fingerprint,
            facts,
        }))
    }

    /// 日志文件多会话共享，绝不作为删除对象；登记源是会话库的删除本就被
    /// sessions 层拒绝（工具自有库不进回收站）。
    fn session_artifacts(&self, _source_file: &Path) -> Vec<PathBuf> {
        vec![]
    }

    /// 逐行接口在有跨行状态的工具上没有事实可产（归属依赖前文），恒 Skip；
    /// 本适配器声明不支持流式扫描，扫描层不会走到这里。
    fn parse_line(&self, _line: &str) -> LineParse {
        LineParse::Skip
    }

    fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
        parse_steps(lines.iter().copied().enumerate())
            .into_iter()
            .filter(|(index, _)| *index >= start)
            .map(|(_, step)| step.into_line_parse())
            .collect()
    }

    /// 转录读本地历史缓存（见模块文档）。登记源是会话库，`source_file` 用
    /// 不上，靠 `external_id`（conversationId）定位缓存目录。
    fn read_transcript(
        &self,
        _source_file: &Path,
        external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let Some(dir) = self.history_dir(external_id) else {
            return Err(Error::Unsupported);
        };
        let index_path = dir.join("index.json");
        if !index_path.exists() {
            return Err(Error::Unsupported);
        }
        // 旁路转录与登记源同受扫描上限约束：登记源闸门 stat 的是 .vscdb，这里
        // 读的 index 与消息文件它管不着——index 超限直接拒绝，消息文件走累计
        // 读取量预算，超限即拒绝，不拖进无界全量读。
        transcript::gate_size(&index_path, crate::scan::MAX_SCAN_FILE_BYTES)?;
        let index: HistoryIndex = serde_json::from_str(&transcript::read_text(&index_path)?)
            .map_err(|_| Error::Unsupported)?;
        if index.messages.is_empty() {
            return Err(Error::Unsupported);
        }
        let mut read_budget = crate::scan::MAX_SCAN_FILE_BYTES;
        let mut entries = Vec::new();
        for reference in &index.messages {
            // 单个消息文件缺失/损坏只跳该条：历史缓存是 IDE 维护的，个别
            // 文件不齐不拖垮整条转录（与 grok 按行跳坏行同口径）。
            let path = dir.join("messages").join(format!("{}.json", reference.id));
            if let Ok(meta) = std::fs::metadata(&path) {
                read_budget -= i64::try_from(meta.len()).unwrap_or(i64::MAX);
                if read_budget < 0 {
                    return Err(Error::TranscriptOversize);
                }
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(file) = serde_json::from_str::<MessageFile>(&text) else {
                continue;
            };
            let Some(inner) = unescaped(&file.message) else {
                continue;
            };
            let extra = unescaped(&file.extra).unwrap_or(Value::Null);
            let ts_ms = file.created_at.as_deref().and_then(ts_ms_of);
            let role = if file.role.is_empty() {
                reference.role.as_str()
            } else {
                file.role.as_str()
            };
            match role {
                "user" => entries.extend(user_entries(&inner, &extra, ts_ms)),
                "assistant" => {
                    let blocks = assistant_blocks(inner.get("content").unwrap_or(&Value::Null));
                    if blocks.is_empty() {
                        continue;
                    }
                    let model = extra
                        .get("modelId")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|model| !model.is_empty())
                        .map(str::to_string);
                    entries.push(TranscriptEntry {
                        role: TranscriptRole::Assistant,
                        ts_ms,
                        model,
                        blocks,
                    });
                }
                "tool" => {
                    let blocks = tool_blocks(inner.get("content").unwrap_or(&Value::Null));
                    if blocks.is_empty() {
                        continue;
                    }
                    entries.push(TranscriptEntry {
                        role: TranscriptRole::Tool,
                        ts_ms,
                        model: None,
                        blocks,
                    });
                }
                // 索引里没出现 system 角色；真出现时按文本透传，不静默丢。
                "system" => {
                    let blocks = transcript::blocks_from_content(
                        inner.get("content").unwrap_or(&Value::Null),
                    );
                    if blocks.is_empty() {
                        continue;
                    }
                    entries.push(TranscriptEntry {
                        role: TranscriptRole::System,
                        ts_ms,
                        model: None,
                        blocks,
                    });
                }
                _ => {}
            }
        }
        Ok(entries)
    }

    /// 数据库类来源的删除（用户主动删除的写例外）。vscdb 是 KV 表（无 session
    /// 表可级联），走定制流程：精确键 `session:<id>` 读值导出 bundle，bundle
    /// 与历史缓存目录（转录正文）一并移入回收站，最后才删键——"先入桶后动库"
    /// 的既有不变式。键必须精确等值：同表混有 `secret://…` 凭据键，任何宽
    /// 匹配（LIKE/前缀扫）都是红线。行不存在返回 `Ok(None)`：登记已无（如
    /// codeg 托管会话根本不在库里），调用方照常清 toktol 侧归属。共享扩展
    /// 日志多会话共用，绝不删——用量行留在日志里，复活由 toktol 侧的
    /// deleted_sessions 墓碑拦住（见扫描层）。
    fn delete_db_session(
        &self,
        db_path: &Path,
        external_id: &str,
        bundle_dir: &Path,
        trash: &dyn Fn(&Path) -> Result<()>,
    ) -> Result<Option<PathBuf>> {
        let key = format!("session:{external_id}");
        // 工具可能在运行：写锁竞争靠 busy_timeout 等待（与 dbdelete 同口径）。
        let mut db = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|err| {
                Error::Internal(format!("codebuddy 会话库打开失败 {db_path:?}: {err}"))
            })?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(Error::from)?;
        // IMMEDIATE：导出与删键之间不容他人插写，入桶快照 == 删键状态。
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let value: Option<String> = tx
            .query_row(
                "SELECT value FROM ItemTable WHERE key = ?1",
                rusqlite::params![key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(value) = value else {
            return Ok(None);
        };

        let history_dir = self.history_dir(external_id);
        let exported_at_ms = chrono::Utc::now().timestamp_millis();
        let safe_id: String = external_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let bundle_path =
            bundle_dir.join(format!("codebuddy-session-{safe_id}-{exported_at_ms}.json"));
        let bundle = serde_json::json!({
            "tool": "codebuddy",
            "db": db_path.to_string_lossy(),
            "key": key,
            "value": value,
            "historyDir": history_dir.as_ref().map(|dir| dir.to_string_lossy()),
            "exportedAtMs": exported_at_ms,
        });
        std::fs::create_dir_all(bundle_dir).map_err(|source| Error::DataFile {
            path: bundle_dir.to_path_buf(),
            source,
        })?;
        std::fs::write(&bundle_path, serde_json::to_string_pretty(&bundle)?).map_err(|source| {
            Error::DataFile {
                path: bundle_path.clone(),
                source,
            }
        })?;
        trash(&bundle_path)?;
        if let Some(dir) = &history_dir
            && dir.exists()
        {
            trash(dir)?;
        }

        let deleted = tx.execute(
            "DELETE FROM ItemTable WHERE key = ?1",
            rusqlite::params![key],
        )?;
        if deleted == 0 {
            return Err(Error::Internal(format!(
                "codebuddy session row vanished during delete: {external_id}"
            )));
        }
        tx.commit()?;
        Ok(Some(bundle_path))
    }
}

fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 会话库 `session:%` 值的 JSON 形状（本机实测）。`status` / `userId` 不入账。
#[derive(Deserialize)]
struct SessionJson {
    #[serde(rename = "conversationId", default)]
    conversation_id: String,
    #[serde(rename = "cwd", default)]
    cwd: String,
    #[serde(default)]
    title: String,
    #[serde(rename = "createdAt", default)]
    created_at: i64,
    #[serde(rename = "updatedAt", default)]
    updated_at: Option<i64>,
}

/// 日志解析的单遍状态：当前会话（最近标记）、当前模型（最近 max_tokens 行）、
/// requestId → 会话（工具上报行等直接给出）。
#[derive(Default)]
struct LogParser {
    current_conversation: Option<String>,
    current_model: Option<String>,
    request_conversation: HashMap<String, String>,
}

/// 一次模型请求步进的解析产物（notifyStepEnd 行）。
struct StepRecord {
    conversation_id: Option<String>,
    ts_ms: Option<i64>,
    model: Option<String>,
    usage: Option<RawUsage>,
    /// 与 usage 原文一致的 JSON 片段（metrics 行的时长配对键）。
    usage_key: Option<String>,
    /// 请求耗时；由后续 metrics 行配对补齐，见 [`parse_steps`]。
    duration_ms: Option<i64>,
}

impl StepRecord {
    fn into_line_parse(self) -> LineParse {
        let Some(facts) = self.usage_facts() else {
            return LineParse::Skip;
        };
        LineParse::Facts(Box::new(LineFacts {
            dedup_suffix: None,
            request_count: 1,
            session_external_id: facts.0,
            title: None,
            project_dir: None,
            ts_ms: Some(facts.1),
            usage: Some(facts.2),
        }))
    }

    /// 入账三要素：会话、模型、时间。缺一没有可入账事实（与 grok 同口径）。
    fn usage_facts(&self) -> Option<(String, i64, UsageFacts)> {
        let conversation_id = self.conversation_id.as_deref()?.to_string();
        let ts_ms = self.ts_ms?;
        let model_raw = self.model.as_deref()?.trim();
        if model_raw.is_empty() {
            return None;
        }
        let raw = self.usage.as_ref()?;
        let usage = raw.buckets();
        if usage.input_tokens == 0
            && usage.output_tokens == 0
            && usage.cache_read_tokens == 0
            && usage.cache_write_tokens == 0
        {
            return None;
        }
        Some((
            conversation_id,
            ts_ms,
            UsageFacts {
                ts_ms,
                model_raw: model_raw.to_string(),
                model: model_raw.to_string(),
                duration_ms: self.duration_ms,
                usage,
            },
        ))
    }
}

/// usage JSON 的原始形状（实测字段）。缺字段按 0 处理——旧版本日志可能少桶。
#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(rename = "inputTokens", default)]
    input_tokens: i64,
    #[serde(rename = "outputTokens", default)]
    output_tokens: i64,
    #[serde(rename = "cacheTokens", default)]
    cache_tokens: i64,
    #[serde(rename = "cachedWriteTokens", default)]
    cached_write_tokens: i64,
    #[serde(rename = "thinkingTokens", default)]
    thinking_tokens: Option<i64>,
}

impl RawUsage {
    /// 实测口径（见模块文档）：inputTokens 含缓存读与缓存写，拆出净输入；
    /// outputTokens 含 thinking，拆出推理。
    fn buckets(&self) -> TokenUsage {
        let cache_read = self.cache_tokens.max(0);
        let cache_write = self.cached_write_tokens.max(0);
        let reasoning = self.thinking_tokens.filter(|&n| n > 0);
        TokenUsage {
            input_tokens: (self.input_tokens.max(0) - cache_read - cache_write).max(0),
            output_tokens: (self.output_tokens.max(0) - reasoning.unwrap_or(0)).max(0),
            cache_read_tokens: cache_read,
            cache_write_tokens: cache_write,
            reasoning_tokens: reasoning,
        }
    }
}

impl LogParser {
    /// 喂入一行，返回该行产生的事件。状态跨行维持，喂入顺序必须是文件顺序；
    /// 同一行先更新状态再产出事件（工具上报行同时携带 conversationId，先归会话）。
    ///
    /// 事件有两种：notifyStepEnd 行完结一次请求步进；紧随其后的 metrics 行带
    /// `duration=<毫秒>ms` 且 usage JSON 与步进逐字节相同——时长按 usage JSON
    /// 为键回填给最近的同 JSON 步进（子代理步进没有 metrics 行，时长留空）。
    fn feed(&mut self, line: &str) -> Option<LogEvent> {
        let ts_ms = parse_log_ts(line);
        if let Some(id) = marker_value(line, "conversationId=")
            .or_else(|| marker_value(line, "conversationId: "))
            .or_else(|| marker_value(line, "conversation: "))
        {
            self.current_conversation = Some(id);
        }
        if let Some(id) = pair_value(line, "requestId=", &[' ', ','])
            && let Some(conversation) = pair_value(line, "conversationId=", &[' ', ','])
        {
            self.request_conversation.insert(id, conversation);
        }
        if let Some(model) = pair_value(line, "modelId=", &[',', ' ', ')']) {
            // 只认请求准备行的 modelId；CompactStrategy 等统计行也带 modelId，
            // 但取值时机相同，无碍——不额外区分。
            self.current_model = Some(model);
        }
        if line.contains("Step execution metrics") {
            let usage_key = extract_usage_json(line);
            let duration_ms = duration_ms_of(line)?;
            return Some(LogEvent::Metrics {
                duration_ms,
                usage_key: usage_key?,
            });
        }
        if !line.contains("notifyStepEnd") {
            return None;
        }
        let request_id = pair_value(line, "requestId: ", &[' ', ','])?;
        let usage_key = extract_usage_json(line);
        let usage = usage_key
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok());
        let conversation_id = self
            .request_conversation
            .get(&request_id)
            .cloned()
            .or_else(|| self.current_conversation.clone());
        Some(LogEvent::Step(StepRecord {
            conversation_id,
            ts_ms,
            model: self.current_model.clone(),
            usage,
            usage_key,
            duration_ms: None,
        }))
    }
}

/// 单行解析的事件：请求步进完结，或一条可配对的步进耗时。
enum LogEvent {
    Step(StepRecord),
    Metrics { duration_ms: i64, usage_key: String },
}

/// 喂入（行下标, 行）序列并配对时长，返回带时长的步进列表（metrics 行只在
/// 配对时消费，不外露）。配对两遍走：metrics 行在步进行**之后**，单遍喂入时
/// 步进事实还没有时长可带——以逐字节相同的 usage JSON 为键回填。扫描与详情
/// 两条路径都必须走这里，否则详情时间线拿不到耗时。
fn parse_steps<'a, I>(lines: I) -> Vec<(usize, StepRecord)>
where
    I: IntoIterator<Item = (usize, &'a str)>,
{
    let mut parser = LogParser::default();
    let mut events: Vec<(usize, LogEvent)> = lines
        .into_iter()
        .filter_map(|(index, line)| parser.feed(line).map(|event| (index, event)))
        .collect();
    // (usage JSON 键, 步进事件下标)。
    let mut pending: Vec<(String, usize)> = Vec::new();
    for ordinal in 0..events.len() {
        let (usage_key, duration) = match &events[ordinal].1 {
            LogEvent::Step(step) => match &step.usage_key {
                Some(key) => (key.clone(), None),
                None => continue,
            },
            LogEvent::Metrics {
                duration_ms,
                usage_key,
            } => (usage_key.clone(), Some(*duration_ms)),
        };
        match duration {
            None => pending.push((usage_key, ordinal)),
            Some(duration_ms) => {
                if let Some(pos) = pending.iter().position(|(k, _)| k == &usage_key) {
                    let (_, target) = pending.remove(pos);
                    if let LogEvent::Step(step) = &mut events[target].1 {
                        step.duration_ms = Some(duration_ms);
                    }
                }
            }
        }
    }
    events
        .into_iter()
        .filter_map(|(index, event)| match event {
            LogEvent::Step(step) => Some((index, step)),
            LogEvent::Metrics { .. } => None,
        })
        .collect()
}

/// `duration=<毫秒>ms`。
fn duration_ms_of(line: &str) -> Option<i64> {
    let start = line.find("duration=")? + "duration=".len();
    let digits: String = line[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// 提取 `needle` 之后的会话 id（hex 串，长度 ≥16 才可信）。
fn marker_value(line: &str, needle: &str) -> Option<String> {
    let start = line.find(needle)? + needle.len();
    let hex: String = line[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    (hex.len() >= 16).then_some(hex)
}

/// 提取 `needle=<值>` 形态的值，`stops` 里任一字符截断。
fn pair_value(line: &str, needle: &str, stops: &[char]) -> Option<String> {
    let start = line.find(needle)? + needle.len();
    let value: String = line[start..]
        .chars()
        .take_while(|c| !stops.contains(c) && !c.is_control())
        .collect();
    let value = value.trim_end();
    non_empty(value)
}

/// 提取 `usage` 字样后第一个 `{` 起的花括号平衡 JSON 片段。notifyStepEnd 行是
/// `usage: {…}`、metrics 行是 `usage={…}`，两种都归一成同一段文本（duration
/// 配对靠它做键）。截断行（轮转尾）没有闭合括号，返回 `None`。
fn extract_usage_json(line: &str) -> Option<String> {
    let usage_at = line.find("usage")?;
    let start = usage_at + line[usage_at..].find('{')?;
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    for (offset, byte) in bytes[start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(line[start..=start + offset].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// 日志行首 23 字节是本地时间 `YYYY-MM-DD HH:MM:SS.mmm`。DST 歧义取早者；
/// 解析不了返回 `None`（不编造时间戳）。
fn parse_log_ts(line: &str) -> Option<i64> {
    let naive = NaiveDateTime::parse_from_str(line.get(..23)?, "%Y-%m-%d %H:%M:%S%.3f").ok()?;
    match Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(time) => Some(time.timestamp_millis()),
        chrono::LocalResult::Ambiguous(time, _) => Some(time.timestamp_millis()),
        chrono::LocalResult::None => None,
    }
}

/// index.json 的形状（实测）：`messages` 按对话顺序给引用，`requests` 是
/// 请求分组（转录用不上）。
#[derive(Deserialize)]
struct HistoryIndex {
    #[serde(default)]
    messages: Vec<HistoryMessageRef>,
}

#[derive(Deserialize)]
struct HistoryMessageRef {
    #[serde(default)]
    id: String,
    /// 消息文件缺 role 字段时的兜底（实测两边都有）。
    #[serde(default)]
    role: String,
}

/// messages/<id>.json 的形状（实测）。`message` 与 `extra` 是转义后的 JSON
/// 字符串（内层再各自解析），`createdAt` 是 RFC3339 的 UTC 时间。
#[derive(Deserialize)]
struct MessageFile {
    #[serde(default)]
    role: String,
    #[serde(default)]
    message: Value,
    #[serde(default)]
    extra: Value,
    #[serde(rename = "createdAt", default)]
    created_at: Option<String>,
}

/// 字段值可能是转义 JSON 字符串（实测形态）也可能是已反序列化的对象（版本
/// 差异兜底），统一解到 `Value`；都解不开返回 `None`。
fn unescaped(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str(text).ok(),
        value @ Value::Object(_) => Some(value.clone()),
        _ => None,
    }
}

/// RFC3339（`2026-09-15T03:23:09.765Z`，实测）→ epoch ms。
fn ts_ms_of(created_at: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(created_at.trim())
        .ok()
        .map(|time| time.timestamp_millis())
}

/// user 消息 → 转录条目（一至两条）。真实提问在 `extra.inputPhrase`
/// （结构化，无需拆标签）；content 文本块是 harness 注入面（实测：纯信封），
/// 信封归 HarnessContext 拆成前置独立条目（codex 口径），reminder/data 注入
/// 计入 Injected 占比条——与 grok 的 `segment_user_text` 同一套分段。
/// helper 消息（`isHelperMessage`，UI 生成的报错/提示）不是用户输入，跳过。
fn user_entries(inner: &Value, extra: &Value, ts_ms: Option<i64>) -> Vec<TranscriptEntry> {
    if extra.get("isHelperMessage").and_then(Value::as_bool) == Some(true) {
        return vec![];
    }
    let content = inner.get("content").unwrap_or(&Value::Null);
    let mut tagged_queries: Vec<Option<String>> = Vec::new();
    let segmented: Vec<TranscriptBlock> = transcript::blocks_from_content(content)
        .into_iter()
        .flat_map(|block| match block {
            TranscriptBlock::Text { text } => {
                let (blocks, tagged) = segment_user_text(&text);
                tagged_queries.push(tagged);
                blocks
            }
            kept => vec![kept],
        })
        .collect();
    let (harness, rest): (Vec<_>, Vec<_>) = segmented
        .into_iter()
        .partition(|block| matches!(block, TranscriptBlock::HarnessContext { .. }));
    // inputPhrase 是输入框里的原始提问（数组形态），取第一段非空内容；
    // 缺失时从 content 的 <user_query> 标签兜底（两条路径是同一句话的
    // 两份记录，只取一份，否则气泡里提问显示两遍）。
    let query = extra
        .get("inputPhrase")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .filter_map(|item| item.get("content").and_then(Value::as_str))
                .map(str::trim)
                .find(|text| !text.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            tagged_queries
                .into_iter()
                .flatten()
                .find(|text| !text.is_empty())
        });
    let mut rest = rest;
    if let Some(query) = query {
        rest.push(TranscriptBlock::Text { text: query });
    }
    let entry = |blocks: Vec<TranscriptBlock>| TranscriptEntry {
        role: TranscriptRole::User,
        ts_ms,
        model: None,
        blocks,
    };
    let mut entries = Vec::with_capacity(2);
    if !harness.is_empty() {
        entries.push(entry(harness));
    }
    if !rest.is_empty() {
        entries.push(entry(rest));
    }
    entries
}

/// user content 文本块的分段（codebuddy 口径）。与 grok 同一套信封切分：
/// `<user_info>`/`<project_context>` 等上下文信封归 HarnessContext，
/// `<system-reminder>`/`<additional_data>` 计入注入字符，`<user_query>` 不成
/// 卡（正文是提问，结构化副本在 `extra.inputPhrase`，原文本就留在 Injected
/// 里供完整口径渲染——与 workbuddy 的原文树含标签一致）。返回（块, 标签里
/// 提到的问——inputPhrase 缺失时的兜底）。散落信封外的自由文字按用户话
/// 保留。
fn segment_user_text(text: &str) -> (Vec<TranscriptBlock>, Option<String>) {
    let mut blocks = Vec::new();
    let mut injected_chars = 0i64;
    let mut tagged_query: Option<String> = None;
    let mut free = String::new();
    for piece in top_level_pieces(text) {
        match piece {
            TextPiece::Free(part) => free.push_str(part),
            TextPiece::Envelope { tag, text: span } => match tag {
                "user_query" => {
                    // 提问标签体内带换行时取 trim 后的（grok 实测口径）。
                    if tagged_query.is_none() {
                        tagged_query = Some(
                            span.split_once('>')
                                .and_then(|(_, body)| body.strip_suffix("</user_query>"))
                                .unwrap_or_default()
                                .trim()
                                .to_string(),
                        );
                    }
                }
                "system-reminder" | "additional_data" => {
                    injected_chars += span.chars().count() as i64;
                }
                _ => blocks.push(TranscriptBlock::HarnessContext {
                    tag: tag.to_string(),
                    text: span.to_string(),
                }),
            },
        }
    }
    // Injected 块携带整段原文（含信封），完整口径的原文树由此渲染；
    // 0 注入字符也保留（前端按字符总数显隐占比条，0 字符不显条）。
    blocks.push(TranscriptBlock::Injected {
        text: text.to_string(),
        injected_chars,
    });
    let free = free.trim();
    if !free.is_empty() {
        blocks.push(TranscriptBlock::Text {
            text: free.to_string(),
        });
    }
    (blocks, tagged_query)
}

/// assistant content 块数组的映射（实测 type：reasoning / text / tool-call）。
/// content 不是数组时退化走通用归一；未识别的块整块 Raw 透传，绝不丢弃。
fn assistant_blocks(content: &Value) -> Vec<TranscriptBlock> {
    let Some(items) = content.as_array() else {
        return transcript::blocks_from_content(content);
    };
    items
        .iter()
        .flat_map(|item| {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
            match kind {
                "reasoning" => item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(|text| {
                        vec![TranscriptBlock::Thinking {
                            text: text.to_string(),
                        }]
                    })
                    .unwrap_or_default(),
                "text" => item
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                    .map(|text| {
                        vec![TranscriptBlock::Text {
                            text: text.to_string(),
                        }]
                    })
                    .unwrap_or_default(),
                "tool-call" => vec![TranscriptBlock::ToolCall {
                    id: item
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    name: item
                        .get("toolName")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    arguments: item
                        .get("args")
                        .map(transcript::arguments_text)
                        .filter(|text| !text.is_empty()),
                }],
                _ => vec![TranscriptBlock::Raw {
                    json: item.to_string(),
                }],
            }
        })
        .collect()
}

/// tool content 块数组的映射（实测 type：tool-result，result 内层带
/// status / success / result）。content 取内层 `result`（真正的载荷，外层
/// 只是包装）；报错判定优先 `success` 布尔，缺失时退回 `status`。
fn tool_blocks(content: &Value) -> Vec<TranscriptBlock> {
    content
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(Value::as_str) != Some("tool-result") {
                        return Some(TranscriptBlock::Raw {
                            json: item.to_string(),
                        });
                    }
                    let result = item.get("result")?;
                    let payload = result.get("result").unwrap_or(result);
                    let is_error = match result.get("success") {
                        Some(Value::Bool(success)) => !success,
                        _ => result.get("status").and_then(Value::as_str) != Some("success"),
                    };
                    Some(TranscriptBlock::ToolResult {
                        call_id: item
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        content: payload.to_string(),
                        is_error,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::storage::Storage;

    const CONV_A: &str = "c0debabe00004c0bae77dd8b3c0d437b";
    const CONV_B: &str = "c0debabe00004c3ebb85008eef9d8809";
    const REQ_A: &str = "c0debabe00004f6eb79735bf263e0539";

    /// 真实日志形状的样板行（形状来自本机实测，字面值缩短）。
    fn marker(conv: &str) -> String {
        format!(
            "2026-10-03 19:26:48.155 [info] [ConversationManager] Restored current conversation: {conv}"
        )
    }
    fn max_tokens(model: &str) -> String {
        format!(
            "2026-10-03 19:26:58.255 [info] [BaseAgent:craft] [BaseAgent] max_tokens resolved: maxTokens=64000, maxInputTokens=192000, promptTokens=6533, modelId={model}, step=1"
        )
    }
    fn tool_action(conv: &str, request: &str, tool: &str) -> String {
        format!(
            "2026-10-03 19:27:11.368 [info] [ToolCallReporter] Emitting tool action to monitor: toolName={tool}, toolCallId=chatcmpl-tool-94cb, conversationId={conv}, requestId={request}"
        )
    }
    fn step_end(ts: &str, request: &str, usage: &str) -> String {
        format!(
            "{ts} [info] [BaseAgent:craft] [{request}] notifyStepEnd, step: 1, requestId: {request}, messageId: 939a, usage: {usage}, isMaxTokenLimit: false"
        )
    }
    const USAGE_A: &str = r#"{"inputTokens":13504,"outputTokens":48,"totalTokens":13552,"cacheTokens":10560,"cachedWriteTokens":0,"cachedMissTokens":2944,"lastTokens":13552,"credit":0,"thinkingTokens":16}"#;

    fn test_db(tag: &str) -> Storage {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toktol-codebuddy-{}-{}-{tag}.db",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_file(&path);
        crate::storage::open(&path).expect("打开测试库")
    }

    /// 临时"安装目录"：`<root>/CodeBuddy CN/`，放日志文件，返回 (适配器, 日志根)。
    fn install_with_log(root: &Path, name: &str, lines: &[String]) -> (CodeBuddyAdapter, PathBuf) {
        let log_host = root
            .join("CodeBuddy CN")
            .join("logs")
            .join("20261003T120000")
            .join("window1")
            .join("exthost")
            .join(LOG_HOST_DIR);
        std::fs::create_dir_all(&log_host).unwrap();
        std::fs::write(log_host.join(name), lines.join("\n") + "\n").unwrap();
        let adapter = CodeBuddyAdapter {
            app_data_dir: Some(root.to_path_buf()),
            local_data_dir: None,
        };
        let logs_root = root.join("CodeBuddy CN").join("logs");
        (adapter, logs_root)
    }

    /// 端到端：两段会话交错、模型切换、工具归属、净桶拆分、全零跳过。
    #[test]
    fn scan_attributes_steps_and_splits_buckets() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lines = [
            marker(CONV_A),
            max_tokens("hy3"),
            tool_action(CONV_A, REQ_A, "list_dir"),
            step_end("2026-10-03 19:27:11.835", REQ_A, USAGE_A),
            // 同会话第二步换模型、带未命中输入；工具归属第二步。
            max_tokens("deepseek-v4.1-flash"),
            tool_action(CONV_A, REQ_A, "read_file"),
            step_end(
                "2026-10-03 19:28:03.348",
                REQ_A,
                r#"{"inputTokens":16287,"outputTokens":68,"cacheTokens":16000,"cachedWriteTokens":0,"cachedMissTokens":287,"thinkingTokens":0}"#,
            ),
            // 会话 B：标记行切换会话，无工具行（requestId 映射缺省回当前会话）。
            marker(CONV_B),
            max_tokens("hy3"),
            step_end(
                "2026-10-03 19:30:20.000",
                "aaa2",
                r#"{"inputTokens":0,"outputTokens":0,"totalTokens":0,"cacheTokens":0,"cachedWriteTokens":0,"cachedMissTokens":0}"#,
            ),
            step_end(
                "2026-10-03 19:30:21.000",
                "aaa2",
                r#"{"inputTokens":100,"outputTokens":10,"cacheTokens":0,"cachedWriteTokens":0,"cachedMissTokens":100,"thinkingTokens":2}"#,
            ),
        ];
        let (adapter, _) = install_with_log(&root, "腾讯云代码助手.log", &lines);
        let storage = test_db("scan");
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 3, "全零步进不入账");
        assert_eq!(report.sessions_created, 2);

        // 实测口径：净输入 = inputTokens − 缓存读 − 缓存写；输出拆出 thinking。
        type BucketRow = (String, String, i64, i64, i64, i64, Option<i64>);
        let rows: Vec<BucketRow> = storage
            .conn()
            .prepare(
                "SELECT s.external_id, r.model_raw, r.input_tokens, r.output_tokens,
                        r.cache_read_tokens, r.cache_write_tokens, r.reasoning_tokens
                 FROM usage_records r JOIN sessions s ON s.id = r.session_id ORDER BY r.ts",
            )
            .unwrap()
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 3);
        // 实测口径：净输入 = inputTokens − 缓存读 − 缓存写；输出拆出 thinking。
        assert_eq!((rows[0].0.as_str(), rows[0].1.as_str()), (CONV_A, "hy3"));
        assert_eq!(
            (rows[0].2, rows[0].3, rows[0].4, rows[0].5, rows[0].6),
            (2944, 32, 10560, 0, Some(16)),
            "步进 1：净输入 2944 = 13504 − 10560；输出 32 = 48 − thinking 16"
        );
        assert_eq!(rows[1].1, "deepseek-v4.1-flash", "模型切换跟随");
        assert_eq!(rows[1].2, 287);
        assert_eq!(rows[2].0, CONV_B, "标记行切换会话后归新会话");
        assert_eq!(rows[2].2, 100);

        // 重扫：同一行重新解析必须被 dedup 拦住。
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 0, "重扫不重复入账");

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 会话库：只读 `session:%` 键；同表的凭据键绝不出现。
    #[test]
    fn db_source_reads_session_keys_only() {
        let root = std::env::temp_dir().join(format!("toktol-codebuddy-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let install = root.join("CodeBuddy CN");
        std::fs::create_dir_all(&install).unwrap();
        let db_path = install.join(SESSIONS_DB_FILE);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             INSERT INTO ItemTable VALUES ('session:feed1234', '{\"conversationId\":\"feed1234\",\"cwd\":\"e:/Dev/DemoApp\",\"userId\":\"u1\",\"title\":\"DemoApp 概览\",\"status\":\"Completed\",\"createdAt\":1789442297853,\"updatedAt\":1789443395162}');
             INSERT INTO ItemTable VALUES ('session:feed5678', '{\"conversationId\":\"feed5678\",\"cwd\":\"\",\"title\":\"\",\"createdAt\":1791026817945}\');
             INSERT INTO ItemTable VALUES ('secret:{\"key\":\"accessToken\"}', '{\"token\":\"x\"}');",
        )
        .unwrap();
        drop(db);

        let adapter = CodeBuddyAdapter {
            app_data_dir: Some(root.clone()),
            local_data_dir: None,
        };
        let source = adapter.read_db_source().unwrap().expect("有库必有来源");
        assert_eq!(source.facts.len(), 1, "空标题空目录的行与凭据键都不入账");
        assert_eq!(source.facts[0].1.session_external_id, "feed1234");
        assert_eq!(source.facts[0].1.title.as_deref(), Some("DemoApp 概览"));
        assert_eq!(
            source.facts[0].1.project_dir.as_deref(),
            Some("e:/Dev/DemoApp")
        );
        assert_eq!(source.facts[0].1.ts_ms, Some(1789443395162));

        // 端到端：库事实入账为会话行（标题/目录落位），凭据键无痕。
        let storage = test_db("db-e2e");
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();
        let titles: Vec<String> = storage
            .conn()
            .prepare("SELECT title FROM sessions ORDER BY external_id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(titles.len(), 1, "无标题无目录的会话行不建行");
        assert_eq!(titles[0], "DemoApp 概览");

        // 标题更新 → 指纹变化 → 全量重放。
        let first = adapter.read_db_source().unwrap().unwrap().fingerprint;
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute(
            "UPDATE ItemTable SET value = replace(value, '概览', '新标题') WHERE key = 'session:feed1234'",
            [],
        )
        .unwrap();
        drop(db);
        let second = adapter.read_db_source().unwrap().unwrap().fingerprint;
        assert_ne!(first, second, "标题变化必须反映到指纹");

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 耗时配对：metrics 行紧随步进行、usage JSON 逐字节相同——时长回填给
    /// 同 JSON 的步进；子代理步进没有 metrics 行，时长为空。
    #[test]
    fn duration_pairs_step_end_with_metrics_line() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-dur-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let usage = r#"{"inputTokens":13504,"outputTokens":48,"totalTokens":13552,"cacheTokens":10560,"cachedWriteTokens":0,"cachedMissTokens":2944,"lastTokens":13552,"credit":0,"thinkingTokens":16}"#;
        let lines = [
            marker(CONV_A),
            max_tokens("hy3"),
            step_end("2026-10-03 19:27:11.835", REQ_A, usage),
            format!(
                "2026-10-03 19:27:11.835 [info] [AgentReporter] [aa14] Step execution metrics: step=1, duration=13586ms, usage={usage}"
            ),
            // 子代理步进：没有 metrics 行。
            "2026-10-03 19:27:20.000 [info] [BaseAgent:code-explorer] [7cee] notifyStepEnd, step: 18, requestId: 7cee, messageId: m, usage: {\"inputTokens\":100,\"outputTokens\":10,\"totalTokens\":110,\"cacheTokens\":0,\"cachedWriteTokens\":0,\"cachedMissTokens\":100,\"thinkingTokens\":0}".to_string(),
        ];
        let (adapter, _) = install_with_log(&root, "腾讯云代码助手.log", &lines);
        let storage = test_db("dur");
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 2);

        let durations: Vec<Option<i64>> = storage
            .conn()
            .prepare("SELECT duration_ms FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            durations,
            vec![Some(13586), None],
            "主代理配到时长，子代理留空"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 缺会话标记 / usage 截断 / 缺时间戳的步进行没有可入账事实（Skip，非
    /// Malformed）。跨行状态（模型、会话）只要齐备就正常入账，不在此列。
    #[test]
    fn incomplete_step_lines_produce_no_facts() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lines = vec![
            // 无任何会话标记：归属缺失。
            max_tokens("hy3"),
            step_end("2026-10-03 19:27:11.835", REQ_A, USAGE_A),
            // usage JSON 截断（轮转尾行）：解析不出完整桶。
            format!(
                "2026-10-03 19:27:12.000 [info] [BaseAgent:craft] notifyStepEnd, requestId: {REQ_A}, usage: {{\"inputTokens\":10"
            ),
            // 时间戳不是日志格式：不编造时间。
            format!("garbage [BaseAgent] notifyStepEnd, requestId: {REQ_A}, usage: {USAGE_A}"),
        ];
        let (adapter, _) = install_with_log(&root, "腾讯云代码助手.log", &lines);
        let parses = adapter.parse_file(&lines.iter().map(String::as_str).collect::<Vec<_>>(), 0);
        assert!(parses.iter().all(|p| matches!(p, LineParse::Skip)));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 临时历史缓存：`<root>/Data/<user>/CodeBuddyIDE/<user>/history/<ws>/<conv>/`
    /// 下写 index.json 与消息文件，返回注入了 local_data_dir 的适配器。
    fn install_with_history(
        root: &Path,
        conv: &str,
        index: &str,
        messages: &[(&str, &str)],
    ) -> CodeBuddyAdapter {
        let dir = root
            .join("Data")
            .join("user-1")
            .join("CodeBuddyIDE")
            .join("user-1")
            .join("history")
            .join("workspace-1")
            .join(conv);
        std::fs::create_dir_all(dir.join("messages")).unwrap();
        std::fs::write(dir.join("index.json"), index).unwrap();
        for (id, body) in messages {
            std::fs::write(dir.join("messages").join(format!("{id}.json")), body).unwrap();
        }
        CodeBuddyAdapter {
            app_data_dir: None,
            local_data_dir: Some(root.join("Data")),
        }
    }

    /// 端到端：本地历史缓存 → 转录条目。user 的信封拆前置 HC 条目、Injected
    /// 占比条与 inputPhrase 提问；assistant 的 reasoning/text/tool-call 与
    /// model；tool 的 tool-result（success=false → is_error）。ts 取 RFC3339。
    #[test]
    fn transcript_reads_local_history_cache() {
        let root = std::env::temp_dir().join(format!("toktol-codebuddy-tr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let conv = "cafe111100004c0bae77dd8b3c0d437b";
        let reminder = "<additional_data>attached files</additional_data>";
        let index = r#"{"messages":[{"id":"m1","role":"user","isComplete":true},{"id":"m2","role":"assistant","isComplete":false},{"id":"m3","role":"tool","isComplete":true},{"id":"m4","role":"user","isComplete":true}]}"#;
        // message / extra 是转义 JSON 字符串（实测形态）。
        let m1 = r#"{"id":"m1","role":"user","createdAt":"2026-09-15T03:23:09.765Z","message":"{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"<user_info>OS win32</user_info><user_query>查看项目内容</user_query><additional_data>attached files</additional_data>\"}]}","extra":"{\"isHelperMessage\":false,\"modelId\":\"hy4\",\"inputPhrase\":[{\"type\":\"normal\",\"content\":\"查看项目内容\"}]}"}"#;
        let m2 = r#"{"id":"m2","role":"assistant","createdAt":"2026-09-15T03:23:40.000Z","message":"{\"role\":\"assistant\",\"content\":[{\"type\":\"reasoning\",\"text\":\"先看结构\"},{\"type\":\"text\",\"text\":\"我来查看。\"},{\"type\":\"tool-call\",\"toolCallId\":\"t1\",\"toolName\":\"list_dir\",\"args\":{\"dir\":\"src\"}}]}","extra":"{\"modelId\":\"hy4\"}"}"#;
        let m3 = r#"{"id":"m3","role":"tool","createdAt":"2026-09-15T03:23:41.000Z","message":"{\"role\":\"tool\",\"content\":[{\"type\":\"tool-result\",\"toolCallId\":\"t1\",\"toolName\":\"list_dir\",\"result\":{\"status\":\"success\",\"success\":false,\"result\":{\"type\":\"list_dir_result\",\"entries\":[\"src\"]}}}]}","extra":"{}"}"#;
        // 旧版本消息可能没有 extra（inputPhrase 缺失）——提问从标签兜底；
        // 标签体内带换行取 trim 后的（grok 实测口径）。
        let m4 = r#"{"id":"m4","role":"user","createdAt":"2026-09-15T03:25:00.000Z","message":"{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"<user_query>\\n直接的问题\\n</user_query>\"}]}"}"#;
        let adapter = install_with_history(
            &root,
            conv,
            index,
            &[("m1", m1), ("m2", m2), ("m3", m3), ("m4", m4)],
        );
        let entries = adapter
            .read_transcript(Path::new("unused.db"), conv)
            .unwrap();
        assert_eq!(entries.len(), 5);

        // 前置 HC 条目：user_info 信封成中性卡（codex 口径）。
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert!(
            matches!(&entries[0].blocks[0], TranscriptBlock::HarnessContext { tag, .. } if tag == "user_info")
        );

        // 用户气泡：Injected 携带整段原文（additional_data 计入注入字符，
        // user_query 标签留在原文树）+ inputPhrase 提问——标签不成卡也不
        // 重复成 Text（inputPhrase 是同一句话的结构化副本）。ts 取 RFC3339。
        assert_eq!(entries[1].role, TranscriptRole::User);
        assert!(
            matches!(&entries[1].blocks[0], TranscriptBlock::Injected { injected_chars, .. } if *injected_chars == reminder.chars().count() as i64)
        );
        assert!(
            matches!(&entries[1].blocks[1], TranscriptBlock::Text { text } if text == "查看项目内容")
        );
        assert_eq!(entries[1].blocks.len(), 2, "<user_query> 不重复成卡/文本");
        assert_eq!(
            entries[1].ts_ms,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-09-15T03:23:09.765Z")
                    .unwrap()
                    .timestamp_millis()
            )
        );

        // assistant：reasoning/text/tool-call 三种块 + model。
        assert_eq!(entries[2].role, TranscriptRole::Assistant);
        assert_eq!(entries[2].model.as_deref(), Some("hy4"));
        assert!(
            matches!(&entries[2].blocks[0], TranscriptBlock::Thinking { text } if text == "先看结构")
        );
        assert!(
            matches!(&entries[2].blocks[1], TranscriptBlock::Text { text } if text == "我来查看。")
        );
        let TranscriptBlock::ToolCall {
            id,
            name,
            arguments,
        } = &entries[2].blocks[2]
        else {
            panic!("应是 ToolCall");
        };
        assert_eq!(
            (id.as_deref(), name.as_deref(), arguments.as_deref()),
            (Some("t1"), Some("list_dir"), Some(r#"{"dir":"src"}"#))
        );

        // tool：success=false → is_error；content 取内层 result 载荷。
        assert_eq!(entries[3].role, TranscriptRole::Tool);
        let TranscriptBlock::ToolResult {
            call_id,
            content,
            is_error,
        } = &entries[3].blocks[0]
        else {
            panic!("应是 ToolResult");
        };
        assert_eq!(call_id.as_deref(), Some("t1"));
        assert!(*is_error);
        // content 是内层 result 的紧凑序列化（键序随 serde_json，按值断言）。
        assert_eq!(
            serde_json::from_str::<Value>(content).unwrap(),
            serde_json::json!({"type": "list_dir_result", "entries": ["src"]})
        );

        // m4：无 extra（inputPhrase 缺失）——提问从 <user_query> 标签兜底，
        // 0 注入字符的 Injected 不影响占比条显隐。
        assert_eq!(entries[4].role, TranscriptRole::User);
        assert!(
            matches!(&entries[4].blocks[0], TranscriptBlock::Injected { injected_chars, .. } if *injected_chars == 0)
        );
        assert!(
            matches!(&entries[4].blocks[1], TranscriptBlock::Text { text } if text == "直接的问题")
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 缓存目录缺失（codeg 托管代理会话的历史是空壳）/ index 无消息 →
    /// Unsupported（详情页占位文案，不合成空壳时间线）。
    #[test]
    fn transcript_without_history_cache_is_unsupported() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-trmiss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let conv = "cafe222200004c0bae77dd8b3c0d437b";
        let adapter = CodeBuddyAdapter {
            app_data_dir: None,
            local_data_dir: Some(root.join("Data")),
        };
        let err = adapter
            .read_transcript(Path::new("unused.db"), conv)
            .unwrap_err();
        assert!(matches!(err, Error::Unsupported), "目录缺失");

        let index = r#"{"messages":[],"requests":[]}"#;
        let adapter = install_with_history(&root, conv, index, &[]);
        let err = adapter
            .read_transcript(Path::new("unused.db"), conv)
            .unwrap_err();
        assert!(matches!(err, Error::Unsupported), "空壳 index");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// helper 消息（UI 生成的提示，isHelperMessage）与损坏的消息文件只跳
    /// 该条，其余照常成条——个别文件不齐不拖垮整条转录。
    #[test]
    fn transcript_skips_helper_and_broken_messages() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-trskip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let conv = "cafe333300004c0bae77dd8b3c0d437b";
        let index = r#"{"messages":[{"id":"m1","role":"user"},{"id":"m2","role":"user"},{"id":"m3","role":"user"},{"id":"m4","role":"user"}]}"#;
        let helper = r#"{"id":"m1","role":"user","message":"{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"请求失败\"}]}","extra":"{\"isHelperMessage\":true}"}"#;
        let broken_outer = r#"{"id":"m2","role":"user","message":"{{{not json""#;
        let broken_inner =
            r#"{"id":"m3","role":"user","message":"not a json object","extra":"{}"}"#;
        let good = r#"{"id":"m4","role":"user","createdAt":"2026-09-15T03:24:00.000Z","message":"{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"<project_context>ctx</project_context>\"}]}","extra":"{\"inputPhrase\":[{\"type\":\"normal\",\"content\":\"你好\"}]}"}"#;
        let adapter = install_with_history(
            &root,
            conv,
            index,
            &[
                ("m1", helper),
                ("m2", broken_outer),
                ("m3", broken_inner),
                ("m4", good),
            ],
        );
        let entries = adapter
            .read_transcript(Path::new("unused.db"), conv)
            .unwrap();
        assert_eq!(entries.len(), 2, "只有 m4 成条：HC 前置条 + 用户气泡");
        assert!(
            matches!(&entries[0].blocks[0], TranscriptBlock::HarnessContext { tag, .. } if tag == "project_context")
        );
        assert!(matches!(&entries[1].blocks[1], TranscriptBlock::Text { text } if text == "你好"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 删除：bundle（键+值+历史目录路径）与历史目录一并入假回收站，vscdb
    /// 行删除；同表 `secret://` 凭据键与他会话行原样。行不存在 → `Ok(None)`
    /// 且不再入桶。
    #[test]
    fn delete_db_session_trashes_and_removes_row() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let conv = "cafe444400004c0bae77dd8b3c0d437b";
        // 历史缓存目录（转录正文）+ vscdb：目标会话、他会话、凭据键。
        let index = r#"{"messages":[{"id":"m1","role":"user"}]}"#;
        let m1 = r#"{"id":"m1","role":"user","message":"{\"role\":\"user\",\"content\":[]}","extra":"{}"}"#;
        let adapter = install_with_history(&root, conv, index, &[("m1", m1)]);
        let db_path = root.join("CodeBuddy CN").join(SESSIONS_DB_FILE);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             INSERT INTO ItemTable VALUES ('session:{conv}', '{{\"conversationId\":\"{conv}\",\"cwd\":\"e:/Dev/DemoApp\",\"title\":\"DemoApp\",\"createdAt\":1789442297853}}');
             INSERT INTO ItemTable VALUES ('session:feed9999', '{{\"conversationId\":\"feed9999\",\"title\":\"Other\",\"createdAt\":1}}');
             INSERT INTO ItemTable VALUES ('secret:{{\"key\":\"accessToken\"}}', '{{\"token\":\"x\"}}');",
        ))
        .unwrap();
        drop(db);

        let staging = root.join("staging");
        let trashed: std::cell::RefCell<Vec<PathBuf>> = std::cell::RefCell::new(Vec::new());
        let trash = |path: &Path| {
            trashed.borrow_mut().push(path.to_path_buf());
            Ok(())
        };
        let bundle = adapter
            .delete_db_session(&db_path, conv, &staging, &trash)
            .unwrap()
            .expect("行在必删");
        assert_eq!(trashed.borrow().len(), 2, "bundle + 历史目录");
        assert!(trashed.borrow().iter().any(|p| p == &bundle));
        let text = std::fs::read_to_string(&bundle).unwrap();
        assert!(
            text.contains(conv) && text.contains("DemoApp"),
            "bundle 带键与值"
        );

        // vscdb：目标行没了，凭据键与他会话原样。
        let db = rusqlite::Connection::open(&db_path).unwrap();
        let gone: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM ItemTable WHERE key = ?1",
                [format!("session:{conv}")],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(gone, 0);
        let secret: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM ItemTable WHERE key LIKE 'secret:%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(secret, 1, "凭据键绝不触碰");
        let other: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM ItemTable WHERE key = 'session:feed9999'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(other, 1);
        drop(db);

        // 行已不在 → Ok(None)，无可清之物不再入桶。
        let again = adapter
            .delete_db_session(&db_path, conv, &staging, &trash)
            .unwrap();
        assert!(again.is_none());
        assert_eq!(trashed.borrow().len(), 2);

        std::fs::remove_dir_all(&root).unwrap();
    }

    /// 防复活：已删会话的用量行永远躺在共享日志里，日志重解析（游标归零）
    /// 时墓碑拦住建行——不出现 0 用量空壳；存量用量保持脱离归属（统计保留）。
    #[test]
    fn deleted_session_is_not_revived_on_rescan() {
        let root =
            std::env::temp_dir().join(format!("toktol-codebuddy-revive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lines = [
            marker(CONV_A),
            max_tokens("hy3"),
            step_end("2026-10-03 19:27:11.835", REQ_A, USAGE_A),
        ];
        let (adapter, _) = install_with_log(&root, "腾讯云代码助手.log", &lines);
        let storage = test_db("revive");
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.sessions_created, 1);

        // 模拟用户删除：记墓碑 + 清归属（会话删除编排层的口径）。
        storage
            .record_deleted_session("codebuddy", CONV_A, 0)
            .unwrap();
        let session_id: i64 = storage
            .conn()
            .query_row("SELECT id FROM sessions", [], |row| row.get(0))
            .unwrap();
        storage.delete_session(session_id).unwrap();

        // 游标归零 = 日志轮转/重写，整文件重解析。
        storage
            .conn()
            .execute("UPDATE scanned_files SET parsed_bytes = 0", [])
            .unwrap();
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();

        let (sessions, usage, orphan): (i64, i64, i64) = storage
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sessions),
                        (SELECT COUNT(*) FROM usage_records),
                        (SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(sessions, 0, "墓碑拦住建行，空壳不复活");
        assert_eq!(usage, 1, "存量用量保留");
        assert_eq!(orphan, 1, "归属保持脱离");

        std::fs::remove_dir_all(&root).unwrap();
    }
}
