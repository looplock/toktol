//! 增量扫描管线：遍历适配器报告的目录，按游标只解析新增部分，交给 [`crate::storage`]
//! 落库。幂等性由两层保证：文件级游标（`parsed_bytes`）与记录级 dedup_key；
//! 文件被轮转/重写（变小）时游标归零整体重解析，重复入账由 dedup_key 拦住。
//! 超过单文件读取上限的明文日志走流式增量解析（行级自洽的适配器，见
//! [`Adapter::supports_streaming_scan`]）。数据库类来源（[`Adapter::read_db_source`]
//! 与解压文件）没有增量游标：每轮全量读取，指纹未变整体跳过，重复入账同样由
//! dedup_key 拦住。

pub mod scheduler;

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::adapter::{Adapter, DbSource, LineFacts, LineParse};
use crate::error::{Error, Result};
use crate::model::Tool;
use crate::storage::{NewSession, NewUsageRecord, Storage};

/// 一次扫描的汇总，给前端展示与冒烟断言用。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    /// 有新增内容并完成解析的文件数（未变化的文件不计入）。
    pub files_scanned: u32,
    /// 本轮发现消失的文件数。
    pub files_missing: u32,
    /// 超过单文件读取上限被跳过的文件数（见 [`MAX_SCAN_FILE_BYTES`]）。
    pub files_skipped: u32,
    /// 本次新建的会话行数。
    pub sessions_created: u32,
    /// 本次新入账的用量记录数（去重后）。
    pub records_inserted: u32,
    /// 解析失败被跳过的行数（坏行不中止文件）。
    pub parse_errors: u32,
}

/// 单文件缓冲读取上限（256 MB）：`scan_file` 的缓冲路径是整文件读进内存
/// （算指纹 + 切行）。超过上限的文件走流式增量解析（行级自洽的适配器），
/// 其余整体跳过、不登记簿记（对账标记 missing 不受影响——`seen` 在调用方
/// 登记）；将来做流式解析时改这里。
pub(crate) const MAX_SCAN_FILE_BYTES: i64 = 256 * 1024 * 1024;

/// 流式解析的游标落盘间隔（32 MB）：中断后最多重扫这么多字节。
const STREAM_CHECKPOINT_BYTES: u64 = 32 * 1024 * 1024;

/// 流式单行缓冲上限（64 MB）。zcode 的历史快照行会无界膨胀（实测单行 2 GB+
/// ——工具结果把大文件整个读进上下文后，每行都携带全量历史），整行缓冲会把
/// 进程打进内存交换、磁盘狂转、扫描假死。超限行整行放弃（损失该请求的一条
/// 用量记录），游标照常越过，文件其余正常行照常入账。
const MAX_STREAM_LINE_BYTES: usize = 64 * 1024 * 1024;

/// 全量跑一遍启用的适配器并重算成本——新入库的行还没有任何
/// 成本，统一交给重算管线，计价只有一个入口。
/// `disabled` 是被禁用工具的 id 列表（前端偏好，见 LS_KEY_TOOLS_DISABLED）：
/// 禁用的工具**跳过扫描**，已入库数据保留在库中，过滤发生在查询层。
pub fn run_scan(storage: &Storage, disabled: &[String]) -> Result<ScanReport> {
    let mut report = ScanReport::default();
    for adapter in crate::adapter::adapters() {
        if disabled.iter().any(|id| id == adapter.tool().as_str()) {
            continue;
        }
        scan_adapter(storage, adapter.as_ref(), &mut report)?;
    }
    // 遗留自映射壳的映射自愈（幂等）：老版本建档的 raw → 自己映射借扫描修掉。
    storage.normalize_auto_mappings()?;
    storage.recompute_costs()?;
    Ok(report)
}

/// 首扫量级提示的输入：流式适配器（超限文件会逐行解析的工具）尚未扫入的
/// 字节总量。多 GB 日志的首扫以分钟计，前端在扫描气泡里拿它提示"为什么
/// 这轮特别久"。按日志目录现场 stat 汇总，未登记过的新文件也计入——全新
/// 安装的首扫最需要提示，而簿记表里还查不到它们。只做量级提示，不追求
/// 精确：扫描进行中的推进、非流式工具（整体读取，无此量级）都不在此列。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanBacklog {
    /// 有剩余待扫字节的文件数。
    pub files: u32,
    /// 剩余待扫字节总数。
    pub bytes: u64,
}

/// 汇总流式适配器尚未扫入的字节量，供壳层转成首扫量级提示（见 [`ScanBacklog`]）。
/// `disabled` 与 [`run_scan`] 同口径：禁用的工具不会扫，也就不该提示积压。
pub fn scan_backlog(storage: &Storage, disabled: &[String]) -> Result<ScanBacklog> {
    let adapters = crate::adapter::adapters();
    let streaming: Vec<&dyn Adapter> = adapters
        .iter()
        .filter(|a| {
            a.supports_streaming_scan() && !disabled.iter().any(|id| id == a.tool().as_str())
        })
        .map(|a| a.as_ref())
        .collect();
    scan_backlog_of(storage, &streaming)
}

/// [`scan_backlog`] 的可注入内核：适配器列表参数化，测试才不必碰真实家目录。
fn scan_backlog_of(storage: &Storage, adapters: &[&dyn Adapter]) -> Result<ScanBacklog> {
    let mut backlog = ScanBacklog::default();
    for adapter in adapters {
        for dir in adapter.log_dirs() {
            if !dir.is_dir() {
                continue;
            }
            for entry in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() || !adapter.is_session_log(entry.path()) {
                    continue;
                }
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let path = entry.path().to_string_lossy().into_owned();
                let parsed = storage
                    .scanned_file_state(&path)?
                    .map_or(0, |s| s.parsed_bytes);
                let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
                if size > parsed {
                    backlog.files += 1;
                    backlog.bytes += u64::try_from(size - parsed).unwrap_or(0);
                }
            }
        }
    }
    Ok(backlog)
}

pub(crate) fn scan_adapter(
    storage: &Storage,
    adapter: &dyn Adapter,
    report: &mut ScanReport,
) -> Result<()> {
    let tool = adapter.tool();
    // 标题首见为准：同一会话在多个文件（主 + subagent）里出现时，先扫到的算数。
    let mut titled: HashSet<String> = HashSet::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in adapter.log_dirs() {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if !entry.file_type().is_file() || !adapter.is_session_log(path) {
                continue;
            }
            seen.insert(path.to_string_lossy().into_owned());
            scan_file(storage, adapter, tool, path, &mut titled, report)?;
        }
    }

    // 数据库类来源：库路径登记进 seen，与文件走同一套 missing 对账。
    if let Some(source) = adapter.read_db_source()? {
        seen.insert(source.path.to_string_lossy().into_owned());
        scan_db_source(storage, adapter, tool, source, &mut titled, report)?;
    }

    // 对账：walkdir 看不到已删除的文件，消失检测靠"本轮没见到的已知文件"。
    for (id, path) in storage.scanned_file_paths(tool.as_str())? {
        if !seen.contains(&path) {
            storage.set_file_missing(id, Some(now_ms()))?;
            report.files_missing += 1;
        }
    }
    Ok(())
}

fn scan_file(
    storage: &Storage,
    adapter: &dyn Adapter,
    tool: Tool,
    path: &Path,
    titled: &mut HashSet<String>,
    report: &mut ScanReport,
) -> Result<()> {
    scan_file_with_limit(
        storage,
        adapter,
        tool,
        path,
        titled,
        report,
        MAX_SCAN_FILE_BYTES,
    )
}

/// `limit` 参数化只为测试：用小上限验证超限流式路径，不必真写几百 MB 的文件。
fn scan_file_with_limit(
    storage: &Storage,
    adapter: &dyn Adapter,
    tool: Tool,
    path: &Path,
    titled: &mut HashSet<String>,
    report: &mut ScanReport,
    limit: i64,
) -> Result<()> {
    // 入库的"规范化路径"就是 walkdir 给出的绝对路径（根来自 home_dir，全程稳定）。
    let path_str = path.to_string_lossy().into_owned();
    let now = now_ms();

    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        // 用户自行删日志是真实历史：只标 missing，数据保留。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(state) = storage.scanned_file_state(&path_str)? {
                storage.set_file_missing(state.id, Some(now))?;
                report.files_missing += 1;
            }
            return Ok(());
        }
        Err(source) => {
            return Err(Error::DataFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);
    let oversize = size > limit;
    if oversize && !adapter.supports_streaming_scan() {
        // 超限且不能流式：不读、不登记簿记。seen 已在调用方登记，不会误标 missing。
        report.files_skipped += 1;
        return Ok(());
    }

    let state = storage.scanned_file_state(&path_str)?;
    let mut cursor = state.as_ref().map_or(0, |s| s.parsed_bytes);
    if size < cursor {
        // 文件变小 = 被轮转/重写，游标归零整体重解析；重复入账由 dedup_key 拦。
        cursor = 0;
    }

    // 先登记拿 file_id（会话外键需要），本次解析推进的游标在末尾再写回。
    let file_id = storage.upsert_scanned_file(
        &path_str,
        tool.as_str(),
        state.as_ref().map_or(&[], |s| &s.content_hash),
        size,
        cursor,
        now,
    )?;

    if size == cursor {
        return Ok(()); // 无新增；upsert 已顺带清掉 missing 标记。
    }

    if oversize {
        return scan_streaming_tail(
            storage,
            adapter,
            tool,
            path,
            &path_str,
            titled,
            report,
            file_id,
            size,
            cursor,
            now,
            MAX_STREAM_LINE_BYTES,
        );
    }

    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|source| Error::DataFile {
            path: path.to_path_buf(),
            source,
        })?;
    let content_hash = Sha256::digest(&bytes).to_vec();

    // 二级跳过：size==cursor 只能说明"长度没变"，指纹比对才能拦住同长重写——
    // 对压缩文件尤其重要，它们没有增量游标，重写检测完全依赖指纹。
    if state
        .as_ref()
        .is_some_and(|s| s.parsed_bytes == size && s.content_hash == content_hash)
    {
        return Ok(());
    }

    let Some(decoded) = adapter.decode(&bytes) else {
        // 解压失败（坏帧）：不推进游标，下次扫描重试。
        report.parse_errors += 1;
        return Ok(());
    };
    // 解压发生（Owned）说明没有安全的增量游标：整文件重解析，起点归零，
    // 簿记游标写压缩后的文件大小；明文文件维持增量游标语义。
    let whole_file = matches!(decoded, Cow::Owned(_));

    // 全量切行：游标前的前缀供需要跨行上下文的适配器（parse_file）重建状态，
    // 不重复产出事实；不完整尾行不切进来，游标停在它之前留给下次。非 UTF8 行
    // 以空串占位，适配器会按坏行计数，口径与逐行喂入时一致。
    let mut lines: Vec<&str> = Vec::new();
    let mut line_starts: Vec<usize> = Vec::new();
    let mut parsed_to = 0usize;
    for chunk in decoded.split_inclusive(|b| *b == b'\n') {
        if !chunk.ends_with(b"\n") {
            break;
        }
        line_starts.push(parsed_to);
        parsed_to += chunk.len();
        lines.push(match std::str::from_utf8(chunk) {
            Ok(text) => text.trim_end_matches(['\r', '\n']),
            Err(_) => "",
        });
    }
    let start = if whole_file {
        0
    } else {
        line_starts.partition_point(|&at| at < cursor.max(0) as usize)
    };

    // ctx 在整个解析期间借用 report，坏行计数先走本地变量，循环后并回。
    let mut malformed = 0u32;
    let mut ctx = FactContext {
        storage,
        adapter,
        tool,
        file_id,
        path_str: &path_str,
        titled,
        report,
    };
    for (text, parsed) in lines[start..]
        .iter()
        .zip(adapter.parse_file_at(path, &lines, start))
    {
        match parsed {
            LineParse::Skip => {}
            LineParse::Malformed => malformed += 1,
            LineParse::Facts(facts) => apply_facts(&mut ctx, text, *facts)?,
            LineParse::Split(facts) => {
                for fact in facts {
                    apply_facts(&mut ctx, text, fact)?;
                }
            }
        }
    }
    report.parse_errors += malformed;

    storage.upsert_scanned_file(
        &path_str,
        tool.as_str(),
        &content_hash,
        size,
        if whole_file {
            size
        } else {
            i64::try_from(parsed_to).unwrap_or(i64::MAX)
        },
        now,
    )?;
    report.files_scanned += 1;
    Ok(())
}

/// 超限文件的流式增量解析：从游标起只读尾部，逐行喂行级适配器，内存有界
/// （行缓冲上限 [`MAX_STREAM_LINE_BYTES`]，超限行整行放弃）。不做整文件指纹
/// ——`content_hash` 存本轮已处理行的哈希，仅作簿记；同长重写检测在超限
/// 文件上放弃（变小仍靠 size < cursor 兜底）。前提：适配器行级自洽
/// （[`Adapter::supports_streaming_scan`]）且日志是明文（不经 [`Adapter::decode`]）。
///
/// 游标按 [`STREAM_CHECKPOINT_BYTES`] 周期落盘：多 GB 文件的首扫以分钟计，
/// 游标只在文件末尾写回的话，中途退出（关窗、崩溃）会让下次从 0 重来——
/// 大文件在"看起来卡死 → 关应用 → 重扫归零"之间无限打转。落盘哈希是
/// "本轮已处理行"的运行哈希，仅簿记，与最终值同源。
///
/// `max_line` 参数化只为测试：用小上限验证超限行放弃路径，不必真写 64 MB。
#[allow(clippy::too_many_arguments)]
fn scan_streaming_tail(
    storage: &Storage,
    adapter: &dyn Adapter,
    tool: Tool,
    path: &Path,
    path_str: &str,
    titled: &mut HashSet<String>,
    report: &mut ScanReport,
    file_id: i64,
    size: i64,
    cursor: i64,
    now: i64,
    max_line: usize,
) -> Result<()> {
    let mut file = File::open(path).map_err(|source| Error::DataFile {
        path: path.to_path_buf(),
        source,
    })?;
    file.seek(SeekFrom::Start(cursor.max(0) as u64))
        .map_err(|source| Error::DataFile {
            path: path.to_path_buf(),
            source,
        })?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut hasher = Sha256::new();
    // committed = 最后一个已处理完整行的行尾。游标只落在完整行边界上：
    // 中途的行（不完整尾行、超限丢弃行）不越过，续扫语义与缓冲路径一致。
    let mut committed = cursor.max(0) as u64;
    let mut since_checkpoint = committed;
    let mut line = Vec::new();
    let mut malformed = 0u32;
    let mut ctx = FactContext {
        storage,
        adapter,
        tool,
        file_id,
        path_str,
        titled,
        report,
    };
    loop {
        // 逐块读完一行：正常行进 line_buf；超过上限立即转丢弃模式——后续
        // 字节只计数、不缓冲、不进哈希，直到行尾。
        line.clear();
        let mut overflow = false;
        let mut consumed = 0u64;
        let complete;
        loop {
            enum Step {
                /// 本块无换行：整块计入当前行（或丢弃），继续读。
                Whole(usize),
                /// 本块内找到行尾：consume 的字节数，行到此完结。
                LineEnd(usize),
                Eof,
            }
            let step = {
                let chunk = reader.fill_buf().map_err(|source| Error::DataFile {
                    path: path.to_path_buf(),
                    source,
                })?;
                if chunk.is_empty() {
                    Step::Eof
                } else {
                    match chunk.iter().position(|&b| b == b'\n') {
                        Some(i) => {
                            if !overflow {
                                line.extend_from_slice(&chunk[..=i]);
                            }
                            Step::LineEnd(i + 1)
                        }
                        None => {
                            if !overflow {
                                line.extend_from_slice(chunk);
                                if line.len() > max_line {
                                    overflow = true;
                                    line.clear();
                                    line.shrink_to_fit();
                                }
                            }
                            Step::Whole(chunk.len())
                        }
                    }
                }
            };
            match step {
                Step::Eof => {
                    complete = false; // 不完整尾行留给下次。
                    break;
                }
                Step::Whole(n) => {
                    reader.consume(n);
                    consumed += n as u64;
                }
                Step::LineEnd(n) => {
                    reader.consume(n);
                    consumed += n as u64;
                    complete = true;
                    break;
                }
            }
        }
        if !complete {
            break;
        }
        if overflow {
            // 巨型行放弃：不解析、不进哈希，游标越过即可。
            committed += consumed;
        } else {
            hasher.update(&line);
            committed += consumed;
            let Ok(text) = std::str::from_utf8(&line) else {
                malformed += 1;
                continue;
            };
            let text = text.trim_end_matches(['\r', '\n']);
            match adapter.parse_line(text) {
                LineParse::Skip => {}
                LineParse::Malformed => malformed += 1,
                LineParse::Facts(facts) => apply_facts(&mut ctx, text, *facts)?,
                // 流式路径只走行级自洽的适配器（supports_streaming_scan 门槛），
                // 拆分只发生在文件级解析的 grok——此臂防御性兜底，逐条入账。
                LineParse::Split(facts) => {
                    for fact in facts {
                        apply_facts(&mut ctx, text, fact)?;
                    }
                }
            }
        }
        if committed - since_checkpoint >= STREAM_CHECKPOINT_BYTES {
            since_checkpoint = committed;
            storage.upsert_scanned_file(
                path_str,
                tool.as_str(),
                &hasher.clone().finalize(),
                size,
                i64::try_from(committed).unwrap_or(i64::MAX),
                now,
            )?;
        }
    }
    ctx.report.parse_errors += malformed;
    storage.upsert_scanned_file(
        path_str,
        tool.as_str(),
        &hasher.finalize(),
        size,
        i64::try_from(committed).unwrap_or(i64::MAX),
        now,
    )?;
    ctx.report.files_scanned += 1;
    Ok(())
}

/// 数据库类来源的扫描：库路径复用 `scanned_files` 簿记，指纹存进 `content_hash`。
/// 每轮全量重放事实，重扫入账由 dedup_key（库路径 + 行键）拦住。
fn scan_db_source(
    storage: &Storage,
    adapter: &dyn Adapter,
    tool: Tool,
    source: DbSource,
    titled: &mut HashSet<String>,
    report: &mut ScanReport,
) -> Result<()> {
    let path_str = source.path.to_string_lossy().into_owned();
    let now = now_ms();

    let meta = match std::fs::metadata(&source.path) {
        Ok(meta) => meta,
        // 适配器对"库不存在"返回 None，走到这里说明登记过却消失：只标 missing。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(state) = storage.scanned_file_state(&path_str)? {
                storage.set_file_missing(state.id, Some(now))?;
                report.files_missing += 1;
            }
            return Ok(());
        }
        Err(err) => {
            return Err(Error::DataFile {
                path: source.path,
                source: err,
            });
        }
    };
    let size = i64::try_from(meta.len()).unwrap_or(i64::MAX);

    let state = storage.scanned_file_state(&path_str)?;
    if state
        .as_ref()
        .is_some_and(|s| s.content_hash == source.fingerprint)
    {
        // 指纹未变：无新增；upsert 顺带清掉 missing 标记。
        storage.upsert_scanned_file(&path_str, tool.as_str(), &source.fingerprint, size, 0, now)?;
        return Ok(());
    }

    let file_id = storage.upsert_scanned_file(
        &path_str,
        tool.as_str(),
        state.as_ref().map_or(&[], |s| &s.content_hash),
        size,
        0,
        now,
    )?;
    let mut ctx = FactContext {
        storage,
        adapter,
        tool,
        file_id,
        path_str: &path_str,
        titled,
        report,
    };
    for (key, facts) in source.facts {
        apply_facts(&mut ctx, &key, facts)?;
    }
    storage.upsert_scanned_file(&path_str, tool.as_str(), &source.fingerprint, size, 0, now)?;
    report.files_scanned += 1;
    Ok(())
}

/// 单个文件解析期间的共享上下文：一次会话/记录入账需要的东西都在这里。
struct FactContext<'a> {
    storage: &'a Storage,
    adapter: &'a dyn Adapter,
    tool: Tool,
    file_id: i64,
    path_str: &'a str,
    /// 本轮已采过标题的会话（标题首见为准）。
    titled: &'a mut HashSet<String>,
    report: &'a mut ScanReport,
}

fn apply_facts(ctx: &mut FactContext, raw_line: &str, facts: LineFacts) -> Result<()> {
    let FactContext {
        storage,
        adapter,
        tool,
        file_id,
        path_str,
        titled,
        report,
    } = ctx;
    // 项目目录统一成 Windows 反斜杠口径：不同工具的来源斜杠风格不一（zcode 的
    // 应用库给 `/`，claude-code 的 cwd 给 `\`），不归一的话同一项目在分组与
    // 筛选里会裂成两行。库内比较全是字符串精确匹配，必须在入账前归一。
    let project_dir = facts.project_dir.map(|dir| {
        if cfg!(windows) {
            dir.replace('/', "\\")
        } else {
            dir
        }
    });
    // 墓碑在册的会话不再建行：共享日志（codebuddy 扩展日志等）轮转/重写时
    // 整文件重解析，没有这道闸，已删会话会以 0 用量空壳复活。同会话的新
    // 用量一并跳过——用户删的就是这个会话，复活后的请求不该再入账。
    if storage.is_session_deleted(tool.as_str(), &facts.session_external_id)? {
        return Ok(());
    }
    let (session_id, created) = storage.get_or_create_session(
        &NewSession {
            tool: tool.as_str().to_string(),
            external_id: facts.session_external_id.clone(),
            external_id_is_derived: adapter.external_id_is_derived(),
            title: None,
            project_dir,
            source_file_id: *file_id,
        },
        now_ms(),
    )?;
    if created {
        report.sessions_created += 1;
        // 新建即以事实时间戳落定活动时间：backfill 的老会话（如 zcode db 源
        // 入账被回收过日志的会话）不该被建行时刻顶到列表最前。
        if let Some(ts) = facts.ts_ms {
            storage.init_session_activity(session_id, ts)?;
        }
    }

    if let Some(title) = &facts.title
        && titled.insert(facts.session_external_id.clone())
    {
        storage.update_session_activity(session_id, facts.ts_ms.unwrap_or(0), Some(title))?;
    }

    if let Some(usage) = facts.usage {
        // 同一行拆出的多条事实（grok 请求级拆分）靠后缀区分：None 后缀串空串，
        // 与"无后缀"等价，不改变整行单条事实的既有键值。
        let dedup_key = Sha256::new()
            .chain_update(tool.as_str())
            .chain_update([0u8])
            .chain_update(path_str)
            .chain_update([0u8])
            .chain_update(raw_line)
            .chain_update(facts.dedup_suffix.as_deref().unwrap_or(""))
            .finalize()
            .to_vec();
        let inserted = storage.insert_usage_record(&NewUsageRecord {
            session_id: Some(session_id),
            tool: tool.as_str().to_string(),
            model_raw: usage.model_raw,
            model: usage.model,
            ts: usage.ts_ms,
            input_tokens: usage.usage.input_tokens,
            output_tokens: usage.usage.output_tokens,
            cache_read_tokens: usage.usage.cache_read_tokens,
            cache_write_tokens: usage.usage.cache_write_tokens,
            reasoning_tokens: usage.usage.reasoning_tokens,
            duration_ms: usage.duration_ms,
            request_count: facts.request_count,
            dedup_key,
        })?;
        if inserted {
            report.records_inserted += 1;
            storage.update_session_activity(session_id, usage.ts_ms, None)?;
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::adapter::codex::CodexAdapter;
    use crate::adapter::{Adapter, LineParse, UsageFacts};
    use crate::model::TokenUsage;
    use crate::storage::Storage;

    /// 假适配器：行格式一行一个 JSON——`{"id":"…","ts":1,"tokens":[i,o,cr,cw]}`
    /// 记用量、`{"id":"…","title":"…"}` 记标题。日志目录注入，可对临时目录测试。
    struct FakeAdapter(PathBuf);

    impl Adapter for FakeAdapter {
        fn tool(&self) -> Tool {
            Tool::Codex
        }

        fn log_dirs(&self) -> Vec<PathBuf> {
            vec![self.0.clone()]
        }

        fn is_session_log(&self, path: &Path) -> bool {
            path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        }

        fn parse_line(&self, line: &str) -> LineParse {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => return LineParse::Malformed,
            };
            let id = match v.get("id").and_then(|s| s.as_str()) {
                Some(id) => id.to_string(),
                None => return LineParse::Skip,
            };
            let title = v.get("title").and_then(|s| s.as_str()).map(str::to_string);
            let usage = v.get("tokens").map(|tokens| UsageFacts {
                ts_ms: v.get("ts").and_then(|n| n.as_i64()).unwrap_or(0),
                model_raw: "raw-model-v2-20250101".to_string(),
                model: "raw-model-v2".to_string(),
                duration_ms: None,
                usage: TokenUsage {
                    input_tokens: bucket(tokens, 0),
                    output_tokens: bucket(tokens, 1),
                    cache_read_tokens: bucket(tokens, 2),
                    cache_write_tokens: bucket(tokens, 3),
                    reasoning_tokens: None,
                },
            });
            LineParse::Facts(Box::new(LineFacts {
                dedup_suffix: None,
                request_count: 1,
                session_external_id: id,
                title,
                project_dir: None,
                ts_ms: usage.as_ref().map(|u| u.ts_ms),
                usage,
            }))
        }
    }

    fn bucket(tokens: &serde_json::Value, i: usize) -> i64 {
        tokens.get(i).and_then(|n| n.as_i64()).unwrap_or(0)
    }

    fn test_db(tag: &str) -> Storage {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toktol-scan-db-{}-{}-{tag}.db",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_file(&path);
        crate::storage::open(&path).expect("打开测试库")
    }

    fn write_log(dir: &Path, name: &str, lines: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), lines.join("\n") + "\n").unwrap();
    }

    /// 拆分适配器：一行 `{"id":"s1","n":2,"tokens":[10,5,0,0]}` 产出 n 条
    /// 请求级事实（r0..，第 k 条 ts = k+1）；n = 1 时不带后缀单行。
    struct SplitAdapter(PathBuf);

    impl Adapter for SplitAdapter {
        fn tool(&self) -> Tool {
            Tool::Codex
        }

        fn log_dirs(&self) -> Vec<PathBuf> {
            vec![self.0.clone()]
        }

        fn is_session_log(&self, path: &Path) -> bool {
            path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        }

        // 只在文件级实现拆分；行级路径不会走到。
        fn parse_line(&self, _line: &str) -> LineParse {
            LineParse::Skip
        }

        fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
            lines[start..]
                .iter()
                .map(|line| {
                    let v: serde_json::Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(_) => return LineParse::Malformed,
                    };
                    let Some(id) = v.get("id").and_then(|s| s.as_str()).map(str::to_string) else {
                        return LineParse::Skip;
                    };
                    let n = v.get("n").and_then(|x| x.as_i64()).unwrap_or(1).max(1) as usize;
                    let tokens =
                        |i: usize| bucket(v.get("tokens").unwrap_or(&serde_json::Value::Null), i);
                    let fact = |k: usize| LineFacts {
                        dedup_suffix: Some(format!("r{k}")),
                        request_count: 1,
                        session_external_id: id.clone(),
                        title: None,
                        project_dir: None,
                        ts_ms: Some(k as i64 + 1),
                        usage: Some(UsageFacts {
                            ts_ms: k as i64 + 1,
                            model_raw: "m".into(),
                            model: "m".into(),
                            duration_ms: None,
                            usage: TokenUsage {
                                input_tokens: tokens(0) / n as i64,
                                output_tokens: tokens(1) / n as i64,
                                cache_read_tokens: 0,
                                cache_write_tokens: 0,
                                reasoning_tokens: None,
                            },
                        }),
                    };
                    if n == 1 {
                        let mut fact = fact(0);
                        fact.dedup_suffix = None;
                        return LineParse::Facts(Box::new(fact));
                    }
                    LineParse::Split((0..n).map(fact).collect())
                })
                .collect()
        }
    }

    /// Split 行的入账与 dedup：一行拆出的 n 条事实首次各入一行，重扫由
    /// 后缀区分的 dedup 键整体拦住，不重复入账。
    #[test]
    fn split_rows_are_deduped_on_rescan() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("split");
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","n":2,"tokens":[10,5,0,0]}"#],
        );

        let mut report = ScanReport::default();
        scan_adapter(&storage, &SplitAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 2, "一行拆出两条请求级事实");

        let (rows, requests): (i64, i64) = storage
            .conn()
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(request_count), 0) FROM usage_records",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .unwrap();
        assert_eq!((rows, requests), (2, 2), "行数与请求数口径一致");

        let mut report = ScanReport::default();
        scan_adapter(&storage, &SplitAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 0, "重扫由后缀 dedup 拦住");
    }

    #[test]
    fn append_only_scan_is_incremental_and_deduped() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-inc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("inc");
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[10,5,0,0]}"#],
        );

        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);
        assert_eq!(report.sessions_created, 1);

        // 再扫同一文件：无新增。
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 0);
        assert_eq!(report.files_scanned, 0);

        // 追加一行：只解析新增部分。
        let mut text = std::fs::read_to_string(dir.join("a.jsonl")).unwrap();
        text.push_str(r#"{"id":"s1","ts":2,"tokens":[1,2,3,4]}"#);
        text.push('\n');
        std::fs::write(dir.join("a.jsonl"), &text).unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);

        // 追加不完整尾行：不入账，游标停在行首，下次续上。
        std::fs::write(dir.join("a.jsonl"), text + r#"{"id":"s1","ts":3,"to"#).unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rewritten_smaller_file_is_rescanned_without_duplicates() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-rew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("rew");
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[10,0,0,0]}"#],
        );
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();

        // 轮转重写：文件变短、内容不同；重写内容里的重复行只入一次。
        write_log(
            &dir,
            "a.jsonl",
            &[
                r#"{"id":"s1","ts":9,"tokens":[99,0,0,0]}"#,
                r#"{"id":"s1","ts":9,"tokens":[99,0,0,0]}"#,
            ],
        );
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 1, "重写后只入新内容");

        let total: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "旧内容 + 新内容，不得重复");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_is_flagged_and_recovery_clears_it() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("miss");
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[1,0,0,0]}"#],
        );
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();

        std::fs::remove_file(dir.join("a.jsonl")).unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.files_missing, 1);
        let missing: Option<i64> = storage
            .conn()
            .query_row("SELECT missing_since FROM scanned_files", [], |r| r.get(0))
            .unwrap();
        assert!(missing.is_some(), "文件消失必须被标记");

        // 文件回来了：标记清除，数据不重复。
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[1,0,0,0]}"#],
        );
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        let missing: Option<i64> = storage
            .conn()
            .query_row("SELECT missing_since FROM scanned_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(missing, None, "文件恢复后 missing 标记必须清除");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn title_comes_from_first_fact_and_survives_rescan() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-title-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("title");
        write_log(
            &dir,
            "a.jsonl",
            &[
                r#"{"id":"s1","title":"第一条"}"#,
                r#"{"id":"s1","title":"第二条"}"#,
                r#"{"id":"s1","ts":1,"tokens":[1,0,0,0]}"#,
            ],
        );
        scan_with(&storage, &dir);
        scan_with(&storage, &dir);

        let title: Option<String> = storage
            .conn()
            .query_row(
                "SELECT title FROM sessions WHERE external_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("第一条"), "标题首见为准，重扫不漂移");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn scan_with(storage: &Storage, dir: &Path) {
        let mut report = ScanReport::default();
        scan_adapter(storage, &FakeAdapter(dir.to_path_buf()), &mut report).unwrap();
    }

    #[test]
    fn codex_resume_scan_keeps_model_from_earlier_lines() {
        // rollout 的模型在 turn_context 行里，usage 行不带；游标续扫必须能从
        // 文件前缀重建上下文，否则新回合的用量会丢模型。日志目录注入临时目录，
        // 不碰本机真实的 ~/.codex。
        struct ScopedCodex(PathBuf);

        impl Adapter for ScopedCodex {
            fn tool(&self) -> Tool {
                Tool::Codex
            }
            fn log_dirs(&self) -> Vec<PathBuf> {
                vec![self.0.clone()]
            }
            fn is_session_log(&self, path: &Path) -> bool {
                CodexAdapter.is_session_log(path)
            }
            fn parse_line(&self, line: &str) -> LineParse {
                CodexAdapter.parse_line(line)
            }
            fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
                CodexAdapter.parse_file(lines, start)
            }
        }

        let dir = std::env::temp_dir().join(format!("toktol-scan-codex-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("codex");
        let log = dir.join("rollout-2026-09-12T07-19-10-01a0947c.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &log,
            [
                r#"{"timestamp":"2026-09-12T07:19:10.911Z","type":"session_meta","payload":{"id":"s1","session_id":"s1","cwd":"E:\\p","source":"cli","model":"m-a"}}"#,
                r#"{"timestamp":"2026-09-12T07:20:00.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"m-a"}}"#,
                r#"{"timestamp":"2026-09-12T07:20:10.000Z","type":"token_usage_record","payload":{"session_id":"s1","usage":{"input_tokens":10,"output_tokens":5}}}"#,
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &ScopedCodex(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);

        // 追加第二个回合：usage 行依旧不带模型，模型只在其前的 turn_context 里。
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str(
            r#"{"timestamp":"2026-09-12T07:21:00.000Z","type":"turn_context","payload":{"turn_id":"t2","model":"m-b"}}"#,
        );
        text.push('\n');
        text.push_str(
            r#"{"timestamp":"2026-09-12T07:21:10.000Z","type":"token_usage_record","payload":{"session_id":"s1","usage":{"input_tokens":20,"output_tokens":6}}}"#,
        );
        text.push('\n');
        std::fs::write(&log, text).unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &ScopedCodex(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 1, "续扫只入新回合的用量");

        let models: Vec<String> = storage
            .conn()
            .prepare("SELECT model_raw FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(models, vec!["m-a", "m-b"], "续扫的新记录必须取到本回合模型");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opencode_db_source_scans_incrementally() {
        use crate::adapter::opencode::OpenCodeAdapter;

        let db_path =
            std::env::temp_dir().join(format!("toktol-scan-oc-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT, title TEXT,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL,
                data TEXT NOT NULL);
             INSERT INTO session (id, directory, title, time_created, time_updated)
             VALUES ('ses_a', 'E:/p', 't', 1, 1);",
        )
        .unwrap();
        let data = r#"{"role":"assistant","modelID":"m-a","tokens":{"input":10,"output":5,"reasoning":2,"cache":{"read":3,"write":0}},"time":{"created":100}}"#;
        db.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('msg_1', 'ses_a', 100, 100, ?1)",
            rusqlite::params![data],
        )
        .unwrap();
        drop(db);

        let storage = test_db("opencode");
        let adapter = OpenCodeAdapter {
            db_path: Some(db_path.clone()),
        };
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);
        assert_eq!(report.sessions_created, 1);

        // 指纹未变：整轮跳过，重放的事实一条也不重复入账。
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 0);
        assert_eq!(report.files_scanned, 0);

        // 新消息：指纹变化触发全量重放，但只有新行入账。
        let db = rusqlite::Connection::open(&db_path).unwrap();
        let data2 = r#"{"role":"assistant","modelID":"m-b","tokens":{"input":20,"output":6,"reasoning":0,"cache":{"read":0,"write":0}},"time":{"created":200}}"#;
        db.execute(
            "INSERT INTO message (id, session_id, time_created, time_updated, data) VALUES ('msg_2', 'ses_a', 200, 200, ?1)",
            rusqlite::params![data2],
        )
        .unwrap();
        drop(db);
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(
            report.records_inserted, 1,
            "重放旧行被 dedup 拦住，只入新行"
        );

        let models: Vec<String> = storage
            .conn()
            .prepare("SELECT model_raw FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(models, vec!["m-a", "m-b"]);

        // 库消失：对账标记 missing，数据保留。
        std::fs::remove_file(&db_path).unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.files_missing, 1);
        let missing: Option<i64> = storage
            .conn()
            .query_row("SELECT missing_since FROM scanned_files", [], |r| r.get(0))
            .unwrap();
        assert!(missing.is_some(), "库消失必须被标记");
    }

    #[test]
    fn compressed_whole_file_sources_reparse_and_dedup() {
        // decode 返回 Owned 的适配器（如 dsh 的 zstd 日志）没有增量游标：
        // 每次内容变化都整文件重解析，重复入账靠 dedup_key 拦，指纹未变整体跳过。
        struct OwnedDecodeAdapter(PathBuf);

        impl Adapter for OwnedDecodeAdapter {
            fn tool(&self) -> Tool {
                Tool::Grok
            }

            fn log_dirs(&self) -> Vec<PathBuf> {
                vec![self.0.clone()]
            }

            fn is_session_log(&self, path: &Path) -> bool {
                path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            }

            fn parse_line(&self, line: &str) -> LineParse {
                FakeAdapter(self.0.clone()).parse_line(line)
            }

            // 输入输出不同即触发 Owned 路径；内容语义与明文路径一致。
            fn decode<'a>(&self, raw: &'a [u8]) -> Option<Cow<'a, [u8]>> {
                Some(Cow::Owned(raw.to_vec()))
            }
        }

        let dir = std::env::temp_dir().join(format!("toktol-scan-owned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("owned");
        let line = r#"{"id":"s1","ts":1,"tokens":[10,0,0,0]}"#;
        write_log(&dir, "a.jsonl", &[line]);

        let adapter = OwnedDecodeAdapter(dir.clone());
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);

        // 重扫：内容指纹未变，整体跳过。
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 0);
        assert_eq!(report.files_scanned, 0);

        // 追加后整文件重解析：旧行被 dedup 拦住，只入新行。
        let mut text = std::fs::read_to_string(dir.join("a.jsonl")).unwrap();
        text.push_str(r#"{"id":"s1","ts":2,"tokens":[20,0,0,0]}"#);
        text.push('\n');
        std::fs::write(dir.join("a.jsonl"), &text).unwrap();
        let mut report = ScanReport::default();
        scan_adapter(&storage, &adapter, &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 删除流程要求钉住的行为：删除会话（文件移走）后还原文件重扫，墓碑
    /// 拦住建行——已删会话不复活为空壳；已入账的用量保持孤儿行，dedup_key
    /// 拦住重复统计。
    #[test]
    fn rescan_after_session_delete_does_not_revive_deleted_session() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-shell-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("shell");
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[10,5,0,0]}"#],
        );
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 1);

        // 删除会话：trash 注入为"直接删除文件"（本测试不关心回收站）。
        let session_id: i64 = storage
            .conn()
            .query_row(
                "SELECT id FROM sessions WHERE external_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        crate::sessions::trash_session_with(
            &storage,
            session_id,
            &FakeAdapter(dir.clone()),
            &dir,
            &|path| {
                std::fs::remove_file(path)
                    .map_err(|e| crate::error::Error::Internal(format!("remove failed: {e}")))
            },
        )
        .unwrap();

        // 还原文件重扫：重建零用量壳，用量不重复入账。
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[10,5,0,0]}"#],
        );
        let mut report = ScanReport::default();
        scan_adapter(&storage, &FakeAdapter(dir.clone()), &mut report).unwrap();
        assert_eq!(report.records_inserted, 0, "dedup 必须拦住重复入账");

        let (sessions, usage, orphan): (i64, i64, i64) = storage
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sessions),
                        (SELECT COUNT(*) FROM usage_records),
                        (SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (sessions, usage, orphan),
            (0, 1, 1),
            "墓碑拦住建行 + 用量保留为孤儿行"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
    /// 全部工具禁用：一个适配器都不跑，报告全零（hermetic，不触真实家目录）。
    #[test]
    fn run_scan_skips_disabled_tools() {
        let storage = test_db("run-scan-disabled");
        let all: Vec<String> = crate::model::Tool::ALL
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        let report = run_scan(&storage, &all).unwrap();
        assert_eq!(
            (
                report.files_scanned,
                report.records_inserted,
                report.sessions_created
            ),
            (0, 0, 0),
            "全禁用时不应有任何扫描动作"
        );
        // 价格种子与重算照常执行（幂等）。
        let prices: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM model_prices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(prices, 0, "没有用量建档时种子挂不上价，属正常");
    }

    /// FakeAdapter 的行级自洽版本：声明支持流式扫描，用于超限路径的测试。
    struct StreamingAdapter(PathBuf);

    impl Adapter for StreamingAdapter {
        fn tool(&self) -> Tool {
            Tool::ZCode
        }

        fn log_dirs(&self) -> Vec<PathBuf> {
            vec![self.0.clone()]
        }

        fn is_session_log(&self, path: &Path) -> bool {
            path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        }

        fn parse_line(&self, line: &str) -> LineParse {
            FakeAdapter(self.0.clone()).parse_line(line)
        }

        fn supports_streaming_scan(&self) -> bool {
            true
        }
    }

    /// 超限文件流式增量：不跳过、逐行入账、游标推进；追加只解析尾部，
    /// 不完整尾行留待下次补上。
    #[test]
    fn oversize_file_streams_incrementally() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("big");
        let log = dir.join("a.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        // 每行 38 字节、内容互异，上限 100：文件一落地就"超限"。
        let line = |ts: u8| format!(r#"{{"id":"s1","ts":{ts},"tokens":[10,5,0,0]}}"#);
        std::fs::write(&log, format!("{}\n{}\n{}\n", line(1), line(2), line(3))).unwrap();

        let scan = |report: &mut ScanReport| {
            let mut titled = HashSet::new();
            scan_file_with_limit(
                &storage,
                &StreamingAdapter(dir.clone()),
                Tool::ZCode,
                &log,
                &mut titled,
                report,
                100,
            )
            .unwrap();
        };
        let cursor = || {
            storage
                .scanned_file_state(&log.to_string_lossy())
                .unwrap()
                .unwrap()
                .parsed_bytes
        };

        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 3, "超限文件不再被跳过");
        assert_eq!(cursor(), std::fs::metadata(&log).unwrap().len() as i64);

        // 追加两行（末行不完整）：只入完整的新行，游标停在行首。
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push_str(&line(4));
        text.push('\n');
        text.push_str(&line(5));
        std::fs::write(&log, &text).unwrap();
        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 1, "只入新增的完整行");
        let partial = std::fs::metadata(&log).unwrap().len() as i64;
        assert!(cursor() < partial, "不完整尾行不入账");

        // 补全尾行：续上入账，不重复。
        let mut text = std::fs::read_to_string(&log).unwrap();
        text.push('\n');
        std::fs::write(&log, &text).unwrap();
        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 1, "断尾行补全后恰好入账一次");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 超限文件轮转重写：size < cursor 兜底归零、整体重解析，dedup 拦住重复。
    #[test]
    fn oversize_rewritten_file_rescans_without_duplicates() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-big-rew-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("big-rew");
        let log = dir.join("a.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        let line = r#"{"id":"s1","ts":1,"tokens":[10,5,0,0]}"#;
        let line_b = r#"{"id":"s1","ts":2,"tokens":[10,5,0,0]}"#;
        let line_c = r#"{"id":"s1","ts":3,"tokens":[10,5,0,0]}"#;
        std::fs::write(&log, format!("{line}\n{line_b}\n{line_c}\n")).unwrap();
        let scan = |report: &mut ScanReport| {
            let mut titled = HashSet::new();
            scan_file_with_limit(
                &storage,
                &StreamingAdapter(dir.clone()),
                Tool::ZCode,
                &log,
                &mut titled,
                report,
                100,
            )
            .unwrap();
        };
        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 3);

        // 重写变短：旧行 + 新行，只入新的。
        let new_line = r#"{"id":"s1","ts":9,"tokens":[99,0,0,0]}"#;
        std::fs::write(&log, format!("{line}\n{new_line}\n")).unwrap();
        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 1, "重写后只入新内容");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 流式断点续扫：游标停在文件中部（上次中断时落盘的检查点）时，
    /// 只从中部继续——重扫过的行被 dedup 拦住，不从头再来。
    #[test]
    fn oversize_stream_resumes_from_checkpointed_cursor() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-big-ck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("big-ck");
        let log = dir.join("a.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        let line = |ts: u8| format!(r#"{{"id":"s1","ts":{ts},"tokens":[10,5,0,0]}}"#);
        std::fs::write(
            &log,
            format!("{}\n{}\n{}\n{}\n", line(1), line(2), line(3), line(4)),
        )
        .unwrap();
        let path_str = log.to_string_lossy().into_owned();
        let scan = |report: &mut ScanReport| {
            let mut titled = HashSet::new();
            scan_file_with_limit(
                &storage,
                &StreamingAdapter(dir.clone()),
                Tool::ZCode,
                &log,
                &mut titled,
                report,
                100,
            )
            .unwrap();
        };

        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 4);

        // 模拟中断：游标只落盘到第 2 行末（记录是逐行入库的，早已在库；
        // 只有游标没跟上——这正是周期检查点要解决的窗口）。
        let stride = line(1).len() as i64 + 1;
        storage
            .conn()
            .execute(
                "UPDATE scanned_files SET parsed_bytes = ?1 WHERE path = ?2",
                rusqlite::params![stride * 2, path_str],
            )
            .unwrap();

        // 续扫：3、4 行重新解析但被 dedup 拦住（仍在库），只有新增的第 5 行入账。
        std::fs::write(
            &log,
            format!(
                "{}\n{}\n{}\n{}\n{}\n",
                line(1),
                line(2),
                line(3),
                line(4),
                line(5)
            ),
        )
        .unwrap();
        let mut report = ScanReport::default();
        scan(&mut report);
        assert_eq!(report.records_inserted, 1, "断点续扫不重复入账");
        let cursor: i64 = storage
            .conn()
            .query_row(
                "SELECT parsed_bytes FROM scanned_files WHERE path = ?1",
                rusqlite::params![path_str],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, std::fs::metadata(&log).unwrap().len() as i64);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 巨型单行放弃：超限行不缓冲（内存有界）、不解析，游标越过，
    /// 前后的正常行照常入账。
    #[test]
    fn oversize_stream_skips_gigantic_line_without_hanging() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-big-line-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("big-line");
        let log = dir.join("a.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        let head = r#"{"id":"s1","ts":1,"tokens":[10,5,0,0]}"#;
        let tail = r#"{"id":"s1","ts":2,"tokens":[20,6,0,0]}"#;
        // 65 MB 的巨型行（高于 64 MB 行上限），夹在两条正常行中间。
        let giant = format!("{{\"pad\":\"{}\"}}", "x".repeat(65 * 1024 * 1024));
        std::fs::write(&log, format!("{head}\n{giant}\n{tail}\n")).unwrap();

        let mut report = ScanReport::default();
        let mut titled = HashSet::new();
        scan_file_with_limit(
            &storage,
            &StreamingAdapter(dir.clone()),
            Tool::ZCode,
            &log,
            &mut titled,
            &mut report,
            100, // 文件超限 → 流式
        )
        .unwrap();

        let total: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM usage_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "巨型行被放弃，前后正常行入账");
        let cursor: i64 = storage
            .conn()
            .query_row(
                "SELECT parsed_bytes FROM scanned_files WHERE path = ?1",
                rusqlite::params![log.to_string_lossy()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cursor, std::fs::metadata(&log).unwrap().len() as i64);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 首扫量级：统计流式适配器的剩余字节，未登记的新文件也计入，
    /// 非日志文件不计。簿记状态直接 upsert 模拟。
    #[test]
    fn scan_backlog_counts_remaining_bytes_of_streaming_tools_only() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-backlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("backlog");
        let log_a = dir.join("a.jsonl");
        let log_b = dir.join("b.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&log_a, "line\nline\nline\n").unwrap(); // 15 字节
        std::fs::write(&log_b, "x\n").unwrap(); // 2 字节
        std::fs::write(dir.join("c.txt"), "not a log\n").unwrap();

        // a.jsonl：簿记游标 10，剩 5 字节。
        storage
            .upsert_scanned_file(&log_a.to_string_lossy(), "zcode", &[], 15, 10, 0)
            .unwrap();

        let adapter = StreamingAdapter(dir.clone());
        let backlog = scan_backlog_of(&storage, &[&adapter]).unwrap();
        assert_eq!(backlog.files, 2, "a.jsonl 余量 + 未登记的 b.jsonl");
        assert_eq!(backlog.bytes, 7);

        // 扫尽后归零。
        storage
            .upsert_scanned_file(&log_a.to_string_lossy(), "zcode", &[], 15, 15, 1)
            .unwrap();
        storage
            .upsert_scanned_file(&log_b.to_string_lossy(), "zcode", &[], 2, 2, 1)
            .unwrap();
        let backlog = scan_backlog_of(&storage, &[&adapter]).unwrap();
        assert_eq!(
            (backlog.files, backlog.bytes),
            (0, 0),
            "已扫尽的文件不再计入"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 需要跨行上下文的适配器：超限文件维持整体跳过，不得误走流式。
    #[test]
    fn oversize_file_still_skipped_for_context_adapters() {
        let dir = std::env::temp_dir().join(format!("toktol-scan-big-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("big-skip");
        write_log(
            &dir,
            "a.jsonl",
            &[r#"{"id":"s1","ts":1,"tokens":[10,5,0,0]}"#],
        );
        let mut report = ScanReport::default();
        let mut titled = HashSet::new();
        scan_file_with_limit(
            &storage,
            &FakeAdapter(dir.clone()),
            Tool::Codex,
            &dir.join("a.jsonl"),
            &mut titled,
            &mut report,
            16,
        )
        .unwrap();
        assert_eq!(report.files_skipped, 1);
        assert_eq!(report.records_inserted, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
