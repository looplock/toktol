//! 会话转录编排：按 (tool, external_id) 定位会话行 → 源文件 → 适配器现读
//! 消息内容。内容只在本次返回值里出现，不落库、不缓存——隐私红线与扫描
//! 管线一致：用户点开才读，读完即弃。

use std::path::Path;

use serde::Serialize;

use crate::adapter::TranscriptEntry;
use crate::error::{Error, Result};
use crate::storage::Storage;

/// 一次会话转录的载荷。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPayload {
    /// 工具 id（原样回传，前端定位品牌图标）。
    pub tool: String,
    /// 工具侧会话 id。
    pub external_id: String,
    /// 按日志顺序排列的消息；空列表 = 日志里没有可展示的消息（如 dsh 只写
    /// 会话头），不是错误。
    pub entries: Vec<TranscriptEntry>,
}

/// 读取一个会话的转录。数据库类来源（opencode）的"源文件"是其自有库，
/// 适配器按红线只 SELECT 消息表；不支持转录的工具（grok）返回
/// `core.unsupported`，前端按码展示说明文案。
pub fn session_transcript(
    storage: &Storage,
    tool: &str,
    external_id: &str,
) -> Result<TranscriptPayload> {
    let (info, adapter, source) = resolve(storage, tool, external_id)?;
    let entries = adapter.read_transcript(Path::new(&source), &info.external_id)?;
    Ok(TranscriptPayload {
        tool: info.tool,
        external_id: info.external_id,
        entries,
    })
}

// ---- 索引转录：分页读取 / 轮次列表 / 建站编排 ----

use std::sync::atomic::AtomicBool;

use crate::adapter::transcript::TranscriptBlock;
use crate::adapter::{Adapter, TranscriptRole};
use crate::storage::TranscriptEntryRow;

/// 一页里的条目：seq 是索引顺序号，前端据此继续翻页与跳轮。serde 不用
/// flatten——前端的类型镜像按嵌套 `{ seq, entry }` 定义，扁平序列化会让
/// `entry` 字段整体 undefined，详情页渲染即崩（SSR 测试手构嵌套数据，
/// 测不出这个错，必须有序列化形状的契约测试钉住）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPageEntry {
    /// 索引顺序号（该会话内从 0 递增）。
    pub seq: i64,
    /// 转录条目本体。
    pub entry: TranscriptEntry,
}

/// 一页转录。`built = false` 表示索引未建（或失效），`entries` 为空——
/// 前端调 `build_transcript_index` 建站（有进度、可取消）后再来取。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptPagePayload {
    /// 工具 id（原样回传）。
    pub tool: String,
    /// 工具侧会话 id（原样回传）。
    pub external_id: String,
    /// 索引是否可用；false 时 entries 为空，前端先触发建站。
    pub built: bool,
    /// 索引条目总数（整个会话）。
    pub total: i64,
    /// 本页条目。
    pub entries: Vec<TranscriptPageEntry>,
}

/// 轮次摘要的一段：正文文本或图片占位——按块序交错，前端把图片段渲染成
/// 内联胶囊（tooltip 里「文件名」原位出现在文本流中）。
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TranscriptTurnPart {
    /// 一段正文文本（已做空白归一）。
    Text {
        /// 文本内容（UTF-8 字符计数的截断段）。
        text: String,
    },
    /// 一张图片占位；filename 为附件展示名。
    Image {
        /// 附件展示名（日志缺失时取路径尾段）。
        filename: String,
    },
}

/// 目录（轮次列表）的一项：每轮询问/压缩摘要。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTurn {
    /// 该轮首条条目的索引顺序号。
    pub seq: i64,
    /// 轮次时间。
    pub ts_ms: Option<i64>,
    /// 轮次摘要：正文文本前 200 字符；摘要行与无正文轮为空串。
    pub snippet: String,
    /// 轮次摘要的结构化分段（文本/图片占位按原位交错）；纯文本版即
    /// `snippet`。摘要行为空串、分段为空。
    pub parts: Vec<TranscriptTurnPart>,
    /// 压缩摘要行（前端用固定文案展示）。
    pub is_summary: bool,
}

/// 目录（轮次列表）的载荷。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTurnsPayload {
    /// 工具 id（原样回传）。
    pub tool: String,
    /// 工具侧会话 id（原样回传）。
    pub external_id: String,
    /// 索引是否可用；false 时 turns 为空，前端先触发建站。
    pub built: bool,
    /// 轮次列表（按转录顺序）。
    pub turns: Vec<TranscriptTurn>,
}

/// 解析 (tool, external_id) → (会话信息, 适配器, 转录真实来源路径)。登记源
/// 可能是工具自有库（zcode 被轮转的会话），适配器据此定位真实转录文件；
/// 已被工具回收时在此处报 `core.source_gone`。
fn resolve(
    storage: &Storage,
    tool: &str,
    external_id: &str,
) -> Result<(crate::storage::SessionInfo, Box<dyn Adapter>, String)> {
    let Some(info) = storage.session_info_by_external(tool, external_id)? else {
        return Err(Error::NotFound(format!("session: {tool}/{external_id}")));
    };
    let t = super::parse_tool(&info.tool)?;
    let adapter = super::find_adapter(t)?;
    let Some(registered) = storage.scanned_file_path(info.source_file_id)? else {
        return Err(Error::NotFound(format!("session {} source file", info.id)));
    };
    let source = adapter
        .resolve_transcript_source(Path::new(&registered), &info.external_id)?
        .to_string_lossy()
        .into_owned();
    Ok((info, adapter, source))
}

/// 现读全量的超限闸门：超过扫描单文件上限且不走索引的工具，直接拒绝，
/// 而不是把进程拖进全量读的泥潭（zcode 走索引，不受此限）。登记源是工具
/// 自有库的（opencode）不设闸：读取走 SELECT，库文件体积与读取成本无关，
/// 闸它会把该工具的全部会话详情错杀成 oversize。
fn gate_read_size(adapter: &dyn Adapter, source: &str) -> Result<()> {
    if adapter
        .db_source_path()
        .is_some_and(|db| db == Path::new(source))
    {
        return Ok(());
    }
    let meta = std::fs::metadata(source).map_err(|err_source| Error::DataFile {
        path: Path::new(source).to_path_buf(),
        source: err_source,
    })?;
    if i64::try_from(meta.len()).unwrap_or(i64::MAX) > crate::scan::MAX_SCAN_FILE_BYTES {
        return Err(Error::TranscriptOversize);
    }
    Ok(())
}

/// 索引是否可用（完整且指纹与当前文件一致）。
fn index_valid(storage: &Storage, file_id: i64, source: &str) -> Result<bool> {
    let Ok(meta) = std::fs::metadata(source) else {
        return Ok(false);
    };
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    Ok(storage
        .transcript_index_state(file_id)?
        .filter(|s| s.complete && s.size == size && s.mtime_ms == mtime_ms)
        .is_some())
}

/// 分页取转录。`after_seq` / `before_seq` 二选一：向后翻页给 `after_seq`
/// （取 seq 更大的前 limit 条），向前翻页给 `before_seq`（取 seq 更小的
/// 后 limit 条，升序返回）；都缺省则从头取。
pub fn session_transcript_page(
    storage: &Storage,
    tool: &str,
    external_id: &str,
    after_seq: Option<i64>,
    before_seq: Option<i64>,
    limit: i64,
) -> Result<TranscriptPagePayload> {
    let (info, adapter, source) = resolve(storage, tool, external_id)?;
    let limit = limit.clamp(1, 500);
    if !adapter.transcript_indexed() {
        // 非索引适配器：现读全量（有闸门），内存里切页。
        gate_read_size(adapter.as_ref(), &source)?;
        let all = adapter.read_transcript(Path::new(&source), &info.external_id)?;
        let total = all.len() as i64;
        let window: Vec<(usize, TranscriptEntry)> = if let Some(before) = before_seq {
            let end = (before.max(0) as usize).min(all.len());
            let start = end.saturating_sub(limit as usize);
            all.into_iter()
                .enumerate()
                .skip(start)
                .take(end - start)
                .collect()
        } else {
            let start = after_seq.map_or(0, |a| (a + 1).max(0) as usize);
            all.into_iter()
                .enumerate()
                .skip(start)
                .take(limit as usize)
                .collect()
        };
        let entries = window
            .into_iter()
            .map(|(seq, entry)| TranscriptPageEntry {
                seq: seq as i64,
                entry,
            })
            .collect();
        return Ok(TranscriptPagePayload {
            tool: info.tool,
            external_id: info.external_id,
            built: true,
            total,
            entries,
        });
    }

    // 索引路径：不可用（未建/失效）就交还给前端触发建站。
    if !index_valid(storage, info.source_file_id, &source)? {
        return Ok(TranscriptPagePayload {
            tool: info.tool,
            external_id: info.external_id,
            built: false,
            total: 0,
            entries: Vec::new(),
        });
    }
    let rows: Vec<crate::storage::TranscriptEntryRow> = if let Some(before) = before_seq {
        storage.transcript_entries_before(info.source_file_id, before, limit)?
    } else {
        storage.transcript_entries_page(info.source_file_id, after_seq.unwrap_or(-1), limit)?
    };
    let total = storage.transcript_entry_count(info.source_file_id)?;
    let mut file =
        std::fs::File::open(Path::new(&source)).map_err(|err_source| Error::DataFile {
            path: Path::new(&source).to_path_buf(),
            source: err_source,
        })?;
    let entries = rows_to_entries(&mut file, Path::new(&source), &rows, adapter.as_ref())?;
    Ok(TranscriptPagePayload {
        tool: info.tool,
        external_id: info.external_id,
        built: true,
        total,
        entries,
    })
}

/// 索引行 → 转录条目：按 (偏移, 长度) seek 读片段，交适配器解析。
fn rows_to_entries(
    file: &mut std::fs::File,
    path: &Path,
    rows: &[TranscriptEntryRow],
    adapter: &dyn Adapter,
) -> Result<Vec<TranscriptPageEntry>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let frag =
            crate::adapter::zcode::index::read_range(file, path, row.frag_offset, row.frag_len)?;
        if let Some(entry) = adapter.transcript_entry_from_frag(
            row.kind,
            &frag,
            row.skip_blocks,
            &row.role,
            row.ts_ms,
            row.model.as_deref(),
            row.frag_meta.as_deref(),
        )? {
            out.push(TranscriptPageEntry {
                seq: row.seq,
                entry,
            });
        }
    }
    Ok(out)
}

/// 目录（轮次列表）。语义与前端原 `outlineItems` 一致：压缩摘要行 +
/// 带正文的用户询问轮 + （首条非询问但有条目价值时的兜底）。
pub fn session_transcript_turns(
    storage: &Storage,
    tool: &str,
    external_id: &str,
) -> Result<TranscriptTurnsPayload> {
    let (info, adapter, source) = resolve(storage, tool, external_id)?;
    let make_payload = |built: bool, turns: Vec<TranscriptTurn>| TranscriptTurnsPayload {
        tool: info.tool.clone(),
        external_id: info.external_id.clone(),
        built,
        turns,
    };
    if !adapter.transcript_indexed() {
        gate_read_size(adapter.as_ref(), &source)?;
        let all = adapter.read_transcript(Path::new(&source), &info.external_id)?;
        let mut turns = Vec::new();
        for (index, entry) in all.iter().enumerate() {
            let first_fallback = turns.is_empty();
            turn_from_entry(&mut turns, index as i64, entry, first_fallback);
        }
        return Ok(make_payload(true, turns));
    }
    if !index_valid(storage, info.source_file_id, &source)? {
        return Ok(make_payload(false, Vec::new()));
    }
    let file_id = info.source_file_id;
    let path = Path::new(&source);
    let mut file = std::fs::File::open(path).map_err(|err_source| Error::DataFile {
        path: path.to_path_buf(),
        source: err_source,
    })?;
    let mut turns = Vec::new();
    // 首条兜底：首条索引行若非用户询问，但仍可能值得进目录（旧 outline
    // 的 items.length === 0 规则）。
    if let Some(first) = storage
        .transcript_entries_page(file_id, -1, 1)?
        .first()
        .cloned()
        && let Some(entry) = frag_entry(adapter.as_ref(), &mut file, path, &first)?
    {
        // 只贴图不打字的用户消息同样开轮（与 turn_from_entry 同口径）。
        let starts_turn = entry.role == TranscriptRole::User
            && entry.blocks.iter().any(|b| {
                matches!(
                    b,
                    TranscriptBlock::Text { .. } | TranscriptBlock::Image { .. }
                )
            });
        if !starts_turn {
            turn_from_entry(&mut turns, first.seq, &entry, true);
        }
    }
    for row in storage.transcript_turn_candidates(file_id)? {
        if let Some(entry) = frag_entry(adapter.as_ref(), &mut file, path, &row)? {
            turn_from_entry(&mut turns, row.seq, &entry, false);
        }
    }
    Ok(make_payload(true, turns))
}

/// 一条条目是否进目录、以什么样子进（与前端 outlineItems / entrySnippet 同口径）。
fn turn_from_entry(
    turns: &mut Vec<TranscriptTurn>,
    seq: i64,
    entry: &TranscriptEntry,
    first_fallback: bool,
) {
    let is_summary = entry
        .blocks
        .iter()
        .any(|b| matches!(b, TranscriptBlock::HistorySummary { .. }));
    // 只贴图不打字的用户消息（opencode 的 file part 不带文字）同样是一轮；
    // 工具结果行（role=user 无 text/image）不受影响，仍归入上一轮。
    let starts_turn = entry.role == TranscriptRole::User
        && entry.blocks.iter().any(|b| {
            matches!(
                b,
                TranscriptBlock::Text { .. } | TranscriptBlock::Image { .. }
            )
        });
    let outline_worthy = entry.blocks.iter().any(|b| {
        !matches!(
            b,
            TranscriptBlock::Injected { .. }
                | TranscriptBlock::TaskNotification { .. }
                | TranscriptBlock::HistorySummary { .. }
                | TranscriptBlock::HarnessContext { .. }
                | TranscriptBlock::TurnAborted
        )
    });
    if is_summary {
        turns.push(TranscriptTurn {
            seq,
            ts_ms: entry.ts_ms,
            snippet: String::new(),
            parts: Vec::new(),
            is_summary: true,
        });
        return;
    }
    if !(starts_turn || (first_fallback && outline_worthy)) {
        return;
    }
    // 按块序构建结构化分段：文本段做空白归一并共享 200 字符预算（超出即
    // 截断，后续文本段丢弃）；图片段不占预算、原位插入（块序已经过
    // interleave_image_mentions 按 @image#N 提及归位）。
    const SNIPPET_BUDGET: usize = 200;
    let mut parts: Vec<TranscriptTurnPart> = Vec::new();
    let mut budget = SNIPPET_BUDGET;
    for block in &entry.blocks {
        match block {
            TranscriptBlock::Text { text } => {
                if budget == 0 {
                    continue;
                }
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.is_empty() {
                    continue;
                }
                let taken: String = text.chars().take(budget).collect();
                budget -= taken.chars().count();
                match parts.last_mut() {
                    Some(TranscriptTurnPart::Text { text: prev }) => {
                        prev.push(' ');
                        prev.push_str(&taken);
                    }
                    _ => parts.push(TranscriptTurnPart::Text { text: taken }),
                }
            }
            TranscriptBlock::Image { filename, .. } => {
                parts.push(TranscriptTurnPart::Image {
                    filename: filename.clone(),
                });
            }
            _ => {}
        }
    }
    let snippet: String = parts
        .iter()
        .filter_map(|p| match p {
            TranscriptTurnPart::Text { text } => Some(text.as_str()),
            TranscriptTurnPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    turns.push(TranscriptTurn {
        seq,
        ts_ms: entry.ts_ms,
        snippet,
        parts,
        is_summary: false,
    });
}

fn frag_entry(
    adapter: &dyn Adapter,
    file: &mut std::fs::File,
    path: &Path,
    row: &TranscriptEntryRow,
) -> Result<Option<TranscriptEntry>> {
    let frag = crate::adapter::zcode::index::read_range(file, path, row.frag_offset, row.frag_len)?;
    adapter.transcript_entry_from_frag(
        row.kind,
        &frag,
        row.skip_blocks,
        &row.role,
        row.ts_ms,
        row.model.as_deref(),
        row.frag_meta.as_deref(),
    )
}

/// 为一个会话的源文件构建转录索引（低优先级由壳线程负责、可取消、检查点
/// 续建）。返回是否建完整；非索引适配器无事可做，直接返回 true。
pub fn build_transcript_index(
    storage: &Storage,
    tool: &str,
    external_id: &str,
    checkpoint_bytes: u64,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64, u64),
) -> Result<bool> {
    let (info, adapter, source) = resolve(storage, tool, external_id)?;
    if !adapter.transcript_indexed() {
        return Ok(true);
    }
    adapter.build_transcript_index(
        storage,
        info.source_file_id,
        Path::new(&source),
        &info.external_id,
        checkpoint_bytes,
        cancel,
        progress,
    )
}

/// 建站检查点间隔（壳层透传给建站器）。
pub const TRANSCRIPT_CHECKPOINT_BYTES: u64 = 4 * 1024 * 1024;

#[cfg(test)]
mod tests {

    use super::*;
    use crate::storage::{NewSession, NewUsageRecord};

    fn test_db(tag: &str) -> Storage {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toktol-transcript-db-{}-{}-{tag}.db",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_file(&path);
        crate::storage::open(&path).expect("打开测试库")
    }

    fn create_session(storage: &Storage, tool: &str, external_id: &str, source: &str) -> i64 {
        let file_id = storage
            .upsert_scanned_file(source, tool, &[0xAB], 3, 3, 0)
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: tool.into(),
                    external_id: external_id.into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();
        session_id
    }

    /// 序列化形状契约：page entry 必须是嵌套 `{ seq, entry: {...} }`——
    /// 前端类型镜像按嵌套定义，曾经 flatten 序列化导致线上详情页全白屏
    /// （SSR 测试手构嵌套数据测不出，只有钉住真实序列化形状才防得住）。
    #[test]
    fn transcript_page_entry_serializes_nested() {
        let page = TranscriptPageEntry {
            seq: 7,
            entry: crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: Some(1_789_200_645_293),
                model: None,
                blocks: vec![crate::adapter::transcript::TranscriptBlock::Text {
                    text: "问".into(),
                }],
            },
        };
        let json: serde_json::Value = serde_json::to_value(&page).unwrap();
        assert_eq!(json["seq"], 7);
        assert_eq!(json["entry"]["role"], "user");
        assert_eq!(json["entry"]["blocks"][0]["kind"], "text");
        assert!(json.get("role").is_none(), "条目字段不得扁平进顶层");
    }

    /// 只贴图不打字的用户消息（opencode 的 file part 不带文字）同样开轮、
    /// 进目录——曾经只认 text 块，导致这类消息平铺到左侧、目录漏收。
    #[test]
    fn image_only_user_entry_starts_turn() {
        let mut turns = Vec::new();
        turn_from_entry(
            &mut turns,
            7,
            &crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: Some(1_789_200_645_293),
                model: None,
                blocks: vec![crate::adapter::transcript::TranscriptBlock::Image {
                    path: String::new(),
                    filename: "image".into(),
                    size: Some(94_532),
                    exists: true,
                    data_url: None,
                }],
            },
            false,
        );
        assert_eq!(turns.len(), 1, "纯图用户消息必须开轮");
        assert_eq!(turns[0].seq, 7);
        assert!(!turns[0].is_summary);

        // 工具结果形态（role=user，既无 text 也无 image）不开轮。
        let mut turns = Vec::new();
        turn_from_entry(
            &mut turns,
            8,
            &crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: None,
                model: None,
                blocks: vec![crate::adapter::transcript::TranscriptBlock::ToolResult {
                    call_id: None,
                    content: "ok".into(),
                    is_error: false,
                }],
            },
            false,
        );
        assert!(turns.is_empty(), "工具结果行不得开轮");
    }

    /// 轮次摘要分段：文本/图片按块序原位交错（前端胶囊按此渲染），
    /// 文本段共享 200 字符预算，纯文本版同时落进 snippet。
    #[test]
    fn turn_parts_interleave_text_and_images() {
        let img = |name: &str| crate::adapter::transcript::TranscriptBlock::Image {
            path: String::new(),
            filename: name.into(),
            size: None,
            exists: true,
            data_url: None,
        };
        let mut turns = Vec::new();
        turn_from_entry(
            &mut turns,
            3,
            &crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: None,
                model: None,
                blocks: vec![
                    crate::adapter::transcript::TranscriptBlock::Text {
                        text: "看这张图".into(),
                    },
                    img("image1.png"),
                    crate::adapter::transcript::TranscriptBlock::Text {
                        text: "再看这张".into(),
                    },
                    img("image2.png"),
                ],
            },
            false,
        );
        assert_eq!(turns.len(), 1);
        let parts = &turns[0].parts;
        assert_eq!(
            format!("{parts:?}"),
            "[Text { text: \"看这张图\" }, Image { filename: \"image1.png\" }, \
             Text { text: \"再看这张\" }, Image { filename: \"image2.png\" }]"
        );
        assert_eq!(turns[0].snippet, "看这张图 再看这张");

        // 序列化形状：tag = kind，图片段带 filename（前端胶囊据此渲染）。
        let json: serde_json::Value = serde_json::to_value(&turns[0]).unwrap();
        assert_eq!(json["parts"][0]["kind"], "text");
        assert_eq!(json["parts"][1]["kind"], "image");
        assert_eq!(json["parts"][1]["filename"], "image1.png");

        // 纯图轮：snippet 为空，分段仍有图片胶囊。
        let mut turns = Vec::new();
        turn_from_entry(
            &mut turns,
            4,
            &crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: None,
                model: None,
                blocks: vec![img("shot.png")],
            },
            false,
        );
        assert_eq!(turns[0].snippet, "");
        assert_eq!(turns[0].parts.len(), 1);
    }

    /// 工具注入的会话上下文（codex 的 environment_context 等包裹消息）不开
    /// 轮、不进目录——连首条兜底也不进。
    #[test]
    fn harness_context_entry_does_not_start_turn() {
        let mut turns = Vec::new();
        turn_from_entry(
            &mut turns,
            3,
            &crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: Some(1),
                model: None,
                blocks: vec![
                    crate::adapter::transcript::TranscriptBlock::HarnessContext {
                        tag: "environment_context".into(),
                        text: "<environment_context>…</environment_context>".into(),
                    },
                ],
            },
            true,
        );
        assert!(turns.is_empty(), "注入上下文不开轮、不进目录（含首条兜底）");
    }

    /// 用户中断标记（turn_aborted）是事件不是内容：不开轮、不进目录。
    #[test]
    fn turn_aborted_entry_does_not_start_turn() {
        let mut turns = Vec::new();
        turn_from_entry(
            &mut turns,
            4,
            &crate::adapter::TranscriptEntry {
                role: crate::adapter::TranscriptRole::User,
                ts_ms: Some(1),
                model: None,
                blocks: vec![crate::adapter::transcript::TranscriptBlock::TurnAborted],
            },
            true,
        );
        assert!(turns.is_empty(), "中断标记不开轮、不进目录（含首条兜底）");
    }

    #[test]
    fn transcript_reads_claude_session_file() {
        let dir = std::env::temp_dir().join(format!("toktol-transcript-cl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.jsonl");
        std::fs::write(
            &log,
            [
                r#"{"type":"user","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":"问题"}}"#,
                r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-01-01T00:00:05Z","message":{"model":"claude-sonnet-4-5","content":[{"type":"text","text":"回答"}]}}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let storage = test_db("claude");
        create_session(
            &storage,
            "claude-code",
            "s1",
            log.to_string_lossy().as_ref(),
        );

        let payload = session_transcript(&storage, "claude-code", "s1").unwrap();
        assert_eq!(payload.tool, "claude-code");
        assert_eq!(payload.external_id, "s1");
        assert_eq!(payload.entries.len(), 2);
        assert_eq!(
            payload.entries[1].model.as_deref(),
            Some("claude-sonnet-4-5")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unknown_session_and_missing_file_and_unsupported_tool() {
        let dir = std::env::temp_dir().join(format!("toktol-transcript-er-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let storage = test_db("errors");

        // 会话不存在：core.not_found（会话已删/未入库，非程序 bug）。
        let err = session_transcript(&storage, "claude-code", "nope").unwrap_err();
        assert_eq!(err.code().as_str(), "core.not_found");

        // 不支持转录的工具（codebuddy，消息正文只在云端）：core.unsupported。
        let log = dir.join("codebuddy-sessions.vscdb");
        std::fs::write(&log, "").unwrap();
        create_session(&storage, "codebuddy", "s1", log.to_string_lossy().as_ref());
        let err = session_transcript(&storage, "codebuddy", "s1").unwrap_err();
        assert_eq!(err.code().as_str(), "core.unsupported");

        // 源文件已被删除（如回收站清空）：core.data_file。
        let gone = dir.join("gone.jsonl");
        create_session(
            &storage,
            "claude-code",
            "s2",
            gone.to_string_lossy().as_ref(),
        );
        let err = session_transcript(&storage, "claude-code", "s2").unwrap_err();
        assert_eq!(err.code().as_str(), "core.data_file");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 用量记录与会话共存时互不干扰：转录只读源文件，不碰 usage_records。
    #[test]
    fn usage_records_are_untouched_by_transcript_reads() {
        let dir = std::env::temp_dir().join(format!("toktol-transcript-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("s.jsonl");
        std::fs::write(
            &log,
            r#"{"type":"user","sessionId":"s1","message":{"role":"user","content":"问"}}"#,
        )
        .unwrap();
        let storage = test_db("usage");
        let session_id = create_session(
            &storage,
            "claude-code",
            "s1",
            log.to_string_lossy().as_ref(),
        );
        storage
            .insert_usage_record(&NewUsageRecord {
                session_id: Some(session_id),
                tool: "claude-code".into(),
                model_raw: "m".into(),
                model: "m".into(),
                ts: 1,
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                duration_ms: None,
                request_count: 1,
                dedup_key: vec![1],
            })
            .unwrap();

        session_transcript(&storage, "claude-code", "s1").unwrap();
        let count: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1, "转录读取是无副作用操作");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
