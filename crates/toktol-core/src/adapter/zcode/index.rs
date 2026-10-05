//! zcode 转录索引的建站器：把（可达多 GB 的）model_io 日志流式转换成
//! "条目 → 源文件字节区间"的纯元数据索引。详情页读取从全量现读变为按
//! 索引 seek 读片段；消息正文绝不落库（红线），索引只存角色/时间/区间。
//!
//! 条目推导与 [`super`] 的 `read_transcript` 共用同一套 diff 与展开代码
//! （[`SnapshotDiff`] / `derive_entries`），两条管线对"哪些消息要展开"
//! 的判定永远一致——金标准测试在 zcode tests 里对拍两条管线的输出。
//!
//! 增量续建：检查点每 [`CHECKPOINT_BYTES`] 落一次库（条目行 + 建站状态）。
//! 压缩块（delta/tail）的历史是"基线前缀 + 本行"拼出来的，单一字节区间
//! 复原不了 diff 基线，所以状态里持久化的是**回放锚点**——最近一条
//! offset==0 的消息块行（该行起完整历史即其字节）；续建从锚点行重放 diff
//! （不产条目）恢复基线与末轮响应悬挂，再从 `built_offset` 继续。文件只
//! 增不缩即视为前缀有效（活动会话的常态），缩短 / 原地改写 / 无锚点则
//! 整表重建。

use std::io::{BufRead, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::adapter::transcript::{self, TranscriptBlock, TranscriptEntry, TranscriptRole};
use crate::error::{Error, Result};
use crate::storage::{NewTranscriptEntry, Storage, TranscriptIndexState};

use super::{
    SnapshotDelta, SnapshotDiff, TranscriptLine, TranscriptResponse, derive_entries, history_entry,
    line_applies, raw_ts_ms, strip_cache_control,
};

/// 单行解析上限（与扫描层 MAX_STREAM_LINE_BYTES 同口径）：超限行跳过，
/// 索引里不含该行产生的条目（其后的行靠 rewritten 路径兜住语义）。
const MAX_INDEX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// 流式建站：从上次检查点（或文件头）处理到 EOF（或取消点）。
/// 可重入：重复调用直到返回 `true`（`false` = 被取消，已建部分已落库）。
pub(crate) fn build(
    storage: &Storage,
    file_id: i64,
    path: &Path,
    external_id: &str,
    checkpoint_bytes: u64,
    cancel: &AtomicBool,
    progress: &dyn Fn(u64, u64),
) -> Result<bool> {
    let meta = std::fs::metadata(path).map_err(|source| Error::DataFile {
        path: path.to_path_buf(),
        source,
    })?;
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0);

    // 可续建 = 字节区间 [0, built_offset) 的索引仍然有效：文件未缩短、
    // 未原地改写（日志只追加），且有回放锚点。未建完（检查点/取消）与
    // 已建完后再追加（活动会话）都走同一条续建路——整表重建会让边用
    // 边写的会话陷入"建完即失效"的循环。
    let resume = match storage.transcript_index_state(file_id)? {
        Some(state)
            if state.built_offset >= 0
                && size >= state.built_offset
                && state.baseline_offset.is_some()
                && state.baseline_len.is_some()
                && (state.mtime_ms == mtime_ms || size > state.size) =>
        {
            Some(state)
        }
        _ => {
            storage.reset_transcript_index(file_id)?;
            None
        }
    };
    // 下一个 seq = 已提交行数（seq 连续插入；读取侧空块过滤不影响行数）。
    let mut next_seq = if resume.is_some() {
        storage.transcript_entry_count(file_id)?
    } else {
        0
    };
    let append_resume = resume.as_ref().is_some_and(|s| s.complete);
    let mut built_offset = resume.as_ref().map_or(0, |s| s.built_offset.max(0));

    let mut diff = SnapshotDiff::new();
    // 末轮响应悬挂：回放到 built_offset 时恢复。complete 态的它已在建完时
    // 物化成最后一行，续建先撤掉——其内容会经下一条快照或 EOF 重新进
    // 索引，不撤则同一回复出现两次。
    let mut pending: Option<(i64, i64, Option<i64>, Option<String>)> = None;
    let mut root = resume
        .as_ref()
        .and_then(|s| Some((s.baseline_offset?, s.baseline_len?)));

    let mut reader_file = std::fs::File::open(path).map_err(|source| Error::DataFile {
        path: path.to_path_buf(),
        source,
    })?;

    // 回放：锚点行起逐行重算 diff / pending / 锚点，不产条目。size 预检
    // 已保证文件不短于 built_offset，回放不会提前撞 EOF。
    if let Some((root_start, _)) = root {
        reader_file
            .seek(SeekFrom::Start(root_start as u64))
            .map_err(|source| Error::DataFile {
                path: path.to_path_buf(),
                source,
            })?;
        let mut reader = std::io::BufReader::with_capacity(1024 * 1024, &mut reader_file);
        let mut replayed = root_start;
        let mut line_buf = Vec::new();
        while replayed < built_offset {
            line_buf.clear();
            let read =
                reader
                    .read_until(b'\n', &mut line_buf)
                    .map_err(|source| Error::DataFile {
                        path: path.to_path_buf(),
                        source,
                    })?;
            if read == 0 {
                break;
            }
            let line_start = replayed;
            replayed += read as i64;
            if !line_buf.ends_with(b"\n") {
                break; // 尾行不完整：不可能出现在 built_offset 前，防御性停
            }
            while line_buf.last() == Some(&b'\n') || line_buf.last() == Some(&b'\r') {
                line_buf.pop();
            }
            if line_buf.len() <= MAX_INDEX_LINE_BYTES {
                process_line(
                    &line_buf,
                    line_start,
                    external_id,
                    &mut diff,
                    &mut root,
                    &mut pending,
                    false,
                );
            }
        }
        if append_resume && pending.is_some() {
            let last = next_seq - 1;
            storage.delete_transcript_entry(file_id, last)?;
            next_seq = last;
        }
    }
    progress(built_offset as u64, size as u64);

    reader_file
        .seek(SeekFrom::Start(built_offset as u64))
        .map_err(|source| Error::DataFile {
            path: path.to_path_buf(),
            source,
        })?;
    let mut reader = std::io::BufReader::with_capacity(1024 * 1024, reader_file);
    let mut offset = built_offset;
    let mut rows: Vec<NewTranscriptEntry> = Vec::new();

    let mut line_buf = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            commit_index(
                storage,
                file_id,
                &mut rows,
                &mut next_seq,
                &StateSnapshot {
                    built_offset: offset,
                    size,
                    mtime_ms,
                    complete: false,
                    root,
                },
            )?;
            progress(offset as u64, size as u64);
            return Ok(false);
        }
        line_buf.clear();
        let read = reader
            .read_until(b'\n', &mut line_buf)
            .map_err(|source| Error::DataFile {
                path: path.to_path_buf(),
                source,
            })?;
        if read == 0 {
            break; // EOF
        }
        let line_start = offset;
        offset += read as i64;
        // 不完整尾行：游标留在行首，等下次补上（与扫描层同语义）。
        if !line_buf.ends_with(b"\n") {
            offset = line_start;
            break;
        }
        while line_buf.last() == Some(&b'\n') || line_buf.last() == Some(&b'\r') {
            line_buf.pop();
        }

        if line_buf.len() <= MAX_INDEX_LINE_BYTES {
            // seq 由 commit_index 统一赋值（唯一写入口，避免漂移）。
            rows.extend(process_line(
                &line_buf,
                line_start,
                external_id,
                &mut diff,
                &mut root,
                &mut pending,
                true,
            ));
        }

        progress(offset as u64, size as u64);

        if offset - built_offset >= i64::try_from(checkpoint_bytes).unwrap_or(i64::MAX) {
            commit_index(
                storage,
                file_id,
                &mut rows,
                &mut next_seq,
                &StateSnapshot {
                    built_offset: offset,
                    size,
                    mtime_ms,
                    complete: false,
                    root,
                },
            )?;
            rows.clear();
            built_offset = offset;
        }
    }

    // EOF：悬挂的末轮响应落为条目，标记完整。锚点原样留进状态，文件后续
    // 追加时增量续建据此回放。
    if let Some((off, len, ts, model)) = pending.take() {
        rows.push(NewTranscriptEntry {
            seq: 0,
            role: "assistant".into(),
            ts_ms: ts,
            model,
            kind: 2,
            frag_offset: off,
            frag_len: len,
            skip_blocks: None,
            frag_meta: None,
        });
    }
    commit_index(
        storage,
        file_id,
        &mut rows,
        &mut next_seq,
        &StateSnapshot {
            built_offset: offset,
            size,
            mtime_ms,
            complete: true,
            root,
        },
    )?;
    progress(offset as u64, size as u64);
    Ok(true)
}

/// 状态表的瞬时视图（提交前在栈上组装）。
struct StateSnapshot {
    built_offset: i64,
    size: i64,
    mtime_ms: i64,
    complete: bool,
    /// 回放锚点：最近一条 offset==0 消息块行的 (行起点, 行长)。
    root: Option<(i64, i64)>,
}

fn commit_index(
    storage: &Storage,
    file_id: i64,
    rows: &mut Vec<NewTranscriptEntry>,
    next_seq: &mut i64,
    state: &StateSnapshot,
) -> Result<()> {
    for row in rows.iter_mut() {
        row.seq = *next_seq;
        *next_seq += 1;
    }
    let (baseline_offset, baseline_len) = match state.root {
        Some((off, len)) => (Some(off), Some(len)),
        None => (None, None),
    };
    storage.commit_transcript_index(
        file_id,
        rows,
        &TranscriptIndexState {
            built_offset: state.built_offset,
            size: state.size,
            mtime_ms: state.mtime_ms,
            complete: state.complete,
            baseline_offset,
            baseline_len,
            // 末轮悬挂由续建时的锚点回放恢复，不再持久化。
            pending_offset: None,
            pending_len: None,
            pending_ts: None,
            pending_model: None,
        },
    )?;
    rows.clear();
    Ok(())
}

/// 处理一行：返回该行产生的索引行（可能 0~N 条，seq 由调用方补）。
/// 不满足过滤条件/解析失败/字节扫描与 serde 不一致时返回空——后者按整行
/// 跳过（与基线的前缀失配由 rewritten 路径兜住）。`emit = false` 为回放：
/// 只推进 diff / pending / 锚点，不产条目。
#[allow(clippy::too_many_arguments)]
fn process_line(
    line: &[u8],
    line_start: i64,
    external_id: &str,
    diff: &mut SnapshotDiff,
    root: &mut Option<(i64, i64)>,
    pending: &mut Option<(i64, i64, Option<i64>, Option<String>)>,
    emit: bool,
) -> Vec<NewTranscriptEntry> {
    let Ok(raw) = serde_json::from_slice::<TranscriptLine>(line) else {
        return Vec::new();
    };
    if !line_applies(&raw, external_id) {
        return Vec::new();
    }
    let ts_ms = raw_ts_ms(&raw);
    let scan = LineRanges::scan(line);
    let Some(items) = scan.message_items() else {
        return Vec::new();
    };
    let response_range = scan.response_range();

    let mut raw = raw;
    let Some(req) = raw.request.take() else {
        return Vec::new();
    };
    let Some(mut messages) = req.messages else {
        return Vec::new();
    };
    // serde 与字节扫描必须看到同一个数组；不一致视为坏行整行跳过。
    if items.len() != messages.len() {
        return Vec::new();
    }
    let offset = usize::try_from(req.message_offset.unwrap_or(0)).unwrap_or(0);
    messages.iter_mut().for_each(strip_cache_control);

    // 拼出本行的完整历史快照（offset==0 时即本行消息本身）；滑窗会话的
    // offset 是全史绝对起点，换算见 SnapshotDiff::splice_at。
    let splice = diff.splice_at(offset);
    let mut effective = diff.baseline()[..splice].to_vec();
    effective.extend(messages);

    let mut rows = Vec::new();
    // 与转录路径同一套推导（derive_entries），只保留真正会渲染的条目；
    // delta 下标针对拼接后的完整历史，字节区间只在本行消息里，故减去
    // 拼接长度定位本行数组元素。
    for (_entry, delta) in derive_entries(diff, &effective, ts_ms) {
        let (kind, index, skip_items, frag_meta) = match delta {
            SnapshotDelta::Message { index } => (0i64, index, None, None),
            SnapshotDelta::Appended {
                item,
                old,
                skip_items,
            } => {
                // tool 角色的追加块需要旧消息的 tool_call_id，落进行元数据。
                let frag_meta = diff
                    .baseline()
                    .get(old)
                    .and_then(|old_value| old_value.get("tool_call_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                (1i64, item, Some(skip_items as i64), frag_meta)
            }
        };
        let Some(item_index) = index.checked_sub(splice) else {
            continue;
        };
        let Some(item_range) = items.get(item_index) else {
            continue;
        };
        rows.push(NewTranscriptEntry {
            seq: 0,
            role: role_of(&effective[index]),
            ts_ms,
            model: None,
            kind,
            frag_offset: line_start + item_range.start as i64,
            frag_len: (item_range.end - item_range.start) as i64,
            skip_blocks: skip_items,
            frag_meta,
        });
    }
    diff.commit(splice, offset, effective);
    if offset == 0 {
        *root = Some((line_start, line.len() as i64));
    }
    *pending = raw.response.filter(|r| r.has_content()).and_then(|r| {
        let range = response_range?;
        Some((
            line_start + range.start as i64,
            (range.end - range.start) as i64,
            ts_ms,
            r.model_id,
        ))
    });
    if emit { rows } else { Vec::new() }
}

fn role_of(value: &Value) -> String {
    value
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// 从文件任意偏移读取一段字节（读取侧按索引 seek 片段用）。
pub(crate) fn read_range(
    file: &mut std::fs::File,
    path: &Path,
    offset: i64,
    len: i64,
) -> Result<Vec<u8>> {
    if offset < 0 || len < 0 {
        return Err(Error::Internal("transcript frag 区间非法".into()));
    }
    file.seek(SeekFrom::Start(offset as u64))
        .map_err(|source| Error::DataFile {
            path: path.to_path_buf(),
            source,
        })?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|source| Error::DataFile {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(buf)
}

/// 一行 JSON 的结构区间（纯字节走查，不物化）。
pub(crate) struct LineRanges {
    items: Option<Vec<std::ops::Range<usize>>>,
    response: Option<(usize, usize)>,
}

impl LineRanges {
    pub(crate) fn scan(bytes: &[u8]) -> Self {
        let json = ByteJson::new(bytes);
        let root = 0..bytes.len();
        let mut this = Self {
            items: None,
            response: None,
        };
        let Some(request) = json.field(root, "request") else {
            return this;
        };
        // 消息块挂在 request 下（body 里只有请求参数）。
        let Some(messages) = json.field(request, "messages") else {
            return this;
        };
        if json.byte_at(messages.start) == Some(b'[') {
            this.items = Some(json.array_items(messages.clone()));
        }
        if let Some(response) = json.field(0..bytes.len(), "response") {
            this.response = Some((response.start, response.end));
        }
        this
    }

    /// messages 数组的元素区间；数组不存在（旁路行等）为 `None`。
    pub(crate) fn message_items(&self) -> Option<Vec<std::ops::Range<usize>>> {
        self.items.clone()
    }

    pub(crate) fn response_range(&self) -> Option<std::ops::Range<usize>> {
        self.response.map(|(s, e)| s..e)
    }
}

/// 最小 JSON 字节走查：只定位结构与区间，绝不物化内容。
struct ByteJson<'a> {
    b: &'a [u8],
}

impl<'a> ByteJson<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b }
    }

    fn byte_at(&self, i: usize) -> Option<u8> {
        self.b.get(i).copied()
    }

    fn skip_ws(&self, mut i: usize) -> usize {
        while matches!(self.byte_at(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            i += 1;
        }
        i
    }

    /// 字符串字面量的结束（含闭引号）；处理转义。
    fn string_end(&self, open: usize) -> Option<usize> {
        let mut i = open + 1;
        while let Some(byte) = self.byte_at(i) {
            match byte {
                b'\\' => i += 2,
                b'"' => return Some(i + 1),
                _ => i += 1,
            }
        }
        None
    }

    /// 从 start 起跳过一个 JSON 值，返回结束下标（exclusive）。
    fn value_end(&self, start: usize) -> Option<usize> {
        let mut i = self.skip_ws(start);
        match self.byte_at(i)? {
            b'{' | b'[' => {
                let (open, close) = if self.byte_at(i) == Some(b'{') {
                    (b'{', b'}')
                } else {
                    (b'[', b']')
                };
                let mut depth = 0usize;
                while let Some(byte) = self.byte_at(i) {
                    match byte {
                        b'"' => {
                            i = self.string_end(i)?;
                            continue;
                        }
                        b if b == open => depth += 1,
                        b if b == close => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(i + 1);
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                None
            }
            b'"' => self.string_end(i),
            _ => {
                // 数字 / 字面量：读到结构边界。
                while let Some(byte) = self.byte_at(i) {
                    if matches!(byte, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    i += 1;
                }
                Some(i)
            }
        }
    }

    /// 对象（含花括号的区间）里某个顶层键的值区间。
    fn field(&self, obj: std::ops::Range<usize>, key: &str) -> Option<std::ops::Range<usize>> {
        let mut i = self.skip_ws(obj.start + 1);
        loop {
            match self.byte_at(i)? {
                b'}' => return None,
                b'"' => {}
                _ => return None,
            }
            let key_end = self.string_end(i)?;
            let key_name = std::str::from_utf8(&self.b[i + 1..key_end - 1]).ok()?;
            let colon = self.skip_ws(key_end);
            if self.byte_at(colon) != Some(b':') {
                return None;
            }
            let value_start = self.skip_ws(colon + 1);
            let value_end = self.value_end(value_start)?;
            if key_name == key {
                return Some(value_start..value_end);
            }
            i = self.skip_ws(value_end);
            if self.byte_at(i) == Some(b',') {
                i = self.skip_ws(i + 1);
            } else {
                return None;
            }
        }
    }

    /// 数组（含方括号的区间）的元素区间列表。
    fn array_items(&self, arr: std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
        let mut items = Vec::new();
        let mut i = self.skip_ws(arr.start + 1);
        loop {
            match self.byte_at(i) {
                Some(b']') | None => return items,
                _ => {}
            }
            let Some(end) = self.value_end(i) else {
                return items;
            };
            items.push(i..end);
            i = self.skip_ws(end);
            if self.byte_at(i) == Some(b',') {
                i = self.skip_ws(i + 1);
            } else {
                return items;
            }
        }
    }
}

/// 索引片段 → 转录条目（读取侧）。片段语义由建站器定义：
/// 0 = 消息对象（`history_entry` 口径），1 = 追加式消息（跳过旧块，余下
/// 按角色映射），2 = 末轮响应对象。渲染为空的片段返回 `None`。
pub(crate) fn entry_from_frag(
    kind: i64,
    frag: &[u8],
    skip_blocks: Option<i64>,
    role: &str,
    ts_ms: Option<i64>,
    model: Option<&str>,
    frag_meta: Option<&str>,
) -> Result<Option<TranscriptEntry>> {
    let value: Value = serde_json::from_slice(frag)
        .map_err(|err| Error::Internal(format!("transcript frag 解析失败: {err}")))?;
    let entry = match kind {
        0 => history_entry(&value, ts_ms),
        1 => {
            let Some(items) = value.get("content").and_then(|c| c.as_array()) else {
                return Ok(None);
            };
            let skip = skip_blocks.unwrap_or(0).clamp(0, items.len() as i64) as usize;
            let blocks = extra_blocks_for(role, frag_meta, &items[skip..]);
            if blocks.is_empty() {
                None
            } else {
                Some(TranscriptEntry {
                    role: role_enum(role),
                    ts_ms,
                    model: None,
                    blocks,
                })
            }
        }
        2 => {
            let response: TranscriptResponse = serde_json::from_slice(frag)
                .map_err(|err| Error::Internal(format!("transcript resp 解析失败: {err}")))?;
            let blocks = response.blocks();
            if blocks.is_empty() {
                None
            } else {
                Some(TranscriptEntry {
                    role: TranscriptRole::Assistant,
                    ts_ms,
                    model: model.map(str::to_string).filter(|m| !m.trim().is_empty()),
                    blocks,
                })
            }
        }
        _ => None,
    };
    Ok(entry)
}

fn role_enum(role: &str) -> TranscriptRole {
    match role {
        "assistant" => TranscriptRole::Assistant,
        "system" => TranscriptRole::System,
        "tool" => TranscriptRole::Tool,
        _ => TranscriptRole::User,
    }
}

/// 按角色把追加的内容块归一成转录块（与 extra_blocks 的角色分派一致；
/// 读取侧没有旧消息对象，tool 的 call_id 由建站器存进行元数据）。
fn extra_blocks_for<'a, I>(role: &str, call_id: Option<&str>, extra: I) -> Vec<TranscriptBlock>
where
    I: IntoIterator<Item = &'a Value>,
{
    extra
        .into_iter()
        .flat_map(|item| match role {
            "user" => super::user_blocks(item),
            "tool" => vec![TranscriptBlock::ToolResult {
                call_id: call_id.map(str::to_string),
                content: transcript::value_text(item),
                is_error: false,
            }],
            _ => transcript::blocks_from_content(item),
        })
        .collect()
}
