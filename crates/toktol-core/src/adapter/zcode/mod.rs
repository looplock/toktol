//! zcode 适配器：两个事实来源，各管一摊。
//!
//! **用量**：应用库 `~/.zcode/cli/db/db.sqlite` 的 `model_usage` 表（[`Self::read_db_source`]，
//! 与工具官方用量页同源）。rollout 日志在默认配置下会被工具滚动清理（生产默认
//! `MAX_ROLLOUT_FILES = 3`，超出删最旧），且行内消息是 delta/tail 压缩形态——
//! 用量靠日志文件既不全也不稳；应用库的行不受轮转影响，还完整覆盖 rollout
//! 记不到的 subagent / compact / session_title 请求。行键 = model_usage.id，天然幂等。
//!
//! **转录**：`~/.zcode/cli/rollout/model-io-*.jsonl` 模型 I/O 日志（现读，绝不落库）。
//! rollout 文件仍在的会话（登记源即文件）照常读；已被轮转的会话（登记源是应用库）
//! 由 [`Self::resolve_transcript_source`] 按 external_id 重新定位文件，定位不到报
//! `core.source_gone`——用量统计仍在，只是详情不可查看。
//!
//! rollout 文件扫描仍保留，但只产出会话簿记事实（标题、项目目录），不产用量：
//! 同一会话的文件扫描先于库扫描跑，`get_or_create_session` 的 INSERT OR IGNORE
//! 保证有文件的会话登记源始终指向 rollout 文件，转录与字节区间索引的簿记锚点
//! 不变；无文件的会话由库扫描建行。
//!
//! 项目目录与会话标题：session 表的 directory / title（官方值，比日志里的首条
//! 用户文本可靠）；库打不开（没装/被锁）时标题回退到请求消息里的首条非注入
//! 用户文本，目录则是 None。该库的另一身份是转录的登记锚点（见上），红线：
//! 只 SELECT session / model_usage 两张表，凭据类数据绝不触碰；请求行携带的
//! `request.headers`（含供应商密钥）在转录路径上也不落库，dedup 只存行键。
//!
//! 会话转录：对 `querySource:"main_turn"` 的请求消息块做**相邻前缀去重**。
//! 消息块挂在 `request.messages`（body 里只有请求参数），压缩形态由
//! `messageOffset` 表达：0 = 全量快照，等于基线长度 = 纯增量，基线中部 =
//! 尾部块（老历史被挤出快照）。重建时 `基线前缀[:offset] + 本行` 拼出完整
//! 快照，与上一快照求最长公共前缀，只把新增尾巴展开成条目（user 提问、
//! assistant 的思考/文本/工具调用、tool 结果按日志原顺序自然出现）；末轮
//! 回复不会再进任何后续快照，从响应字段的 text / reasoningText / toolCalls
//! 补上。标题生成（`session_title`）等旁路请求是独立小对话，整行跳过。
//! 快照间消息会变的非内容差异要先归掉：`cache_control` 缓存标记随请求
//! 尾部移动（递归剥除）；历史被压缩改写或新输入并进上一条提问时，公共
//! 前缀变短，退回"只展开没见过的/追加的块"，避免重放已展示的旧消息。
//!
//! 转录索引：本适配器支持把转录建为字节区间索引（`index.rs`），多 GB 会话
//! 的详情页按索引 seek 读片段，不再全量现读。
//!
//! 用量语义（实测）：应用库 `input_tokens` 含缓存读（OpenAI 语义，同 grok /
//! workbuddy）——入库前拆出，否则缓存读按输入价二次计费；`reasoning_tokens`
//! 单列不在输出内——与统一模型"输出不含推理"口径一致；实际服务的模型用
//! `model_id`（如 `GLM-5.3-Flash`），存储层归一化折叠大小写与供应商前缀。

use std::collections::HashMap;

pub(crate) mod index;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::transcript::{self, TranscriptBlock, TranscriptEntry, TranscriptRole};
use super::{Adapter, DbSource, LineFacts, LineParse, UsageFacts};
use crate::error::{Error, Result};
use crate::model::{TokenUsage, Tool};

/// 会话标题超长截断——标题是提问，不是全文备份。取值优先级：应用库
/// session.title（工具自己的标题）→ 请求消息里首条非注入的用户文本。
const TITLE_MAX_CHARS: usize = 200;

/// 删除导出的白名单从表（实测 schema 2026-10）：全部随 session 级联，键列
/// 都是 session_id，唯 session_task_link 的子端按 child_session_id 级联
/// （父端 SET NULL，行保留，无需导出）。part 经 message 二级级联，但自带
/// session_id 列，直接按列导出。白名单之外——input_history、local_setting
/// 等工具全局数据与凭据——绝不触碰；会话行本身随删除语句单独导出。
const DELETE_EXPORT_TABLES: [(&str, &str); 10] = [
    ("message", "session_id"),
    ("part", "session_id"),
    ("model_usage", "session_id"),
    ("tool_usage", "session_id"),
    ("turn_usage", "session_id"),
    ("session_entry", "session_id"),
    ("session_input", "session_id"),
    ("session_target", "session_id"),
    ("todo", "session_id"),
    ("session_task_link", "child_session_id"),
];

/// 应用库 session 表的一行元数据；标题为空串/纯空白视同没有。
#[derive(Debug, Default, Clone)]
pub(crate) struct SessionInfo {
    directory: Option<String>,
    title: Option<String>,
}

/// zcode 的适配器实现；逐行解析，无跨行状态。`db_path` / `rollout_dir` 为
/// `None` 时取 home 下的默认路径；测试注入临时路径。`session_meta` 是本轮扫描
/// 的会话→元数据缓存（惰性加载一次）——`adapters()` 每轮新建适配器，缓存不跨轮。
pub struct ZcodeAdapter {
    pub(crate) db_path: Option<PathBuf>,
    pub(crate) rollout_dir: Option<PathBuf>,
    pub(crate) session_meta: OnceLock<HashMap<String, SessionInfo>>,
}

impl ZcodeAdapter {
    /// 应用库路径：用量事实来源，也是无文件会话的登记锚点与标题/目录查找表。
    fn db_source_path(&self) -> Option<PathBuf> {
        self.db_path.clone().or_else(|| {
            crate::paths::home_dir()
                .map(|home| home.join(".zcode").join("cli").join("db").join("db.sqlite"))
        })
    }

    /// rollout 目录；日志目录不可用（无 home）时 None。
    fn rollout_dir(&self) -> Option<PathBuf> {
        self.rollout_dir.clone().or_else(|| {
            crate::paths::home_dir().map(|home| home.join(".zcode").join("cli").join("rollout"))
        })
    }

    /// 会话的 rollout 文件名：zcode 侧 `model-io-{sessionId}.jsonl`，sessionId
    /// 是 `sess_uuid` 形态、文件名安全，可直接拼接。
    fn rollout_file_name(external_id: &str) -> String {
        format!("model-io-{external_id}.jsonl")
    }

    /// 会话的元数据；映射来源不可用或会话不在库里就是 `None`。
    fn session_meta(&self, external_id: &str) -> Option<SessionInfo> {
        let map = self
            .session_meta
            .get_or_init(|| self.load_session_meta().unwrap_or_default());
        map.get(external_id).cloned()
    }

    /// 只读打开应用的 db.sqlite，取 session 表的 id → (directory, title)。
    /// 失败（没装、被写锁、结构变了）返回 Err，调用方按空映射处理。
    fn load_session_meta(&self) -> rusqlite::Result<HashMap<String, SessionInfo>> {
        let Some(path) = self.db_source_path() else {
            return Ok(HashMap::new());
        };
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = db.prepare(
            "SELECT id, directory, title FROM session
             WHERE directory IS NOT NULL OR TRIM(COALESCE(title, '')) <> ''",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, directory, title) = row?;
            let directory = directory.filter(|d| !d.trim().is_empty());
            let title = title
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            if directory.is_some() || title.is_some() {
                map.insert(id, SessionInfo { directory, title });
            }
        }
        Ok(map)
    }
}

impl Adapter for ZcodeAdapter {
    fn tool(&self) -> Tool {
        Tool::ZCode
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        self.rollout_dir().into_iter().collect()
    }

    fn is_session_log(&self, path: &Path) -> bool {
        path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
    }

    /// 单文件可长到数 GB（实测 2.1 GB）且逐行自洽：必须走流式增量，
    /// 否则超限后整个会话的后续用量全部丢失。
    fn supports_streaming_scan(&self) -> bool {
        true
    }

    fn db_source_path(&self) -> Option<PathBuf> {
        self.db_source_path()
    }

    /// rollout 文件出转录、应用库出用量与标题：会话事实横跨两处，删除必须
    /// 双清——只回收文件会留下可被重扫重建的空壳。
    fn purge_db_rows_for_file_sessions(&self) -> bool {
        true
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
            "zcode",
            &DELETE_EXPORT_TABLES,
        )
    }

    /// 应用库全量读取：session 表出标题/目录（被轮转会话的唯一事实来源），
    /// model_usage 表出请求级用量。行键即主键 id，重放由 dedup_key 拦住。
    fn read_db_source(&self) -> Result<Option<DbSource>> {
        let Some(path) = self.db_source_path() else {
            return Ok(None);
        };
        // 库不存在 = 工具没装，常态而非错误；已登记路径的消失由扫描层对账标记。
        if !path.exists() {
            return Ok(None);
        }
        let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| Error::Internal(format!("zcode db.sqlite 打开失败 {path:?}: {err}")))?;

        // 指纹 = 两表的行数与水位（行只增不改，水位足以判定"有无新增"）。
        let (usage_count, usage_max_rowid): (i64, i64) = db.query_row(
            "SELECT COUNT(*), COALESCE(MAX(rowid), 0) FROM model_usage",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (session_count, session_max_updated): (i64, i64) = db.query_row(
            "SELECT COUNT(*), COALESCE(MAX(time_updated), 0) FROM session",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let fingerprint = Sha256::new()
            .chain_update(usage_count.to_le_bytes())
            .chain_update(usage_max_rowid.to_le_bytes())
            .chain_update(session_count.to_le_bytes())
            .chain_update(session_max_updated.to_le_bytes())
            .finalize()
            .to_vec();

        let session_meta = self.load_session_meta().unwrap_or_default();
        let mut facts = Vec::new();

        // 会话事实：标题与项目目录。rollout 已被轮转的会话只有这里有名字。
        {
            let mut stmt = db
                .prepare("SELECT id, COALESCE(time_updated, time_created, 0) FROM session")
                .map_err(Error::from)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(Error::from)?;
            for row in rows {
                let (id, ts) = row.map_err(Error::from)?;
                let meta = session_meta.get(&id).cloned().unwrap_or_default();
                if meta.title.is_none() && meta.directory.is_none() {
                    continue; // 无标题无目录的会话行没有可入账事实。
                }
                facts.push((
                    format!("session:{id}"),
                    LineFacts {
                        dedup_suffix: None,
                        request_count: 1,
                        session_external_id: id,
                        title: meta.title.map(truncate_title),
                        project_dir: meta.directory,
                        ts_ms: Some(ts),
                        usage: None,
                    },
                ));
            }
        }

        // 用量事实：status/agent 全口径（非 completed 行 token 恒 0，自然过滤）。
        {
            let mut stmt = db
                .prepare(
                    "SELECT id, session_id, started_at, completed_at, duration_ms, model_id,
                            input_tokens, output_tokens, reasoning_tokens,
                            cache_creation_input_tokens, cache_read_input_tokens
                     FROM model_usage ORDER BY rowid",
                )
                .map_err(Error::from)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(UsageRow {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        started_at: row.get(2)?,
                        completed_at: row.get(3)?,
                        duration_ms: row.get(4)?,
                        model_id: row.get(5)?,
                        input: row.get(6)?,
                        output: row.get(7)?,
                        reasoning: row.get(8)?,
                        cache_write: row.get(9)?,
                        cache_read: row.get(10)?,
                    })
                })
                .map_err(Error::from)?;
            for row in rows {
                let row = row.map_err(Error::from)?;
                let Some((key, session_external_id, usage)) = row.facts() else {
                    continue; // 归属/模型/时间不全或全零用量：无可入账事实。
                };
                facts.push((
                    key,
                    LineFacts {
                        dedup_suffix: None,
                        request_count: 1,
                        session_external_id,
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

    /// 登记源是应用库的会话（db 扫描建行）：按 external_id 重新定位 rollout
    /// 文件，文件名固定为 `model-io-{sessionId}.jsonl`。已被轮转回收时返回
    /// `core.source_gone`——用量已在库，只是转录不可查看。
    fn resolve_transcript_source(&self, source_file: &Path, external_id: &str) -> Result<PathBuf> {
        let is_rollout_file = source_file
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("model-io-"));
        if is_rollout_file {
            // 登记源就是 rollout 文件：它可能已被回收/移动。转录的全部入口
            // （turns / page / build 索引）都经这里定位，存在性检查统一收口
            // ——缺失报 SourceGone，而不是让后续 File::open 抛 DataFile、
            // 前端显示成笼统的"读取失败"。
            if source_file.exists() {
                return Ok(source_file.to_path_buf());
            }
            return Err(Error::SourceGone(source_file.to_path_buf()));
        }
        let Some(dir) = self.rollout_dir() else {
            return Err(Error::SourceGone(source_file.to_path_buf()));
        };
        let path = dir.join(Self::rollout_file_name(external_id));
        if path.exists() {
            Ok(path)
        } else {
            Err(Error::SourceGone(path))
        }
    }

    /// 登记源是应用库的会话没有可入桶的文件（工具的库绝不进回收站）；
    /// 真实 rollout 文件的删除走默认路径。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        if self.db_source_path().is_some_and(|db| db == source_file) {
            return vec![];
        }
        vec![source_file.to_path_buf()]
    }

    /// 转录走字节区间索引：单会话日志可达数 GB，全量现读会打爆内存与磁盘。
    fn transcript_indexed(&self) -> bool {
        true
    }

    fn build_transcript_index(
        &self,
        storage: &crate::storage::Storage,
        file_id: i64,
        source: &Path,
        external_id: &str,
        checkpoint_bytes: u64,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &dyn Fn(u64, u64),
    ) -> crate::error::Result<bool> {
        index::build(
            storage,
            file_id,
            source,
            external_id,
            checkpoint_bytes,
            cancel,
            progress,
        )
    }

    fn transcript_entry_from_frag(
        &self,
        kind: i64,
        frag: &[u8],
        skip_blocks: Option<i64>,
        role: &str,
        ts_ms: Option<i64>,
        model: Option<&str>,
        frag_meta: Option<&str>,
    ) -> crate::error::Result<Option<TranscriptEntry>> {
        index::entry_from_frag(kind, frag, skip_blocks, role, ts_ms, model, frag_meta)
    }

    fn parse_line(&self, line: &str) -> LineParse {
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(_) => return LineParse::Malformed,
        };
        if raw.kind != "model_io" {
            return LineParse::Skip;
        }
        // no-session 汇总文件里的行无法归属会话，跳过而不是编造。
        let Some(session_external_id) = raw
            .session_id
            .filter(|id| !id.trim().is_empty())
            .map(|id| id.trim().to_string())
        else {
            return LineParse::Skip;
        };

        // 用量一律走应用库（read_db_source），rollout 行只产会话簿记事实。
        // 标题：应用库的官方值优先；回退到请求消息里的首条真实用户文本
        // （user 角色里混着 harness 注入的上下文包裹，不是提问）。
        // 回退要对整行二次解析（轻量解析跳过了正文），只在拿不到官方标题时发生。
        // 无标题也照常产事实：会话登记源必须锚在 rollout 文件上（转录索引、
        // 删除入桶都依赖它），项目目录同理。
        let meta = self.session_meta(&session_external_id);
        let title = meta
            .as_ref()
            .and_then(|m| m.title.clone())
            .or_else(|| title_from_request(line))
            .map(truncate_title);

        LineParse::Facts(Box::new(LineFacts {
            dedup_suffix: None,
            request_count: 1,
            session_external_id,
            title,
            project_dir: meta.as_ref().and_then(|m| m.directory.clone()),
            ts_ms: raw
                .started_at
                .as_deref()
                .or(raw.completed_at.as_deref())
                .and_then(parse_ts_ms),
            usage: None,
        }))
    }

    /// 逐请求压缩块的对话重建：`基线前缀[:offset] + 本行消息` 拼出完整
    /// 快照，相邻快照求最长公共前缀，只展开新增尾巴。消息正文只在用户
    /// 点开会话详情时现读，绝不落库（与扫描口径互补）；headers 一律不碰。
    fn read_transcript(
        &self,
        source_file: &Path,
        external_id: &str,
    ) -> crate::error::Result<Vec<TranscriptEntry>> {
        let text = transcript::read_text(source_file)?;
        let mut entries = Vec::new();
        let mut diff = SnapshotDiff::new();
        // 末轮回复：出现在响应字段里、但不再进任何后续快照的历史，留到
        // 最后补上；一旦有更新的快照进来，说明该回复已进历史，作废。
        let mut pending: Option<(TranscriptResponse, Option<i64>)> = None;

        for line in text.lines() {
            let Ok(raw) = serde_json::from_str::<TranscriptLine>(line) else {
                continue;
            };
            if !line_applies(&raw, external_id) {
                continue;
            }
            let ts_ms = raw_ts_ms(&raw);
            let mut raw = raw;
            let Some(req) = raw.request.take() else {
                continue;
            };
            let Some(mut messages) = req.messages else {
                continue;
            };
            let offset = usize::try_from(req.message_offset.unwrap_or(0)).unwrap_or(0);
            messages.iter_mut().for_each(strip_cache_control);

            // 日志被轮转从中间开始时首行 offset 会超出基线：按"接在已有
            // 历史之后"处理，能看多少看多少。
            let splice = diff.splice_at(offset);
            let mut effective = diff.baseline()[..splice].to_vec();
            effective.extend(messages);

            entries.extend(
                derive_entries(&mut diff, &effective, ts_ms)
                    .into_iter()
                    .map(|(entry, _)| entry),
            );
            diff.commit(splice, offset, effective);
            pending = raw.response.filter(|r| r.has_content()).map(|r| (r, ts_ms));
        }

        if let Some((response, ts_ms)) = pending {
            let blocks = response.blocks();
            if !blocks.is_empty() {
                entries.push(TranscriptEntry {
                    role: TranscriptRole::Assistant,
                    ts_ms,
                    model: response.model_id.filter(|m| !m.trim().is_empty()),
                    blocks,
                });
            }
        }
        Ok(entries)
    }
}

/// 行级过滤（转录与索引建站共用）：model_io、归属本会话、主轮次请求。
fn line_applies(raw: &TranscriptLine, external_id: &str) -> bool {
    if raw.kind != "model_io" {
        return false;
    }
    if raw
        .session_id
        .as_ref()
        .is_none_or(|id| id.trim() != external_id)
    {
        return false;
    }
    raw.query_source
        .as_deref()
        .is_none_or(|source| source == "main_turn")
}

fn raw_ts_ms(raw: &TranscriptLine) -> Option<i64> {
    raw.started_at
        .as_deref()
        .or(raw.completed_at.as_deref())
        .and_then(parse_ts_ms)
}

/// 快照 diff 的产物：当前快照里要展开成条目的消息。
pub(crate) enum SnapshotDelta {
    /// 整条消息（正常路径；含压缩改写后的全新消息）。
    Message { index: usize },
    /// 追加式改写：旧消息并入了新内容块，只展开追加段。`item` 是新消息在
    /// 当前快照里的下标，`old` 是它在旧基线里的下标，`skip_items` 是旧
    /// content 数组的长度（展开时跳过这些块）。
    Appended {
        item: usize,
        old: usize,
        skip_items: usize,
    },
}

/// 相邻快照的前缀 diff：与转录/索引两条消费路径共用，保证两条管线对
/// "哪些消息要展开"永远判定一致。
pub(crate) struct SnapshotDiff {
    baseline: Vec<serde_json::Value>,
    /// 基线[0] 在会话全史中的绝对下标。zcode 快照是"最近 64 条"滑窗，
    /// messageOffset 是窗口在全史中的绝对起点；日志开头不在文件里（续接
    /// 会话、头部被回收）时基线从非零绝对下标起步，拿 offset 直接当基线
    /// 下标会让两窗的重叠段整窗重复拼接（每行 64 条、404 条的根源）。
    base_start: usize,
}

impl SnapshotDiff {
    pub(crate) fn new() -> Self {
        Self {
            baseline: Vec::new(),
            base_start: 0,
        }
    }

    /// 旧基线（diff 之后、commit 之前可访问，供追加式展开取旧消息）。
    pub(crate) fn baseline(&self) -> &[serde_json::Value] {
        &self.baseline
    }

    /// 本行消息数组的拼接点：offset（全史绝对起点）换算成基线下标，钳到
    /// [0, 基线长度]——越界说明日志中部有缺口，按接在已有历史之后处理。
    pub(crate) fn splice_at(&self, offset: usize) -> usize {
        offset
            .saturating_sub(self.base_start)
            .min(self.baseline.len())
    }

    /// 对一条新快照做 diff。`messages` 必须已剥 cache_control。
    pub(crate) fn diff(&mut self, messages: &[serde_json::Value]) -> Vec<SnapshotDelta> {
        let mut deltas = Vec::new();
        let common = self
            .baseline
            .iter()
            .zip(messages)
            .take_while(|(old, new)| old == new)
            .count();
        let fresh = &messages[common..];
        // 历史被压缩改写时公共前缀变短：尾巴里混着"已展示过的保留消息"，
        // 按整条相等跳过它们，只展开没见过的（摘要、新提问）。同位置的
        // 消息若只是被追加了内容块（新输入并进上一条提问），只出追加的
        // 块，不重放旧块。
        let rewritten = common < self.baseline.len();
        for (offset, message) in fresh.iter().enumerate() {
            if rewritten {
                if self.baseline.contains(message) {
                    continue;
                }
                if let Some(old) = self.baseline.get(common + offset)
                    && appended_blocks(old, message).is_some()
                {
                    let skip_items = old
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map_or(0, |items| items.len());
                    deltas.push(SnapshotDelta::Appended {
                        item: common + offset,
                        old: common + offset,
                        skip_items,
                    });
                    continue;
                }
            }
            deltas.push(SnapshotDelta::Message {
                index: common + offset,
            });
        }
        deltas
    }

    /// diff 之后把基线推进到新快照。拼接点为 0 时新基线从本行消息的绝对
    /// 起点 `offset` 起步（基线为空，或本行覆盖到基线起点之前）；拼接点
    /// 大于 0 时基线[0] 不变。
    pub(crate) fn commit(
        &mut self,
        splice: usize,
        offset: usize,
        messages: Vec<serde_json::Value>,
    ) {
        if splice == 0 {
            self.base_start = offset;
        }
        self.baseline = messages;
    }
}

/// 一条新快照的展开条目（转录与索引建站共用的推导：哪些 delta 产出条目、
/// 条目长什么样），连同它对应的 delta 一起返回——建站器靠 delta 定位
/// 条目的源文件字节区间。`diff` 在此之后才 commit，追加式展开从
/// `diff.baseline()` 取旧消息。
fn derive_entries(
    diff: &mut SnapshotDiff,
    messages: &[serde_json::Value],
    ts_ms: Option<i64>,
) -> Vec<(TranscriptEntry, SnapshotDelta)> {
    let mut out = Vec::new();
    for delta in diff.diff(messages) {
        match delta {
            SnapshotDelta::Message { index } => {
                if let Some(entry) = history_entry(&messages[index], ts_ms) {
                    out.push((entry, delta));
                }
            }
            SnapshotDelta::Appended { item, old, .. } => {
                let (Some(new_value), Some(old_value)) =
                    (messages.get(item), diff.baseline().get(old))
                else {
                    continue;
                };
                if let Some(extra) = appended_blocks(old_value, new_value) {
                    let blocks = extra_blocks(old_value, &extra);
                    if !blocks.is_empty()
                        && let Some(mut entry) = history_entry(old_value, ts_ms)
                    {
                        entry.blocks = blocks;
                        out.push((entry, delta));
                    }
                }
            }
        }
    }
    out
}

/// 历史消息 → 转录条目。system 是 harness 的静态提示，不是对话内容；其余
/// 角色按块归一，无块（空 assistant 占位等）不给条目。模型名历史里不带，
/// 只在末轮响应条目上有。
fn history_entry(message: &serde_json::Value, ts_ms: Option<i64>) -> Option<TranscriptEntry> {
    let obj = message.as_object()?;
    let role = obj.get("role")?.as_str()?;
    let content = obj.get("content").unwrap_or(&serde_json::Value::Null);
    let (role, blocks) = match role {
        "user" => (TranscriptRole::User, user_blocks(content)),
        "assistant" => (
            TranscriptRole::Assistant,
            assistant_blocks(message, content),
        ),
        "tool" => (
            TranscriptRole::Tool,
            vec![TranscriptBlock::ToolResult {
                call_id: obj
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                content: transcript::value_text(content),
                is_error: false,
            }],
        ),
        _ => return None,
    };
    if blocks.is_empty() {
        return None;
    }
    Some(TranscriptEntry {
        role,
        ts_ms,
        model: None,
        blocks,
    })
}

/// user 消息块：文本块按注入分段（`<system-reminder>` 归 Injected、正文留
/// Text），其余块（图片路径标签等）走通用归一。
fn user_blocks(content: &serde_json::Value) -> Vec<TranscriptBlock> {
    transcript::blocks_from_content(content)
        .into_iter()
        .flat_map(|block| match block {
            TranscriptBlock::Text { text } => segment_user_text(&text),
            kept => vec![kept],
        })
        .collect()
}

/// zcode user 消息里的注入段：`<system-reminder>`（技能清单、agentsMd 等
/// harness 上下文）与 `<in-app-browser-context>`（IDE 内嵌浏览器的环境状态）。
/// 都不是用户的话。
const INJECTED_TAGS: &[(&str, &str)] = &[
    ("<system-reminder", "</system-reminder>"),
    ("<in-app-browser-context", "</in-app-browser-context>"),
];

/// zcode 注入模板里真实提问的引导标记（实测恒定格式，紧随注入段之后）。
const REQUEST_MARKER: &str = "## My request for ZCode:";

/// 标记之后的文本（已 trim，可能为空）；无标记返回 None。注入模板恒以
/// 该标记引导真实提问，标记本身是脚手架、不是提问。
fn after_request_marker(text: &str) -> Option<&str> {
    text.split_once(REQUEST_MARKER).map(|(_, rest)| rest.trim())
}

/// zcode 的 user 文本分段。注入段（[`INJECTED_TAGS`]）剥出来归注入口径：
/// 纯注入消息（剥除后无正文）整条归 [`TranscriptBlock::HarnessContext`]
/// 事件卡，不进用户气泡也不带占比条；混了正文的走 workbuddy 口径——
/// [`TranscriptBlock::Injected`] 带整条原文，标记（[`REQUEST_MARKER`]）或
/// 剥除后的正文单独成块。
fn segment_user_text(text: &str) -> Vec<TranscriptBlock> {
    let (cleaned, spans) = transcript::split_tag_spans(text, INJECTED_TAGS);
    if spans.is_empty() {
        return (!cleaned.trim().is_empty())
            .then_some(TranscriptBlock::Text { text: cleaned })
            .into_iter()
            .collect();
    }
    // 注入模板的固定格式：标记后的文本才是真实提问。标记后无正文视同纯
    // 注入；没有标记（旧版日志、防御）退回剥除后的正文。
    let body = match after_request_marker(&cleaned) {
        Some(body) if !body.is_empty() => body,
        Some(_) => "",
        None => cleaned.trim(),
    };
    if body.is_empty() {
        return vec![TranscriptBlock::HarnessContext {
            tag: span_tag(&spans[0]).to_string(),
            text: text.to_string(),
        }];
    }
    vec![
        TranscriptBlock::Injected {
            text: text.to_string(),
            injected_chars: spans.iter().map(|s| s.chars().count() as i64).sum(),
        },
        TranscriptBlock::Text {
            text: body.to_string(),
        },
    ]
}

/// 注入段原文的包裹标签名（`<tag …>` → `tag`），HarnessContext 事件卡用。
fn span_tag(span: &str) -> &str {
    let rest = span.strip_prefix('<').unwrap_or(span);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// 历史里的 assistant 消息：推理（reasoning_content）→ Thinking，正文 →
/// 文本，tool_calls（OpenAI 形状，arguments 是 JSON 文本）→ ToolCall。
fn assistant_blocks(
    message: &serde_json::Value,
    content: &serde_json::Value,
) -> Vec<TranscriptBlock> {
    let mut blocks = Vec::new();
    if let Some(text) = message
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .filter(|text| !text.is_empty())
    {
        blocks.push(TranscriptBlock::Thinking {
            text: text.to_string(),
        });
    }
    match content {
        serde_json::Value::String(text) if !text.is_empty() => {
            blocks.push(TranscriptBlock::Text { text: text.clone() })
        }
        other => blocks.extend(transcript::blocks_from_content(other)),
    }
    if let Some(calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for call in calls {
            let function = call.get("function");
            blocks.push(TranscriptBlock::ToolCall {
                id: call.get("id").and_then(|v| v.as_str()).map(str::to_string),
                name: function
                    .and_then(|f| f.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                arguments: function
                    .and_then(|f| f.get("arguments"))
                    .map(transcript::arguments_text),
            });
        }
    }
    blocks
}

/// 递归剥掉 `cache_control`（camelCase 变体一并）：缓存断点标记挂在请求
/// 尾部的消息上、进了历史就被摘掉，留着会让相邻快照的前缀比较失效（同一
/// 提问重复展开）。
fn strip_cache_control(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.remove("cache_control");
            map.remove("cacheControl");
            for child in map.values_mut() {
                strip_cache_control(child);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_cache_control),
        _ => {}
    }
}

/// 同位置消息被追加内容块（新输入并进上一条提问，追加段是本轮真实的新
/// 话）：旧消息的内容数组是新消息的严格前缀时返回追加的原始块；其余
/// 改写一律 None，走通用路径。
fn appended_blocks(
    old: &serde_json::Value,
    new: &serde_json::Value,
) -> Option<Vec<serde_json::Value>> {
    if old.get("role") != new.get("role") {
        return None;
    }
    let (Some(serde_json::Value::Array(old_items)), Some(serde_json::Value::Array(new_items))) =
        (old.get("content"), new.get("content"))
    else {
        return None;
    };
    if new_items.len() <= old_items.len() || new_items[..old_items.len()] != *old_items {
        return None;
    }
    Some(new_items[old_items.len()..].to_vec())
}

/// 追加块 → 转录块：按消息角色归一（user 走注入分段，tool 摊平成结果，
/// 其余走通用块归一）。
fn extra_blocks(message: &serde_json::Value, extra: &[serde_json::Value]) -> Vec<TranscriptBlock> {
    let role = message.get("role").and_then(|v| v.as_str()).unwrap_or("");
    extra
        .iter()
        .flat_map(|item| match role {
            "user" => user_blocks(item),
            "tool" => vec![TranscriptBlock::ToolResult {
                call_id: message
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                content: transcript::value_text(item),
                is_error: false,
            }],
            _ => transcript::blocks_from_content(item),
        })
        .collect()
}

/// 转录侧的请求行结构：消息保持原样 Value（要做跨行前缀比较），响应字段
/// 与扫描侧的 RawLine 分开（此处要正文，扫描侧只要用量）。
#[derive(Deserialize)]
struct TranscriptLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "querySource")]
    query_source: Option<String>,
    #[serde(rename = "startedAt")]
    started_at: Option<String>,
    #[serde(rename = "completedAt")]
    completed_at: Option<String>,
    #[serde(default)]
    request: Option<TranscriptRequest>,
    #[serde(default)]
    response: Option<TranscriptResponse>,
}

#[derive(Deserialize)]
struct TranscriptRequest {
    /// 消息块挂在 request 下（body 里只有 model/tools 等请求参数）。行内是
    /// 压缩形态，`messageOffset` 统一表达三种 kind：0 = 全量快照（full），
    /// 等于前基线长度 = 纯增量（delta），落在基线中部 = 从该下标起的尾部
    /// 块（tail，老历史被挤出快照）。重建一律 `基线前缀[:offset] + 本行`
    /// 拼出完整历史，交给前缀 diff。
    #[serde(default)]
    messages: Option<Vec<serde_json::Value>>,
    #[serde(rename = "messageOffset", default)]
    message_offset: Option<i64>,
}

#[derive(Deserialize)]
struct TranscriptResponse {
    #[serde(rename = "modelId")]
    model_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(rename = "reasoningText", default)]
    reasoning_text: Option<String>,
    #[serde(rename = "toolCalls", default)]
    tool_calls: Option<Vec<TranscriptToolCall>>,
}

struct TranscriptToolCall {
    id: Option<String>,
    name: Option<String>,
    input: Option<serde_json::Value>,
}

impl<'de> Deserialize<'de> for TranscriptToolCall {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Shape {
            id: Option<String>,
            name: Option<String>,
            function: Option<ShapeFunction>,
            input: Option<serde_json::Value>,
        }
        #[derive(Default, Deserialize)]
        struct ShapeFunction {
            name: Option<String>,
            arguments: Option<serde_json::Value>,
        }
        let shape = Shape::deserialize(deserializer)?;
        let Shape {
            id,
            name,
            function,
            input,
        } = shape;
        // 响应里的调用是 {id, name, input}；兼容历史形状 {id, function:{name, arguments}}。
        let (fallback_name, fallback_args) = match function {
            Some(f) => (f.name, f.arguments),
            None => (None, None),
        };
        Ok(TranscriptToolCall {
            id,
            name: name.or(fallback_name),
            input: input.or(fallback_args),
        })
    }
}

impl TranscriptResponse {
    fn has_content(&self) -> bool {
        self.text.as_deref().is_some_and(|t| !t.is_empty())
            || self
                .reasoning_text
                .as_deref()
                .is_some_and(|t| !t.is_empty())
            || self
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
    }

    /// 末轮回复 → 内容块：思考、文本、工具调用，按日志呈现顺序。
    fn blocks(&self) -> Vec<TranscriptBlock> {
        let mut blocks = Vec::new();
        if let Some(text) = self
            .reasoning_text
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            blocks.push(TranscriptBlock::Thinking {
                text: text.to_string(),
            });
        }
        if let Some(text) = self.text.as_deref().filter(|text| !text.is_empty()) {
            blocks.push(TranscriptBlock::Text {
                text: text.to_string(),
            });
        }
        for call in self.tool_calls.iter().flatten() {
            blocks.push(TranscriptBlock::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.input.as_ref().map(transcript::arguments_text),
            });
        }
        blocks
    }
}

fn parse_ts_ms(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// 标题回退：对整行做第二次反序列化，取请求消息里的首条真实用户文本。
/// 轻量解析（[`RawLine`]）不物化请求正文——那是单行可达数十 MB 的历史快照，
/// 逐行扫描时物化它会把大文件首扫拖慢一个数量级。
fn title_from_request(line: &str) -> Option<String> {
    let raw: RawLineWithRequest = serde_json::from_str(line).ok()?;
    let messages = raw.request?.messages?;
    first_user_text(&messages)
}

/// 请求消息列表里的首条真实用户文本。system / assistant / 工具结果一律不算；
/// user 角色里 harness 注入的上下文（[`INJECTED_TAGS`]：技能清单、浏览器
/// 状态等）不是用户的提问——带 [`REQUEST_MARKER`] 的取标记后的提问，纯
/// 注入包裹跳过后看下一条。
fn first_user_text(messages: &[RawMessage]) -> Option<String> {
    messages
        .iter()
        .filter(|m| m.role == "user")
        .find_map(|m| text_of(&m.content).and_then(|text| real_user_text(&text)))
}

/// user 文本里的真实提问：注入包裹开头时只有标记后的文本算数；其余原样。
fn real_user_text(text: &str) -> Option<String> {
    if !is_context_wrapper(text) {
        return Some(text.to_string());
    }
    after_request_marker(text)
        .filter(|body| !body.is_empty())
        .map(str::to_string)
}

/// harness 注入的上下文包裹文本（`<system-reminder>`、`<in-app-browser-context>`）。
fn is_context_wrapper(text: &str) -> bool {
    let head = text.trim_start();
    INJECTED_TAGS.iter().any(|(tag, _)| head.starts_with(tag))
}

fn text_of(content: &Option<serde_json::Value>) -> Option<String> {
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
struct RawLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "startedAt")]
    started_at: Option<String>,
    #[serde(rename = "completedAt")]
    completed_at: Option<String>,
    // 刻意不声明 request / response：serde 跳过未知字段不物化正文，标题回退
    // 走二次解析；用量已改走应用库，行里只剩会话簿记要看的几个字段。
}

/// 标题回退用的完整形态：正文在这里才物化（见 [`title_from_request`]）。
#[derive(Deserialize)]
struct RawLineWithRequest {
    #[serde(default)]
    request: Option<RawRequest>,
}

#[derive(Deserialize)]
struct RawRequest {
    #[serde(default)]
    messages: Option<Vec<RawMessage>>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Option<serde_json::Value>,
}

/// `model_usage` 的一行，只取入账所需列（INTEGER 列容忍 NULL）。
struct UsageRow {
    id: String,
    session_id: Option<String>,
    started_at: Option<i64>,
    completed_at: Option<i64>,
    duration_ms: Option<i64>,
    model_id: Option<String>,
    input: Option<i64>,
    output: Option<i64>,
    reasoning: Option<i64>,
    cache_write: Option<i64>,
    cache_read: Option<i64>,
}

impl UsageRow {
    /// → (行键, 会话归属, 用量事实)。归属/模型/时间不全或全零用量返回 None。
    /// 时间取 started_at，缺失退 completed_at（schema 非空，退路只为防御）。
    fn facts(&self) -> Option<(String, String, UsageFacts)> {
        let session_id = self
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        let model_raw = self
            .model_id
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())?;
        let ts_ms = self.started_at.or(self.completed_at)?;
        // OpenAI 语义：cached ⊆ input，输入记拆掉缓存读的净量（同 rollout 口径）。
        let cache_read = self.cache_read.unwrap_or(0).max(0);
        let usage = TokenUsage {
            input_tokens: (self.input.unwrap_or(0).max(0) - cache_read).max(0),
            output_tokens: self.output.unwrap_or(0).max(0),
            cache_read_tokens: cache_read,
            cache_write_tokens: self.cache_write.unwrap_or(0).max(0),
            reasoning_tokens: self.reasoning.filter(|&n| n > 0),
        };
        if usage.input_tokens == 0
            && usage.output_tokens == 0
            && usage.cache_read_tokens == 0
            && usage.cache_write_tokens == 0
        {
            return None;
        }
        Some((
            self.id.clone(),
            session_id.to_string(),
            UsageFacts {
                ts_ms,
                model_raw: model_raw.to_string(),
                model: model_raw.to_string(),
                duration_ms: self.duration_ms.map(|d| d.max(0)),
                usage,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> ZcodeAdapter {
        ZcodeAdapter {
            // 注入必然不存在的库路径：只读打开失败 → 空映射，测试不碰真实 home。
            db_path: Some(std::env::temp_dir().join(format!(
                "toktol-zcode-no-such-db-{}.sqlite",
                std::process::id()
            ))),
            rollout_dir: None,
            session_meta: OnceLock::new(),
        }
    }

    /// 建一个 zcode 形状的应用库（session + model_usage，只含适配器 SELECT 的
    /// 列；真实 DDL 列远多于此，多出的列不影响读取）。
    fn zcode_db(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "toktol-zcode-db-{}-{tag}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, directory TEXT, title TEXT,
                time_created INTEGER, time_updated INTEGER);
             CREATE TABLE model_usage (
                id TEXT PRIMARY KEY, session_id TEXT, started_at INTEGER,
                completed_at INTEGER, duration_ms INTEGER, model_id TEXT,
                input_tokens INTEGER, output_tokens INTEGER, reasoning_tokens INTEGER,
                cache_creation_input_tokens INTEGER, cache_read_input_tokens INTEGER);",
        )
        .unwrap();
        path
    }

    const LINE: &str = r#"{"completedAt":"2026-09-23T15:22:27.778Z","durationMs":5793,"requestId":"0e27bdd2","attempt":1,"model":{"modelId":"glm-5.3-flash","providerId":"9ebe9de6"},"request":{"messages":[{"role":"system","content":"You are ZCode."},{"role":"user","content":[{"type":"text","text":"  帮我整体审查一下项目  "}]}],"messageOffset":0},"response":{"finishReason":"tool-calls","modelId":"z-ai/glm-5.3-flash","responseId":"gen-1790176938-x","usage":{"inputTokens":21293,"outputTokens":181,"totalTokens":21474,"cacheReadTokens":21248,"reasoningTokens":111}},"sessionId":"sess_02836629-940a","startedAt":"2026-09-23T15:22:21.985Z","type":"model_io"}"#;

    #[test]
    fn model_io_line_yields_session_bookkeeping_without_usage() {
        let facts = match adapter().parse_line(LINE) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.session_external_id, "sess_02836629-940a");
        assert_eq!(facts.project_dir, None, "映射库不可用时目录为 None");
        assert_eq!(facts.title.as_deref(), Some("帮我整体审查一下项目"));
        assert_eq!(facts.ts_ms, Some(1_790_176_941_985));
        assert_eq!(
            facts.usage, None,
            "用量一律来自应用库，rollout 行不再产用量"
        );
    }

    #[test]
    fn lines_without_title_still_yield_session_facts() {
        // no-session 汇总文件：行不带 sessionId，无法归属会话。
        let no_session =
            r#"{"type":"model_io","attempt":1,"completedAt":"2026-09-23T15:22:27.778Z"}"#;
        assert_eq!(adapter().parse_line(no_session), LineParse::Skip);
        // 无标题来源（报错请求、无官方标题、正文里没有真实提问）也照常产事实：
        // 会话登记源必须锚在 rollout 文件上，只是没有标题可写。
        let errored = r#"{"type":"model_io","sessionId":"sess_x","completedAt":"2026-09-23T15:22:27.778Z","error":{"name":"Error","message":"v4 session stopped"}}"#;
        match adapter().parse_line(errored) {
            LineParse::Facts(facts) => {
                assert_eq!(facts.session_external_id, "sess_x");
                assert_eq!(facts.title, None, "无标题来源不编造标题");
                assert_eq!(facts.ts_ms, Some(1_790_176_947_778));
            }
            other => panic!("期望 Facts，实际 {other:?}"),
        }
        assert_eq!(adapter().parse_line("{ not json"), LineParse::Malformed);
    }

    #[test]
    fn zero_usage_lines_still_yield_title_facts() {
        // 有标题即有事实（用量是否为零与簿记无关）。
        let titled = r#"{"type":"model_io","sessionId":"sess_x","startedAt":"2026-09-23T15:22:21.985Z","response":{"modelId":"m","usage":{"inputTokens":0,"outputTokens":0}},"request":{"messages":[{"role":"user","content":"开个新话题"}]}}"#;
        match adapter().parse_line(titled) {
            LineParse::Facts(facts) => {
                assert_eq!(facts.usage, None);
                assert_eq!(facts.title.as_deref(), Some("开个新话题"));
            }
            other => panic!("期望 Facts，实际 {other:?}"),
        }
    }

    #[test]
    fn system_and_tool_messages_never_become_titles() {
        let line = r#"{"type":"model_io","sessionId":"sess_x","startedAt":"2026-09-23T15:22:21.985Z","response":{"modelId":"m","usage":{"inputTokens":1,"outputTokens":1}},"request":{"messages":[{"role":"system","content":"system prompt"},{"role":"user","content":[{"type":"tool_result","content":"x"}]}]}}"#;
        // 系统提示与工具结果都不算提问：事实照常产出，但标题为空。
        match adapter().parse_line(line) {
            LineParse::Facts(facts) => {
                assert_eq!(facts.session_external_id, "sess_x");
                assert_eq!(facts.title, None, "系统/工具文本绝不充当标题");
            }
            other => panic!("期望 Facts，实际 {other:?}"),
        }
    }

    /// 实测：user 角色的前几条是 harness 注入的 <system-reminder> 上下文
    /// （技能清单、agentsMd），真实提问在后面——标题必须取到后者。
    #[test]
    fn system_reminder_wrapped_user_texts_are_not_titles() {
        let line = r#"{"type":"model_io","sessionId":"sess_x","startedAt":"2026-09-23T15:22:21.985Z","response":{"modelId":"m","usage":{"inputTokens":1,"outputTokens":1}},"request":{"messages":[{"role":"user","content":"<system-reminder>\nThe following skills are available\n</system-reminder>"},{"role":"user","content":[{"type":"text","text":"帮我整体查看这个项目"}]}]}}"#;
        let facts = match adapter().parse_line(line) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.title.as_deref(), Some("帮我整体查看这个项目"));
    }

    /// 应用库里的 title 是工具自己的官方标题，优先于请求消息回退来源。
    #[test]
    fn app_db_title_wins_over_request_messages() {
        let db_path =
            std::env::temp_dir().join(format!("toktol-zcode-title-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT);
             INSERT INTO session (id, directory, title) VALUES
                 ('sess_a', 'E:\\proj\\demo', '禁用编译测试相关命令');",
        )
        .unwrap();
        drop(db);

        let adapter = ZcodeAdapter {
            db_path: Some(db_path.clone()),
            rollout_dir: None,
            session_meta: OnceLock::new(),
        };
        let line = r#"{"type":"model_io","sessionId":"sess_a","startedAt":"2026-09-23T15:22:21.985Z","response":{"modelId":"m","usage":{"inputTokens":1,"outputTokens":1}},"request":{"messages":[{"role":"user","content":"<system-reminder>ctx</system-reminder>"},{"role":"user","content":"首条提问"}]}}"#;
        let facts = match adapter.parse_line(line) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.title.as_deref(), Some("禁用编译测试相关命令"));
        assert_eq!(facts.project_dir.as_deref(), Some("E:\\proj\\demo"));

        std::fs::remove_file(&db_path).unwrap();
    }

    #[test]
    fn only_jsonl_files_count_as_session_logs() {
        let adapter = adapter();
        assert!(adapter.is_session_log(Path::new("/r/model-io-sess_x.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/r/model-io-keep.txt")));
    }

    /// 转录重建：主对话快照求前缀差只展开新增尾巴，标题生成旁路整行跳过，
    /// 末轮回复从响应字段补上。
    #[test]
    fn transcript_dedupes_snapshots_and_appends_final_response() {
        let path = transcript_fixture(
            [
                // 第一轮：注入 + 真实提问；响应带思考、文本与工具调用。
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"system","content":"You are ZCode."},{"role":"user","content":"<system-reminder>技能清单</system-reminder>真实提问"}]},"response":{"modelId":"z-ai/glm","text":"回答","reasoningText":"思考","toolCalls":[{"id":"c1","name":"Bash","input":{"command":"ls"}}]}}"#,
                // 标题生成旁路：独立小对话，不进转录。
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"session_title","request":{"messages":[{"role":"system","content":"Generate a concise title"},{"role":"user","content":"真实提问"}]},"response":{"text":"标题"}}"#,
                // 第二轮：增量块携带第一轮的回复进历史（assistant 历史形状，
                // 含 reasoning_content 与 OpenAI 形状 tool_calls），offset 指向
                // 基线尾部；响应只有文本，成为末轮回复条目。
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:23:00.000Z","request":{"messages":[{"role":"assistant","content":"回答","reasoning_content":"思考","tool_calls":[{"id":"c1","type":"function","function":{"name":"Bash","arguments":"{\"command\":\"ls\"}"}}]},{"role":"tool","tool_call_id":"c1","content":"输出"},{"role":"user","content":"第二个问题"}],"messageOffset":2},"response":{"modelId":"z-ai/glm","text":"最终回答"}}"#,
                // 别的会话的行不进本会话转录。
                r#"{"type":"model_io","sessionId":"sess_b","querySource":"main_turn","request":{"messages":[{"role":"user","content":"别人家的问题"}]},"response":{"text":"别人家的回答"}}"#,
            ]
            .join("\n")
            .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let shapes: Vec<String> = entries
            .iter()
            .map(|e| {
                e.blocks
                    .iter()
                    .map(|b| match b {
                        TranscriptBlock::Text { .. } => "T",
                        TranscriptBlock::Thinking { .. } => "K",
                        TranscriptBlock::ToolCall { .. } => "C",
                        TranscriptBlock::ToolResult { .. } => "R",
                        TranscriptBlock::Injected { .. } => "I",
                        _ => "?",
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            shapes,
            vec!["IT", "KTC", "R", "T", "T"],
            "user→assistant→tool→user→末轮回复"
        );
        assert_eq!(entries[0].ts_ms, Some(1_790_176_941_985));
        let TranscriptBlock::Injected {
            text,
            injected_chars,
        } = &entries[0].blocks[0]
        else {
            panic!("注入段应归 Injected");
        };
        assert_eq!(text, "<system-reminder>技能清单</system-reminder>真实提问");
        assert_eq!(*injected_chars, 39);
        let TranscriptBlock::Text { text } = &entries[0].blocks[1] else {
            panic!("剥除注入后应剩正文");
        };
        assert_eq!(text, "真实提问");
        assert_eq!(entries[1].model, None, "历史消息不带模型");
        let TranscriptBlock::ToolCall {
            id,
            name,
            arguments,
        } = &entries[1].blocks[2]
        else {
            panic!("OpenAI 形状 tool_calls 应归 ToolCall");
        };
        assert_eq!(
            (id.as_deref(), name.as_deref(), arguments.as_deref()),
            (Some("c1"), Some("Bash"), Some(r#"{"command":"ls"}"#))
        );
        let TranscriptBlock::ToolResult {
            call_id,
            content,
            is_error,
        } = &entries[2].blocks[0]
        else {
            panic!("tool 角色应归 ToolResult");
        };
        assert_eq!(
            (call_id.as_deref(), content.as_str(), *is_error),
            (Some("c1"), "输出", false)
        );
        assert_eq!(
            entries[4].model.as_deref(),
            Some("z-ai/glm"),
            "末轮回复带模型"
        );
        assert_eq!(entries[4].ts_ms, Some(1_790_176_980_000));
    }

    /// 实测：会话开头的 agentsMd 注入是**独立 user 消息**、整条都是
    /// `<system-reminder>`——与 codex 的全局注入同类，归 HarnessContext
    /// 事件卡（中性、不进用户气泡），紧随的裸文本提问正常开轮次。
    #[test]
    fn standalone_system_reminder_message_is_harness_context() {
        let path = transcript_fixture(
            [r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"system","content":"You are ZCode."},{"role":"user","content":"<system-reminder>\nagentsMd 内容\n</system-reminder>\n"},{"role":"user","content":"查看项目内容"}]},"response":{"text":"回答"}}"#]
                .join("\n")
                .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        assert_eq!(
            entries.len(),
            3,
            "system 提示不产条目；响应不再进后续快照，作末轮回复补上"
        );
        match &entries[0].blocks[..] {
            [TranscriptBlock::HarnessContext { tag, text }] => {
                assert_eq!(tag, "system-reminder");
                assert_eq!(
                    text,
                    "<system-reminder>\nagentsMd 内容\n</system-reminder>\n"
                );
            }
            other => panic!("纯注入消息应归 HarnessContext，实际 {other:?}"),
        }
        match &entries[1].blocks[..] {
            [TranscriptBlock::Text { text }] => assert_eq!(text, "查看项目内容"),
            other => panic!("提问应是独立 Text 条目，实际 {other:?}"),
        }
    }

    /// 实测：IDE 内嵌浏览器的环境状态注入与真实提问混在同一 user 消息里，
    /// 标记（## My request for ZCode:）之后才是提问——对齐 workbuddy 口径：
    /// Injected 带整条原文进占比条，提问单独成气泡，注入 XML 不再裸奔在
    /// 用户气泡里。
    #[test]
    fn browser_context_injection_is_split_from_request() {
        let path = transcript_fixture(
            [r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"user","content":"<in-app-browser-context source=\"ambient-ui-state\">\nambient UI state\n</in-app-browser-context>\n\n## My request for ZCode:\n实现一下"}]},"response":{"text":"回答"}}"#]
                .join("\n")
                .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        assert_eq!(entries.len(), 2, "注入+提问一条、末轮回复一条");
        match &entries[0].blocks[..] {
            [
                TranscriptBlock::Injected {
                    text,
                    injected_chars,
                },
                TranscriptBlock::Text { text: question },
            ] => {
                assert_eq!(question, "实现一下");
                assert_eq!(
                    text,
                    "<in-app-browser-context source=\"ambient-ui-state\">\nambient UI state\n</in-app-browser-context>\n\n## My request for ZCode:\n实现一下"
                );
                let expected = "<in-app-browser-context source=\"ambient-ui-state\">\nambient UI state\n</in-app-browser-context>".chars().count() as i64;
                assert_eq!(*injected_chars, expected, "占比条分子只数注入段");
            }
            other => panic!("应为 Injected + Text，实际 {other:?}"),
        }
    }

    /// 实测（防御）：注入段整条包裹、没有标记也不带正文——与 codex 全局
    /// 注入同类，归 HarnessContext 事件卡，标签名取实际包裹标签。
    #[test]
    fn pure_browser_context_message_is_harness_context() {
        let path = transcript_fixture(
            [r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"user","content":"<in-app-browser-context source=\"ambient-ui-state\">\nambient UI state\n</in-app-browser-context>"}]},"response":{"text":"回答"}}"#]
                .join("\n")
                .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        match &entries[0].blocks[..] {
            [TranscriptBlock::HarnessContext { tag, .. }] => {
                assert_eq!(tag, "in-app-browser-context");
            }
            other => panic!("纯注入消息应归 HarnessContext，实际 {other:?}"),
        }
    }

    /// 实测：分享图片的提问 = 注入包裹 + 标记提问 + 图片块 + 引用文本
    /// 尾巴。气泡应渲染提问与图片卡：注入原文进占比条，引用文本与图片块
    /// 合并（图片路径取 source.path，文件名取 placeholder）。
    #[test]
    fn browser_context_message_with_image_keeps_question_and_image() {
        let path = transcript_fixture(
            [r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"user","content":[{"type":"text","text":"<in-app-browser-context source=\"ambient-ui-state\">\nambient\n</in-app-browser-context>\n\n## My request for ZCode:\n是不是就是这样的。"},{"type":"image","mediaType":"image/png","dataUrl":"[dataUrl omitted from model-io: image/png, 115314 chars]","source":{"id":"turn-attachment-1","kind":"inline","mimeType":"image/png","placeholder":"image.png","path":"C:\\imgs\\x.png"}},{"type":"text","text":"[Image: source: C:\\imgs\\x.png]"}]}]},"response":{"text":"回答"}}"#]
                .join("\n")
                .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let blocks = &entries[0].blocks;
        assert_eq!(blocks.len(), 3, "注入、提问、图片——引用文本已折叠");
        match &blocks[..] {
            [
                TranscriptBlock::Injected { .. },
                TranscriptBlock::Text { text },
                TranscriptBlock::Image { path, filename, .. },
            ] => {
                assert_eq!(text, "是不是就是这样的。");
                assert_eq!(path, r"C:\imgs\x.png");
                assert_eq!(filename, "image.png");
            }
            other => panic!("应为 Injected + Text + Image，实际 {other:?}"),
        }
    }

    /// 标题回退：首条 user 文本是浏览器状态注入包裹时，取标记后的提问，
    /// 而不是把注入原文当标题。
    #[test]
    fn browser_context_request_marker_becomes_title() {
        let line = r#"{"type":"model_io","sessionId":"sess_x","startedAt":"2026-09-23T15:22:21.985Z","response":{"modelId":"m","usage":{"inputTokens":1,"outputTokens":1}},"request":{"messages":[{"role":"user","content":"<in-app-browser-context source=\"ambient-ui-state\">\nambient\n</in-app-browser-context>\n\n## My request for ZCode:\n查看项目内容，主要查看总览页面。"}]}}"#;
        let facts = match adapter().parse_line(line) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(
            facts.title.as_deref(),
            Some("查看项目内容，主要查看总览页面。")
        );
    }

    /// 实测：历史超过 64 条后快照变成"最近 64 条"滑窗，messageOffset 是
    /// 窗口在全史中的绝对起点。日志开头不在文件里（续接会话）时首行
    /// offset>0，基线必须记住自己的绝对起点，否则两窗的重叠段整窗重复
    /// 展开（每行 64 条、转录膨胀一个量级的根源）。此处用窗口=2 的迷你
    /// 形状复现同一机制。
    #[test]
    fn transcript_sliding_window_snapshots_do_not_duplicate() {
        let path = transcript_fixture(
            [
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"assistant","content":"答1"},{"role":"tool","tool_call_id":"c1","content":"果1"}],"messageOffset":2},"response":{"text":"临时"}}"#,
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"tool","tool_call_id":"c1","content":"果1"},{"role":"user","content":"问2"}],"messageOffset":3},"response":{"text":"临时2"}}"#,
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":"问2"},{"role":"assistant","content":"答2"}],"messageOffset":4},"response":{"text":"答3"}}"#,
            ]
            .join("\n")
            .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let seq: Vec<&str> = entries
            .iter()
            .filter_map(|e| {
                e.blocks.iter().find_map(|b| match b {
                    TranscriptBlock::Text { text } => Some(text.as_str()),
                    TranscriptBlock::ToolResult { content, .. } => Some(content.as_str()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            seq,
            vec!["答1", "果1", "问2", "答2", "答3"],
            "两窗重叠的果1只展开一次，末轮回复答3落为条目"
        );
    }

    /// 历史被压缩改写（公共前缀变短）：已展示过的保留消息按整条相等跳过，
    /// 没见过的摘要与新提问照常展开；被压缩吞掉的上轮响应不复活。
    #[test]
    fn transcript_rewrite_only_emits_unseen_messages() {
        let path = transcript_fixture(
            [
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"system","content":"You are ZCode."},{"role":"user","content":"问1"},{"role":"assistant","content":"答1"},{"role":"tool","tool_call_id":"c1","content":"结果"}]},"response":{"text":"答2"}}"#,
                // 历史被压缩改写：尾部块从基线下标 1 起替换，公共前缀只剩
                // system。已展示过的保留消息按整条相等跳过，没见过的摘要与
                // 新提问照常展开；被压缩吞掉的上轮响应不复活。
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":"摘要：早期对话"},{"role":"tool","tool_call_id":"c1","content":"结果"},{"role":"user","content":"问3"}],"messageOffset":1},"response":{"text":"答3"}}"#,
            ]
            .join("\n")
            .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let texts: Vec<String> = entries
            .iter()
            .flat_map(|e| {
                e.blocks.iter().filter_map(|b| match b {
                    TranscriptBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            texts,
            vec!["问1", "答1", "摘要：早期对话", "问3", "答3"],
            "保留的 tool 结果不重放，答2 被压缩吞掉不复活"
        );
    }

    /// 末轮是错误中断（无响应）时不补条目，也不复活更早已进历史的回复。
    #[test]
    fn transcript_errored_final_line_adds_nothing() {
        let path = transcript_fixture(
            [
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":"问1"}]},"response":{"text":"答1"}}"#,
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":"问1"},{"role":"assistant","content":"答1"},{"role":"user","content":"问2"}]},"error":{"name":"Error","message":"stopped"}}"#,
            ]
            .join("\n")
            .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let texts: Vec<String> = entries
            .iter()
            .flat_map(|e| {
                e.blocks.iter().filter_map(|b| match b {
                    TranscriptBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(texts, vec!["问1", "答1", "问2"], "错误行无响应可补");
    }

    /// cache_control 缓存标记挂在当轮最新消息上、进历史就被摘掉：剥除后
    /// 前缀比较不失效，同一提问不重复展开。
    #[test]
    fn transcript_cache_control_marker_does_not_duplicate_user_turn() {
        let path = transcript_fixture(
            [
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":[{"type":"text","text":"5173 被占用会怎么样","cache_control":{"type":"ephemeral"}}]}]},"response":{"text":"答1"}}"#,
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"assistant","content":"答1"},{"role":"user","content":"追问"}],"messageOffset":1},"response":{"text":"答2"}}"#,
            ]
            .join("\n")
            .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let texts: Vec<String> = entries
            .iter()
            .flat_map(|e| {
                e.blocks.iter().filter_map(|b| match b {
                    TranscriptBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(texts, vec!["5173 被占用会怎么样", "答1", "追问", "答2"]);
    }

    /// 新输入被并进上一条提问（同位置消息追加了 text 块）：只展开追加的
    /// 块，旧块不重放。
    #[test]
    fn transcript_appended_user_block_emits_only_the_extra() {
        let path = transcript_fixture(
            [
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"文件内容"}]}]},"response":{"text":"答1"}}"#,
                // 新输入并进上一条提问：增量块从基线下标 0 起替换，同位置
                // 消息追加了 text 块——只展开追加的块，旧块不重放。
                r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","request":{"messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"文件内容"},{"type":"text","text":"本地有 dev 服务器已经启动，你可以看到。"}]},{"role":"assistant","content":"答2"}],"messageOffset":0},"response":{"text":"答3"}}"#,
            ]
            .join("\n")
            .as_bytes(),
        );
        let entries = adapter().read_transcript(&path, "sess_a").unwrap();
        let shapes: Vec<String> = entries
            .iter()
            .map(|e| {
                format!(
                    "{:?}:{}",
                    e.role,
                    e.blocks
                        .iter()
                        .map(|b| match b {
                            TranscriptBlock::Text { text } => format!("T({text})"),
                            TranscriptBlock::ToolResult { content, .. } => {
                                format!("R({content})")
                            }
                            _ => "?".into(),
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                )
            })
            .collect();
        assert_eq!(
            shapes,
            vec![
                "User:R(文件内容)",
                "User:T(本地有 dev 服务器已经启动，你可以看到。)",
                "Assistant:T(答2)",
                "Assistant:T(答3)",
            ],
            "追加段单独成条，tool_result 不重放"
        );
    }

    /// 金标准对拍：索引建站 + 分页读回的转录必须与全量现读（read_transcript）
    /// 完全一致。fixture 覆盖真实日志的压缩形态：全量快照（offset 0）、纯
    /// 增量（offset = 基线长）、尾部块（offset 落基线中部）、追加式消息、
    /// 压缩改写、旁路请求、他会话行、末轮响应。
    #[test]
    fn transcript_index_pages_match_read_transcript() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

        let lines = [
            // 轮 1：全量快照（offset 0），注入 + 真实提问；响应带模型与文本。
            r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"system","content":"You are ZCode."},{"role":"user","content":"<system-reminder>技能清单</system-reminder>问1"}],"messageOffset":0},"response":{"modelId":"z-ai/glm","text":"答1"}}"#,
            // 旁路请求：整行跳过。
            r#"{"type":"model_io","sessionId":"sess_a","querySource":"session_title","request":{"messages":[{"role":"user","content":"问1"}],"messageOffset":0},"response":{"text":"标题"}}"#,
            // 轮 2：纯增量（offset = 基线长），回复进历史 + 新提问。
            r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:23:00.000Z","request":{"messages":[{"role":"assistant","content":"答1"},{"role":"user","content":"问2"}],"messageOffset":2},"response":{"modelId":"z-ai/glm","text":"答2"}}"#,
            // 他会话的行：跳过。
            r#"{"type":"model_io","sessionId":"sess_b","querySource":"main_turn","request":{"messages":[{"role":"user","content":"别人家的问题"}],"messageOffset":0},"response":{"text":"别人家的回答"}}"#,
            // 轮 3：新输入并进上一条提问（追加式增量），只展开追加段。
            r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:24:00.000Z","request":{"messages":[{"role":"user","content":[{"type":"text","text":"问2"},{"type":"text","text":"补充一下"}]}],"messageOffset":4},"response":{"modelId":"z-ai/glm","text":"答3"}}"#,
            // 轮 4：尾部块（offset 落基线中部）：历史被压缩改写，保留消息
            // 按整条相等跳过。
            r#"{"type":"model_io","sessionId":"sess_a","querySource":"main_turn","startedAt":"2026-09-23T15:25:00.000Z","request":{"messages":[{"role":"user","content":"摘要：早期对话"},{"role":"user","content":"问3"}],"messageOffset":1},"response":{"modelId":"z-ai/glm","text":"答4"}}"#,
        ];
        let path = transcript_fixture((lines.join("\n") + "\n").as_bytes());
        let path_str = path.to_string_lossy().into_owned();

        let storage = {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let db_path = std::env::temp_dir().join(format!(
                "toktol-zcode-golden-{}-{}.db",
                std::process::id(),
                SEQ.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            let _ = std::fs::remove_file(&db_path);
            crate::storage::open(std::path::Path::new(&db_path)).unwrap()
        };
        let file_id = storage
            .upsert_scanned_file(
                &path_str,
                "zcode",
                &[],
                (lines.join("\n") + "\n").len() as i64,
                0,
                0,
            )
            .unwrap();
        storage
            .get_or_create_session(
                &crate::storage::NewSession {
                    tool: "zcode".into(),
                    external_id: "sess_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let adapter = adapter();
        let expected = adapter.read_transcript(&path, "sess_a").unwrap();
        assert!(!expected.is_empty(), "fixture 必须产出条目");

        // 建站（1KB 检查点 → 多次提交路径也被走到）。
        let cancel = AtomicBool::new(false);
        let complete = crate::sessions::build_transcript_index(
            &storage,
            "zcode",
            "sess_a",
            1024,
            &cancel,
            &|_, _| {},
        )
        .unwrap();
        assert!(complete);

        // 分页读回（含向前翻页）拼回全量，与全量现读对拍。
        let mut pages: Vec<crate::sessions::TranscriptPageEntry> = Vec::new();
        loop {
            let after = pages
                .last()
                .map_or(-1, |p: &crate::sessions::TranscriptPageEntry| p.seq);
            let page = crate::sessions::session_transcript_page(
                &storage,
                "zcode",
                "sess_a",
                Some(after),
                None,
                2,
            )
            .unwrap();
            assert!(page.built);
            let done = page.entries.is_empty();
            pages.extend(page.entries);
            if done {
                break;
            }
        }
        let got: Vec<TranscriptEntry> = pages.into_iter().map(|p| p.entry).collect();
        assert_eq!(
            format!("{got:?}"),
            format!("{expected:?}"),
            "索引读回与全量现读必须一致"
        );

        // 向前翻页：取 seq<2 的最后两行。
        let page =
            crate::sessions::session_transcript_page(&storage, "zcode", "sess_a", None, Some(2), 2)
                .unwrap();
        let seqs: Vec<i64> = page.entries.iter().map(|p| p.seq).collect();
        assert_eq!(seqs, vec![0, 1], "before 窗口应取 seq<2 的最后两行");

        // 目录：三个用户询问轮。
        let turns = crate::sessions::session_transcript_turns(&storage, "zcode", "sess_a").unwrap();
        assert!(turns.built);
        // 问1、问2、追加段（"补充一下"自成条目即自成轮）、摘要行、问3。
        assert_eq!(turns.turns.iter().filter(|t| !t.is_summary).count(), 5);
        assert_eq!(turns.turns[0].snippet, "问1");
    }

    /// 取消后续建：第一次取消（部分提交），第二次不取消建完，结果仍与
    /// 全量现读一致。
    #[test]
    fn transcript_index_resume_after_cancel() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

        let lines: Vec<String> = history_lines(60);
        let path = transcript_fixture((lines.join("\n") + "\n").as_bytes());
        let path_str = path.to_string_lossy().into_owned();

        let storage = {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let db_path = std::env::temp_dir().join(format!(
                "toktol-zcode-resume-{}-{}.db",
                std::process::id(),
                SEQ.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            let _ = std::fs::remove_file(&db_path);
            crate::storage::open(std::path::Path::new(&db_path)).unwrap()
        };
        let file_id = storage
            .upsert_scanned_file(
                &path_str,
                "zcode",
                &[],
                (lines.join("\n") + "\n").len() as i64,
                0,
                0,
            )
            .unwrap();
        storage
            .get_or_create_session(
                &crate::storage::NewSession {
                    tool: "zcode".into(),
                    external_id: "sess_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        // 第一次建站：进度回调在 2KB 后置位取消。
        let cancel = AtomicBool::new(false);
        let seen = AtomicUsize::new(0);
        let cancel_ref = &cancel;
        let seen_ref = &seen;
        let complete = crate::sessions::build_transcript_index(
            &storage,
            "zcode",
            "sess_a",
            512,
            cancel_ref,
            &move |done, _| {
                if done > 2048 && seen_ref.fetch_add(1, AtomicOrdering::Relaxed) > 1 {
                    cancel_ref.store(true, AtomicOrdering::Relaxed);
                }
            },
        )
        .unwrap();
        assert!(!complete, "取消后应返回未完整");
        let partial_count = storage.transcript_entry_count(file_id).unwrap();
        assert!(
            partial_count > 0 && partial_count < 120,
            "取消时应有部分进度, got {partial_count}"
        );

        // 第二次建站：不取消，续建到完整。
        let cancel = AtomicBool::new(false);
        let complete = crate::sessions::build_transcript_index(
            &storage,
            "zcode",
            "sess_a",
            512,
            &cancel,
            &|_, _| {},
        )
        .unwrap();
        assert!(complete);
        // 60 条用户提问 + 59 条进历史的回复 + 1 条末轮悬挂回复。
        assert_eq!(storage.transcript_entry_count(file_id).unwrap(), 120);

        // 读回与全量现读一致。
        let adapter = adapter();
        let expected = adapter.read_transcript(&path, "sess_a").unwrap();
        let page = crate::sessions::session_transcript_page(
            &storage,
            "zcode",
            "sess_a",
            Some(-1),
            None,
            500,
        )
        .unwrap();
        let got: Vec<TranscriptEntry> = page.entries.into_iter().map(|p| p.entry).collect();
        assert_eq!(format!("{got:?}"), format!("{expected:?}"));
    }

    /// 建完后再追加（活动会话的常态）：增量补尾而非整表重建，读回仍与全量
    /// 现读一致；建完时物化的末轮响应在续建里先撤后经快照重进历史，不得
    /// 丢失或重复。
    #[test]
    fn transcript_index_resume_after_append() {
        use std::io::Write as _;
        use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

        let all = history_lines(30);
        let initial: Vec<String> = all[..20].to_vec();
        let appended: Vec<String> = all[20..].to_vec();
        let initial_len = (initial.join("\n") + "\n").len() as i64;

        let path = transcript_fixture((initial.join("\n") + "\n").as_bytes());
        let path_str = path.to_string_lossy().into_owned();

        let storage = {
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let db_path = std::env::temp_dir().join(format!(
                "toktol-zcode-append-{}-{}.db",
                std::process::id(),
                SEQ.fetch_add(1, AtomicOrdering::Relaxed)
            ));
            let _ = std::fs::remove_file(&db_path);
            crate::storage::open(std::path::Path::new(&db_path)).unwrap()
        };
        let file_id = storage
            .upsert_scanned_file(&path_str, "zcode", &[], initial_len, 0, 0)
            .unwrap();
        storage
            .get_or_create_session(
                &crate::storage::NewSession {
                    tool: "zcode".into(),
                    external_id: "sess_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let adapter = adapter();
        let cancel = AtomicBool::new(false);
        let complete = crate::sessions::build_transcript_index(
            &storage,
            "zcode",
            "sess_a",
            1024,
            &cancel,
            &|_, _| {},
        )
        .unwrap();
        assert!(complete);
        let expected_initial = adapter.read_transcript(&path, "sess_a").unwrap();
        assert_eq!(
            storage.transcript_entry_count(file_id).unwrap(),
            expected_initial.len() as i64,
            "首次建站的行数与全量现读一致"
        );

        // 活动会话追加 10 轮，再次建站：必须从尾部续建（进度起点越过旧 EOF）。
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            for line in &appended {
                writeln!(f, "{line}").unwrap();
            }
        }
        let cancel = AtomicBool::new(false);
        let first_done = AtomicU64::new(u64::MAX);
        let first_ref = &first_done;
        let complete = crate::sessions::build_transcript_index(
            &storage,
            "zcode",
            "sess_a",
            1024,
            &cancel,
            &|done, _| {
                first_ref.fetch_min(done, AtomicOrdering::Relaxed);
            },
        )
        .unwrap();
        assert!(complete);
        assert!(
            first_done.load(AtomicOrdering::Relaxed) as i64 >= initial_len,
            "续建应从追加尾部开始，而不是从 0 整表重建"
        );

        // 金标准对拍：增量续建后的索引读回 = 对追加后全量文件的现读。
        let expected = adapter.read_transcript(&path, "sess_a").unwrap();
        let mut pages: Vec<crate::sessions::TranscriptPageEntry> = Vec::new();
        loop {
            let after = pages
                .last()
                .map_or(-1, |p: &crate::sessions::TranscriptPageEntry| p.seq);
            let page = crate::sessions::session_transcript_page(
                &storage,
                "zcode",
                "sess_a",
                Some(after),
                None,
                4,
            )
            .unwrap();
            assert!(page.built);
            let done = page.entries.is_empty();
            pages.extend(page.entries);
            if done {
                break;
            }
        }
        let got: Vec<TranscriptEntry> = pages.into_iter().map(|p| p.entry).collect();
        assert_eq!(
            got.len(),
            expected.len(),
            "撤掉的物化响应须经快照或 EOF 物化补回，不得丢失或重复"
        );
        assert_eq!(format!("{got:?}"), format!("{expected:?}"));
    }

    fn transcript_fixture(content: &[u8]) -> std::path::PathBuf {
        // 文件名必须是 `model-io-{sessionId}.jsonl` 形态：转录编排会做登记源
        // 解析，非 rollout 命名会被当作库登记源去 rollout 目录重定位，在测试
        // 环境里必然 SourceGone。放进独占临时目录避免并行用例互踩。
        let dir = std::env::temp_dir().join(format!(
            "toktol-zcode-transcript-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model-io-sess_a.jsonl");
        std::fs::write(&path, content).unwrap();
        path
    }

    /// 真实日志形态的轮次序列：偶数轮全量快照（offset 0），奇数轮纯增量
    /// （offset = 基线长，携带上一轮回复进历史）。全部使用后会话历史与
    /// 现读重建一致，可直接对拍。
    fn history_lines(count: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let mut history: Vec<serde_json::Value> = Vec::new();
        for i in 0..count {
            if i > 0 {
                history.push(serde_json::json!({
                    "role": "assistant",
                    "content": format!("回答 {:03}", i - 1),
                }));
            }
            history.push(serde_json::json!({
                "role": "user",
                "content": format!("问题 {:03} {}", i, "x".repeat(200)),
            }));
            let is_full = i % 2 == 0;
            let offset = if is_full { 0 } else { history.len() - 2 };
            let messages = if is_full {
                history.clone()
            } else {
                history[(history.len() - 2)..].to_vec()
            };
            lines.push(
                serde_json::json!({
                    "type": "model_io",
                    "sessionId": "sess_a",
                    "querySource": "main_turn",
                    "startedAt": format!("2026-09-23T15:{:02}:00.000Z", i % 60),
                    "request": { "messages": messages, "messageOffset": offset },
                    "response": { "modelId": "z-ai/glm", "text": format!("回答 {:03}", i) },
                })
                .to_string(),
            );
        }
        lines
    }

    /// 项目目录来自应用的 db.sqlite（只读查 session 表）：注入临时库验证映射，
    /// 不在库里的会话仍是 None；库打不开不报错、目录为 None。
    #[test]
    fn project_dir_comes_from_app_db_session_table() {
        let db_path =
            std::env::temp_dir().join(format!("toktol-zcode-db-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT);
             INSERT INTO session (id, directory, title) VALUES
                 ('sess_a', 'E:\\proj\\demo', NULL),
                 ('sess_b', NULL, NULL);",
        )
        .unwrap();
        drop(db);

        let line = |id: &str| {
            format!(
                r#"{{"type":"model_io","sessionId":"{id}","startedAt":"2026-09-23T15:22:21.985Z","response":{{"modelId":"m","usage":{{"inputTokens":1,"outputTokens":1}}}}}}"#
            )
        };
        let adapter = ZcodeAdapter {
            db_path: Some(db_path.clone()),
            rollout_dir: None,
            session_meta: OnceLock::new(),
        };
        let facts = |id: &str| match adapter.parse_line(&line(id)) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(
            facts("sess_a").project_dir.as_deref(),
            Some("E:\\proj\\demo")
        );
        assert_eq!(facts("sess_b").project_dir, None, "库里目录为空不给编造");
        assert_eq!(facts("sess_missing").project_dir, None, "不在库里不给编造");

        std::fs::remove_file(&db_path).unwrap();
    }

    /// 应用库读取：session 表出标题/目录事实（被轮转会话的唯一来源），
    /// model_usage 表出请求级用量；全零行跳过、缓存读写分桶正确、指纹稳定。
    #[test]
    fn read_db_source_yields_session_and_usage_facts() {
        let db_path = zcode_db("src");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "INSERT INTO session (id, directory, title, time_created, time_updated) VALUES
                 ('sess_a', 'E:\\p', '官方标题', 1, 2),
                 ('sess_bare', NULL, NULL, 1, 1);
             INSERT INTO model_usage (id, session_id, started_at, duration_ms, model_id,
                                      input_tokens, output_tokens, reasoning_tokens,
                                      cache_creation_input_tokens, cache_read_input_tokens)
             VALUES ('mu_1', 'sess_a', 1000, 5, 'GLM-5.3-Flash', 100, 7, 3, 0, 90),
                    ('mu_zero', 'sess_a', 1500, 1, 'GLM-5.3-Flash', 0, 0, NULL, 0, 0),
                    ('mu_cancelled', 'sess_a', 1600, 0, 'GLM-5.3-Flash', NULL, NULL, NULL, NULL, NULL),
                    ('mu_write', 'sess_gone', 2000, 6, 'kimi-k3', 10, 2, NULL, 4, 0);",
        )
        .unwrap();
        drop(db);

        let adapter = ZcodeAdapter {
            db_path: Some(db_path.clone()),
            rollout_dir: None,
            session_meta: OnceLock::new(),
        };
        let source = adapter
            .read_db_source()
            .unwrap()
            .expect("库存在时必须返回来源");

        let session_facts: Vec<_> = source
            .facts
            .iter()
            .filter(|(key, _)| key.starts_with("session:"))
            .collect();
        assert_eq!(session_facts.len(), 1, "无标题无目录的会话行不入账");
        assert_eq!(session_facts[0].1.session_external_id, "sess_a");
        assert_eq!(session_facts[0].1.title.as_deref(), Some("官方标题"));
        assert_eq!(session_facts[0].1.project_dir.as_deref(), Some("E:\\p"));

        let mut usage: Vec<_> = source
            .facts
            .iter()
            .filter_map(|(key, facts)| {
                facts
                    .usage
                    .as_ref()
                    .map(|u| (key.as_str(), facts.session_external_id.as_str(), u))
            })
            .collect();
        usage.sort_by_key(|(key, ..)| *key);
        let keys: Vec<_> = usage.iter().map(|(key, ..)| *key).collect();
        assert_eq!(
            keys,
            vec!["mu_1", "mu_write"],
            "全零行（含 cancelled）不入账"
        );

        let (_, session, mu_1) = usage[0];
        assert_eq!(session, "sess_a");
        assert_eq!(mu_1.model_raw, "GLM-5.3-Flash");
        assert_eq!(mu_1.ts_ms, 1000);
        assert_eq!(mu_1.duration_ms, Some(5));
        assert_eq!(mu_1.usage.input_tokens, 10, "输入是拆掉缓存读的净量");
        assert_eq!(mu_1.usage.cache_read_tokens, 90);
        assert_eq!(mu_1.usage.output_tokens, 7);
        assert_eq!(mu_1.usage.reasoning_tokens, Some(3));
        assert_eq!(usage[1].2.usage.cache_write_tokens, 4, "缓存写单列入账");

        // 指纹稳定；新增行后必须变化。
        let again = adapter.read_db_source().unwrap().unwrap();
        assert_eq!(again.fingerprint, source.fingerprint);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute(
            "INSERT INTO model_usage (id, session_id, started_at, model_id, input_tokens)
             VALUES ('mu_new', 'sess_a', 3000, 'GLM-5.3-Flash', 1)",
            [],
        )
        .unwrap();
        drop(db);
        let changed = adapter.read_db_source().unwrap().unwrap();
        assert_ne!(changed.fingerprint, source.fingerprint, "新增行要触发重放");

        std::fs::remove_file(&db_path).unwrap();
    }

    /// 转录来源解析：文件登记源原样返回；库登记源按 external_id 重新定位
    /// rollout 文件，已被回收时报 SourceGone。
    #[test]
    fn resolve_transcript_source_locates_rollout_and_reports_gone() {
        let rollout =
            std::env::temp_dir().join(format!("toktol-zcode-resolve-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&rollout);
        std::fs::create_dir_all(&rollout).unwrap();
        let file_source = rollout.join("model-io-sess_a.jsonl");
        std::fs::write(&file_source, b"{}\n").unwrap();

        let adapter = ZcodeAdapter {
            db_path: Some(std::env::temp_dir().join("toktol-zcode-no-such-db-resolve.sqlite")),
            rollout_dir: Some(rollout.clone()),
            session_meta: OnceLock::new(),
        };

        assert_eq!(
            adapter
                .resolve_transcript_source(&file_source, "sess_a")
                .unwrap(),
            file_source,
            "文件登记源存在时原样返回"
        );
        // 文件登记源被回收：同样报 SourceGone（否则建索引路径会抛 DataFile）。
        std::fs::remove_file(&file_source).unwrap();
        assert!(matches!(
            adapter.resolve_transcript_source(&file_source, "sess_a"),
            Err(Error::SourceGone(_))
        ));
        std::fs::write(
            &file_source,
            b"{}
",
        )
        .unwrap();
        let db_source = Path::new("/fake").join("db.sqlite");
        assert_eq!(
            adapter
                .resolve_transcript_source(&db_source, "sess_a")
                .unwrap(),
            file_source,
            "库登记源按 external_id 定位 rollout 文件"
        );
        match adapter.resolve_transcript_source(&db_source, "sess_gone") {
            Err(Error::SourceGone(path)) => {
                assert_eq!(path, rollout.join("model-io-sess_gone.jsonl"));
            }
            other => panic!("期望 SourceGone，实际 {other:?}"),
        }

        std::fs::remove_dir_all(&rollout).unwrap();
    }

    /// 端到端：rollout 文件出会话簿记、应用库出用量；有文件的会话登记源
    /// 保持指向文件（转录锚点不变），无文件的会话由库建行；重放被 dedup 拦住。
    #[test]
    fn db_source_scan_ingests_usage_without_double_count() {
        let rollout =
            std::env::temp_dir().join(format!("toktol-zcode-scan-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&rollout);
        std::fs::create_dir_all(&rollout).unwrap();
        std::fs::write(
            rollout.join("model-io-sess_a.jsonl"),
            // 行以换行收尾：扫描层按完整行切分，缺尾换行的行不会进解析。
            concat!(
                r#"{"type":"model_io","sessionId":"sess_a","startedAt":"2026-09-23T15:22:21.985Z","request":{"messages":[{"role":"user","content":"提问"}]}}"#,
                "\n",
            ),
        )
        .unwrap();

        let db_path = zcode_db("scan");
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "INSERT INTO session (id, directory, title, time_created, time_updated) VALUES
                 ('sess_a', 'E:\\p', '官方标题', 1, 2),
                 ('sess_gone', NULL, '被轮转的会话', 1, 2);
             INSERT INTO model_usage (id, session_id, started_at, duration_ms, model_id,
                                      input_tokens, output_tokens, reasoning_tokens,
                                      cache_creation_input_tokens, cache_read_input_tokens)
             VALUES ('mu_1', 'sess_a', 1000, 5, 'GLM-5.3-Flash', 100, 7, 3, 0, 90),
                    ('mu_2', 'sess_gone', 2000, 6, 'GLM-5.3-Flash', 10, 2, NULL, 4, 0);",
        )
        .unwrap();
        drop(db);

        let store_path =
            std::env::temp_dir().join(format!("toktol-zcode-scan-{}-store.db", std::process::id()));
        let _ = std::fs::remove_file(&store_path);
        let storage = crate::storage::open(&store_path).unwrap();

        let adapter = ZcodeAdapter {
            db_path: Some(db_path.clone()),
            rollout_dir: Some(rollout.clone()),
            session_meta: OnceLock::new(),
        };
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();

        let rows: Vec<(String, String, i64, i64, i64, i64)> = storage
            .conn()
            .prepare(
                "SELECT s.external_id, u.model, u.input_tokens, u.output_tokens,
                        u.cache_read_tokens, u.cache_write_tokens
                 FROM usage_records u JOIN sessions s ON s.id = u.session_id
                 ORDER BY s.external_id, u.ts",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 2, "两条用量都入库");
        assert_eq!(rows[0].0, "sess_a");
        assert_eq!(rows[0].1, "glm-5.3-flash", "归一化折叠大小写");
        assert_eq!((rows[0].2, rows[0].3, rows[0].4), (10, 7, 90));
        assert_eq!(rows[1].0, "sess_gone");
        assert_eq!(rows[1].5, 4, "缓存写单列入账");

        // 有文件的会话登记源指向 rollout 文件（转录锚点），无文件的指向应用库。
        let source_of = |external: &str| -> String {
            storage
                .conn()
                .query_row(
                    "SELECT f.path FROM sessions s JOIN scanned_files f ON f.id = s.source_file_id
                     WHERE s.external_id = ?1",
                    [external],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert!(source_of("sess_a").ends_with("model-io-sess_a.jsonl"));
        assert_eq!(
            Path::new(source_of("sess_gone").as_str()),
            db_path,
            "库登记源指向应用库"
        );

        // 重放：事实全量重跑，dedup 拦住重复入账。
        let mut report = crate::scan::ScanReport::default();
        crate::scan::scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 0, "重放不重复入账");

        // Windows 上打开中的 sqlite 文件删不掉：先放掉连接再清理临时文件。
        drop(storage);
        std::fs::remove_dir_all(&rollout).unwrap();
        std::fs::remove_file(&db_path).unwrap();
        std::fs::remove_file(&store_path).unwrap();
        let _ = std::fs::remove_file(store_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(store_path.with_extension("db-shm"));
    }
}
