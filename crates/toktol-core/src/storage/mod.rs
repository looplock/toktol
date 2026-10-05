//! SQLite 存储层：唯一主库（WAL）的迁移、PRAGMA、写入路径原语与读路径聚合。
//! 表结构与删除/重算流程的设计依据就近写在各 migration 的注释里。
//! 双计陷阱：`usage_records`（扫描）与 `gateway_requests`（网关）可能记到同一请求，
//! 汇总查询绝不能盲目相加——两表各自聚合，归属规则未拍板前不合并。

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chrono::{Datelike, Local, LocalResult, NaiveDate, TimeZone, Timelike};
use rusqlite::{
    Connection, OptionalExtension, Transaction, params, params_from_iter, types::Value,
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

mod migrations;

/// 单次写锁等待上限。WAL 下写锁持有都是毫秒级，超时说明有进程卡住而非正常竞争。
const BUSY_TIMEOUT_MS: u64 = 5_000;

const MICROS_PER_MTK: i64 = 1_000_000;

/// 当前时间（epoch ms）；时钟回拨/溢出按 0 兜底，只影响 updated_at 语义。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// 定价页载荷的子结构（camelCase 与父级一致）。
pub mod pricing_overview {
    use serde::Serialize;

    /// 目录建议价（收敛到一条后）。
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct CatalogHint {
        /// 供应商标识。
        pub provider: String,
        /// 目录模型 id（与标准名同风格）。
        pub model_id: String,
        /// 目录展示名。
        pub display_name: Option<String>,
        /// `active` / `deprecated`。
        pub status: Option<String>,
        /// 输入桶建议价。
        pub input_per_mtok_micros: Option<i64>,
        /// 输出桶建议价。
        pub output_per_mtok_micros: Option<i64>,
        /// 缓存读建议价。
        pub cache_read_per_mtok_micros: Option<i64>,
        /// 缓存写建议价。
        pub cache_write_per_mtok_micros: Option<i64>,
    }

    /// 指向某标准模型的一个原始变种名。
    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct PricingVariant {
        /// 原始变种名（日志里的 model_raw）。
        pub raw_model: String,
        /// 映射来源：`auto`（确定性归一）/ `suggested`（启发合并，需过目）/ `user`。
        pub source: String,
    }
}

/// 两张事实表。列名与语义完全同构（token 桶 + 分项成本），凡"按表跑一遍"的语句
/// 都在这里循环——新增事实表时加进来即可。
const FACT_TABLES: [&str; 2] = ["usage_records", "gateway_requests"];

/// 事务内改一条映射并跟着改写两张事实表（不带重算——调用方决定何时跑管线）。
fn repoint_mapping_tx(
    tx: &Transaction,
    raw_model: &str,
    new_model_id: &str,
    source: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
         VALUES (?1, ?1, ?2)",
        params![new_model_id, now],
    )?;
    tx.execute(
        "UPDATE model_mappings
         SET model_id = ?2, source = ?3, updated_at = ?4
         WHERE raw_model = ?1",
        params![raw_model, new_model_id, source, now],
    )?;
    for table in FACT_TABLES {
        tx.execute(
            &format!(
                "UPDATE {table}
                 SET model = (SELECT m.model_id FROM model_mappings AS m
                              WHERE m.raw_model = ?1)
                 WHERE model_raw = ?1"
            ),
            params![raw_model],
        )?;
    }
    Ok(())
}

/// 删除空壳模型行：没有映射、没有价格、两张事实表里都没有用量。映射改指后
/// 留下的遗留壳（如 `z-ai/glm-5.3-flash`）靠这条清掉；有价格的（用户先挂价的）
/// 一律保留。
fn delete_orphan_models(tx: &Transaction) -> Result<usize> {
    let deleted = tx.execute(
        "DELETE FROM models
         WHERE id NOT IN (SELECT model_id FROM model_mappings)
           AND id NOT IN (SELECT model_id FROM model_prices)
           AND id NOT IN (SELECT model FROM usage_records)
           AND id NOT IN (SELECT model FROM gateway_requests)",
        [],
    )?;
    Ok(deleted)
}

/// 按价格表分桶重算 `*_cost_micros` 的语句模板：`{t}` 替换为事实表名，
/// `{models}` 替换为范围过滤（全量为空串，受限重算为 `AND r.model IN (…)`）。
/// 分桶判未知：输入/输出桶有量而无单价 → 该桶 NULL；缓存读/写无单价时**按
/// 输入价计**（缓存价缺省跟随输入价），输入价也缺才 NULL。除以 10^6 截断，
/// 单桶误差 < 1 micro-dollar。输出桶的计价量含 `reasoning_tokens`
/// （按输出价计费）。前置语句（[`reprice_scoped`]）先清掉无价格行模型的分项，
/// 本句只碰有价格行的。
const REPRICE_SQL_TEMPLATE: &str = "
UPDATE {t} AS r
SET
    input_cost_micros = CASE
        WHEN r.input_tokens > 0 AND p.input_per_mtok_micros IS NULL THEN NULL
        ELSE COALESCE(r.input_tokens, 0) * COALESCE(p.input_per_mtok_micros, 0) / ?
    END,
    output_cost_micros = CASE
        WHEN r.output_tokens + COALESCE(r.reasoning_tokens, 0) > 0
             AND p.output_per_mtok_micros IS NULL THEN NULL
        ELSE (COALESCE(r.output_tokens, 0) + COALESCE(r.reasoning_tokens, 0))
             * COALESCE(p.output_per_mtok_micros, 0) / ?
    END,
    cache_read_cost_micros = CASE
        WHEN r.cache_read_tokens > 0 AND p.cache_read_per_mtok_micros IS NULL
             AND p.input_per_mtok_micros IS NULL THEN NULL
        ELSE COALESCE(r.cache_read_tokens, 0)
             * COALESCE(p.cache_read_per_mtok_micros, p.input_per_mtok_micros, 0) / ?
    END,
    cache_write_cost_micros = CASE
        WHEN r.cache_write_tokens > 0 AND p.cache_write_per_mtok_micros IS NULL
             AND p.input_per_mtok_micros IS NULL THEN NULL
        ELSE COALESCE(r.cache_write_tokens, 0)
             * COALESCE(p.cache_write_per_mtok_micros, p.input_per_mtok_micros, 0) / ?
    END
FROM model_prices AS p
WHERE p.model_id = r.model{models}";

/// 一次会话登记的入库输入。
pub struct NewSession {
    /// 合法值由扫描层校验；本层存原样 TEXT。
    pub tool: String,
    /// 工具侧的会话标识。
    pub external_id: String,
    /// `true` = 工具未提供 id，由扫描层派生。
    pub external_id_is_derived: bool,
    /// 工具日志未提供则为 `None`。
    pub title: Option<String>,
    /// 工具日志未提供则为 `None`。
    pub project_dir: Option<String>,
    /// 首次见到该会话的日志文件（`scanned_files.id`）。
    pub source_file_id: i64,
}

/// 一次用量明细的入库输入。
///
/// `model` 是调用方的**建议值**：仅当 `(tool, model_raw)` 尚无映射时生效并登记 auto
/// 映射；已有映射（auto 或 user）一律以映射为准，保证 `usage_records.model` 与
/// `model_mappings` 单一事实源。
pub struct NewUsageRecord {
    /// 网关来源走独立表 `gateway_requests`，扫描来源为 `Some`。
    pub session_id: Option<i64>,
    /// 合法值由扫描层校验；本层存原样 TEXT。
    pub tool: String,
    /// 日志原文，永不改写。
    pub model_raw: String,
    /// 建议的归一化模型 id，生效规则见结构体文档。
    pub model: String,
    /// epoch ms（UTC）。
    pub ts: i64,
    /// 输入桶计数，缺省 0。
    pub input_tokens: i64,
    /// 输出桶计数，缺省 0。
    pub output_tokens: i64,
    /// 缓存读桶计数，缺省 0。
    pub cache_read_tokens: i64,
    /// 缓存写桶计数，缺省 0。
    pub cache_write_tokens: i64,
    /// 老日志可能没有。
    pub reasoning_tokens: Option<i64>,
    /// 请求耗时（毫秒）；日志没带计时则为 `None`。
    pub duration_ms: Option<i64>,
    /// 本行代表的网络请求数；一行即一次请求时为 1，grok 请求级拆分外的
    /// 整轮聚合行为轮次内的真实请求数（modelCalls）。
    pub request_count: i64,
    /// `sha256(tool ‖ 文件标识 ‖ 记录内容)`，入库幂等的依据。
    pub dedup_key: Vec<u8>,
}

/// 一次网关请求的计量入库输入。
///
/// `model` 的生效规则与 [`NewUsageRecord`] 相同：`(tool, model_raw)` 有映射以映射为准；
/// `tool` 为 `None` 时（网关侧识别不了发起工具）无映射可查，`model` 建议值直接生效，
/// 且不登记映射——映射表以 `(tool, raw_model)` 为主键，NULL 归属行没有稳定的映射键。
pub struct NewGatewayRequest {
    /// 发起方工具；网关识别不了则为 `None`（阶段 4 恒如此）。
    pub tool: Option<String>,
    /// 请求里的原始模型名，永不改写。
    pub model_raw: String,
    /// 建议的归一化模型 id，生效规则见结构体文档。
    pub model: String,
    /// epoch ms（UTC）。
    pub ts: i64,
    /// 输入桶计数，缺省 0。
    pub input_tokens: i64,
    /// 输出桶计数，缺省 0。
    pub output_tokens: i64,
    /// 缓存读桶计数，缺省 0。
    pub cache_read_tokens: i64,
    /// 缓存写桶计数，缺省 0。
    pub cache_write_tokens: i64,
    /// 上游未拆分推理明细时为 `None`。
    pub reasoning_tokens: Option<i64>,
    /// 上游响应状态码；请求天折在拿到响应前为 `None`。
    pub status_code: Option<i64>,
    /// 请求总耗时（毫秒）。
    pub latency_ms: Option<i64>,
    /// 命中的上游名（gateway.json 的 `upstreams[].name`）。
    pub upstream: Option<String>,
    /// 访问令牌的 sha256，绝不存明文（隐私红线）。
    pub token_hash: Option<Vec<u8>>,
}

/// 打开后的主库句柄；只能经 [`open`] 构造（PRAGMA 与迁移在打开时完成）。
pub struct Storage {
    conn: Connection,
}

/// [`Storage::scanned_file_state`] 的返回：扫描簿记的当前状态。
pub struct ScannedFileState {
    /// `scanned_files` 行 id。
    pub id: i64,
    /// 增量游标：已解析到文件的哪个字节偏移。
    pub parsed_bytes: i64,
    /// 上次扫描时的整文件内容指纹。
    pub content_hash: Vec<u8>,
}

/// 转录索引的一行：一条转录条目在源文件中的定位元数据。
/// 正文只在源文件里（红线），这里只有角色/时间/片段字节区间。
pub struct NewTranscriptEntry {
    /// 全局顺序号（同一 file_id 内从 0 递增，与转录顺序一致）。
    pub seq: i64,
    /// 发送方角色（user/assistant/system/tool）。
    pub role: String,
    /// 消息时间（epoch ms）；日志未携带则 `None`。
    pub ts_ms: Option<i64>,
    /// 产出模型（仅末轮响应条目有）。
    pub model: Option<String>,
    /// 片段语义：0 = 消息对象，1 = 追加式消息（配合 skip_blocks），2 = 末轮响应对象。
    pub kind: i64,
    /// 片段在源文件中的起始字节偏移。
    pub frag_offset: i64,
    /// 片段字节长度。
    pub frag_len: i64,
    /// kind = 1 时：旧 content 数组的长度（读取时跳过这些块，只渲染追加段）。
    pub skip_blocks: Option<i64>,
    /// 行元数据（kind = 1 且 role = tool 时存旧消息的 tool_call_id）。
    pub frag_meta: Option<String>,
}

/// [`Storage::transcript_index_state`] 的返回：一个源文件的索引建站状态。
pub struct TranscriptIndexState {
    /// 已建站到的文件字节偏移（行边界；检查点）。
    pub built_offset: i64,
    /// 建站时的文件大小（指纹一半）。
    pub size: i64,
    /// 建站时的文件 mtime（指纹另一半；不符即整表重建）。
    pub mtime_ms: i64,
    /// 是否已建完整文件。
    pub complete: bool,
    /// diff 基线（最后一条 main_turn 快照的 messages 数组字节区间）：续建时
    /// seek 读回物化，增量 diff 才有前序基线。
    /// 末条 main_turn 快照 messages 数组的起始偏移（续建时读回当 diff 基线）。
    pub baseline_offset: Option<i64>,
    /// 该数组的字节长度。
    pub baseline_len: Option<i64>,
    /// 末轮响应悬挂（尚未被后续快照作废、也未落为条目）：绝对字节区间。
    /// 末轮响应片段在源文件中的起始偏移。
    pub pending_offset: Option<i64>,
    /// 末轮响应片段长度。
    pub pending_len: Option<i64>,
    /// 末轮响应时间。
    pub pending_ts: Option<i64>,
    /// 末轮响应模型。
    pub pending_model: Option<String>,
}

/// [`Storage::transcript_entries_page`] 的一行。
#[derive(Debug, Clone)]
pub struct TranscriptEntryRow {
    /// 全局顺序号。
    pub seq: i64,
    /// 发送方角色。
    pub role: String,
    /// 消息时间。
    pub ts_ms: Option<i64>,
    /// 产出模型（仅末轮响应条目有）。
    pub model: Option<String>,
    /// 片段语义（见 [`NewTranscriptEntry::kind`]）。
    pub kind: i64,
    /// 片段起始字节偏移。
    pub frag_offset: i64,
    /// 片段字节长度。
    pub frag_len: i64,
    /// kind = 1 时：旧 content 数组长度。
    pub skip_blocks: Option<i64>,
    /// 行元数据（见 [`NewTranscriptEntry::frag_meta`]）。
    pub frag_meta: Option<String>,
}

/// 总览聚合的总量部分。字段直接面向仪表盘指标卡；camelCase 由 serde 统一给。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    /// 输入桶总量。
    pub input_tokens: i64,
    /// 输出桶总量（净量，不含推理）。
    pub output_tokens: i64,
    /// 缓存读桶总量。
    pub cache_read_tokens: i64,
    /// 缓存写桶总量。
    pub cache_write_tokens: i64,
    /// 推理量总量（计入输出价）。
    pub reasoning_tokens: i64,
    /// 会话总数。
    pub session_count: i64,
    /// 用量记录总数。
    pub record_count: i64,
    /// 已知成本合计；未知行不参与求和，也不编造 0。
    pub known_cost_micros: i64,
    /// 总分未知的行数；> 0 时前端须标注"部分未知"。
    pub unknown_cost_rows: i64,
}

/// 按模型聚合的一行。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageRow {
    /// 归一化模型 id。
    pub model: String,
    /// 输入桶总量。
    pub input_tokens: i64,
    /// 输出桶总量（净量，不含推理）。
    pub output_tokens: i64,
    /// 缓存读桶总量。
    pub cache_read_tokens: i64,
    /// 缓存写桶总量。
    pub cache_write_tokens: i64,
    /// 推理量总量。
    pub reasoning_tokens: i64,
    /// 已知成本合计。
    pub known_cost_micros: i64,
    /// 总分未知的行数。
    pub unknown_cost_rows: i64,
}

/// [`Storage::overview`] 的返回载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewPayload {
    /// 全局总量。
    pub totals: UsageTotals,
    /// 按模型聚合，已知成本降序。
    pub by_model: Vec<ModelUsageRow>,
}

// ── 仪表盘聚合（总览页）────────────────────────────────────────
// 与 [`Storage::overview`] 的区别：吃一组与明细页同语义的筛选，且一次给出
// 仪表盘全部卡片要用的切片。口径约定：
// - tokens 一律五桶合计（缓存读是子集，不做减法）；
// - 成本一律"已知成本"：未定价行不参与求和，未定价模型数单独给出；
// - 趋势桶起点 / 日历日期都是**本地时区**（SQLite 'localtime'），
//   本地化 label 不在本层生成，由前端按粒度渲染。

/// 总览仪表盘的总量部分。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTotals {
    /// 已知成本合计；未定价行不参与求和。
    pub cost_micros: i64,
    /// 五桶 token 合计。
    pub tokens: i64,
    /// 用量记录数（= 调用次数）。
    pub calls: i64,
    /// 筛选范围内的会话数。
    pub sessions: i64,
    /// 有归属会话的去重项目数（"无项目"不算项目）。
    pub projects: i64,
    /// 缓存读桶总量。
    pub cache_read_tokens: i64,
    /// 活跃时长合计：各会话（首条 − 末条记录时间）之和；无会话的孤儿行不计。
    pub active_duration_ms: i64,
    /// 去重工具数。
    pub active_tools: i64,
}

/// 趋势序列的一桶。`start_ms` 是本地时区对齐的桶起点（epoch ms）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardTrendBucket {
    /// 桶起点（epoch ms，本地时区对齐）。
    pub start_ms: i64,
    /// 桶内已知成本合计（微美元）。
    pub cost_micros: i64,
    /// 桶内 token 合计。
    pub tokens: i64,
    /// 桶内请求条数。
    pub calls: i64,
    /// 桶内缓存读 token 合计。
    pub cache_read_tokens: i64,
    /// 桶内缓存读成本合计（微美元）。
    pub cache_read_cost_micros: i64,
}

/// 按模型拆分的趋势序列：三个数组与 trend 的桶一一对齐（无记录的桶填 0）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardModelSeries {
    /// 标准模型 id。
    pub model: String,
    /// 与 trend 桶对齐的成本数组（微美元）。
    pub cost_micros: Vec<i64>,
    /// 与 trend 桶对齐的 token 数组。
    pub tokens: Vec<i64>,
    /// 与 trend 桶对齐的请求数数组。
    pub calls: Vec<i64>,
}

/// 构成类卡片的一行（模型 / 项目共用）。label 里的 `""` 是"无项目"哨兵，
/// 前端显示为"（无项目）"（与明细页约定一致）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardBreakdownRow {
    /// 模型 id 或项目目录；`""` = 无项目。
    pub label: String,
    /// 已知成本合计（未定价行不计）。
    pub cost_micros: i64,
    /// token 合计。
    pub tokens: i64,
    /// 请求条数。
    pub calls: i64,
    /// 该行含未定价行（成本未知）。
    pub unknown_cost: bool,
}

/// 桑基图的一条链路：工具 → 模型、模型 → 项目两段。项目段的目标为 `""` 即无归属。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardFlowLink {
    /// 起点（工具名或模型 id）。
    pub source: String,
    /// 终点（模型 id 或项目目录）。
    pub target: String,
    /// 该链路已知成本合计（微美元）。
    pub cost_micros: i64,
    /// 该链路 token 合计。
    pub tokens: i64,
    /// 该链路请求条数。
    pub calls: i64,
}

/// 活跃热力图的一格：day 0=周一 … 6=周日 × 小时。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardActivityCell {
    /// 星期几：0=周一 … 6=周日。
    pub day: i64,
    /// 小时（0-23，本地时区）。
    pub hour: i64,
    /// 格内已知成本合计（微美元）。
    pub cost_micros: i64,
    /// 格内 token 合计。
    pub tokens: i64,
    /// 格内请求条数。
    pub calls: i64,
}

/// 日历热力图的一天；date 为本地日期 `YYYY-MM-DD`，只含有数据的日子（升序）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardDailyPoint {
    /// 本地日期 `YYYY-MM-DD`。
    pub date: String,
    /// 当日已知成本合计（微美元）。
    pub cost_micros: i64,
    /// 当日 token 合计。
    pub tokens: i64,
    /// 当日请求条数。
    pub calls: i64,
}

/// 箱线图的一组五数概括：min / Q1 / 中位数 / Q3 / max（线性插值）。
/// 只统计有价行——未定价的"单次开销"画不出来，硬画 0 等于替它估价。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSpread {
    /// 分组标签（模型 id，顺序与 composition 一致）。
    pub label: String,
    /// 有价行的单条成本样本（微美元）。
    pub cost_micros: Vec<i64>,
    /// 各样本对应的 token 数。
    pub tokens: Vec<i64>,
}

/// 费用构成瀑布的分项；分项之和 = 已知总成本（见 [`Storage::dashboard`] 注释）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardCostComposition {
    /// 输入桶成本（微美元）。
    pub input_cost_micros: i64,
    /// 输出桶成本（微美元）。
    pub output_cost_micros: i64,
    /// 推理量按输出价计费，从输出桶里拆出来单列，瀑布上才看得见它占多少。
    pub reasoning_cost_micros: i64,
    /// 缓存读桶成本（微美元）。
    pub cache_read_cost_micros: i64,
    /// 缓存写桶成本（微美元）。
    pub cache_write_cost_micros: i64,
}

/// 会话消耗气泡：一会话（× 模型）一点；全部行未定价的会话没有费用可画，
/// 不在切片里（部分未定价的会话按已知部分计入，与总口径一致）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSessionPoint {
    /// 会话所属工具；external_id 的限定方——自然键是 (tool, external_id)，
    /// 分组与切片都必须带它，否则不同工具的同名 external_id 会被并成一点。
    pub tool: String,
    /// 会话在工具数据库里的原始 id（与会话页同一身份；为"点击跳会话"留门）。
    pub external_id: String,
    /// 会话的项目目录；无项目为 ""（前端换成哨兵文案）。
    pub project_dir: String,
    /// 模型（标准名；一会话跨多模型时按模型拆点）。
    pub model: String,
    /// 该会话（该模型）的调用次数。
    pub calls: i64,
    /// 该会话（该模型）的 token 合计。
    pub tokens: i64,
    /// 该会话（该模型）的已知成本合计（微美元）。
    pub cost_micros: i64,
    /// 该会话首条记录的时间（epoch ms，本地时区由前端格式化）。
    pub start_ms: i64,
    /// 该会话末条记录的时间（epoch ms）。
    pub end_ms: i64,
}

/// [`Storage::dashboard`] 的返回载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardPayload {
    /// 汇总指标。
    pub totals: DashboardTotals,
    /// 时间趋势，按桶起点升序；空数据为空数组。
    pub trend: Vec<DashboardTrendBucket>,
    /// 趋势粒度：`hour` / `day` / `month`，前端据此生成本地化 label。
    pub trend_grain: String,
    /// 按模型拆分的趋势序列。
    pub model_trend: Vec<DashboardModelSeries>,
    /// 按模型构成，已知成本降序。
    pub composition: Vec<DashboardBreakdownRow>,
    /// 按项目构成，已知成本降序；`""` = 无项目。
    pub projects: Vec<DashboardBreakdownRow>,
    /// 桑基链路（工具→模型、模型→项目）。
    pub flow: Vec<DashboardFlowLink>,
    /// 恒 7×24 格（无数据的格补 0）。
    pub activity: Vec<DashboardActivityCell>,
    /// 有数据的日子，日期升序；**不随时间筛选收窄**（日历卡固定窗口），
    /// 但吃工具/模型/项目/搜索筛选。缺的日子由前端补 0。
    pub daily: Vec<DashboardDailyPoint>,
    /// 有价模型的五数概括，顺序与 composition 一致。
    pub spread: Vec<DashboardSpread>,
    /// 费用构成瀑布的分项合计。
    pub cost_composition: DashboardCostComposition,
    /// 有未定价行的模型个数。
    pub unknown_model_count: i64,
    /// 会话消耗气泡切片：一会话（× 模型）一点，按 token 量降序。
    pub sessions: Vec<DashboardSessionPoint>,
}

/// [`Storage::session_info`] 的返回：会话删除编排所需的归属信息。
pub struct SessionInfo {
    /// 会话行 id。
    pub id: i64,
    /// 工具标识。
    pub tool: String,
    /// 工具侧会话 id。
    pub external_id: String,
    /// 首次见到该会话的日志文件（`scanned_files.id`）。
    pub source_file_id: i64,
}

/// 网关侧总览的总量部分。与 [`UsageTotals`] 同构但没有会话维度——网关请求无会话，
/// 且两表口径严格分开（双计问题），不复用类型以防混用。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTotals {
    /// 输入桶总量。
    pub input_tokens: i64,
    /// 输出桶总量（净量，不含推理）。
    pub output_tokens: i64,
    /// 缓存读桶总量。
    pub cache_read_tokens: i64,
    /// 缓存写桶总量。
    pub cache_write_tokens: i64,
    /// 推理量总量（计入输出价）。
    pub reasoning_tokens: i64,
    /// 请求总数。
    pub request_count: i64,
    /// 非成功响应（status_code >= 400）的请求数。
    pub error_count: i64,
    /// 已知成本合计；未知行不参与求和，也不编造 0。
    pub known_cost_micros: i64,
    /// 总分未知的行数；> 0 时前端须标注"部分未知"。
    pub unknown_cost_rows: i64,
}

/// 流量页的一行（`gateway_requests` 原样事实，成本走视图口径）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequestRow {
    /// 行 id。
    pub id: i64,
    /// 请求开始的 epoch ms（UTC）。
    pub ts: i64,
    /// 原始模型名。
    pub model_raw: String,
    /// 归一化/映射后的模型名。
    pub model: String,
    /// 输入桶。
    pub input_tokens: i64,
    /// 输出桶（净量，不含推理）。
    pub output_tokens: i64,
    /// 上游响应状态码；天折在拿到响应前为 `None`。
    pub status_code: Option<i64>,
    /// 请求总耗时（毫秒）。
    pub latency_ms: Option<i64>,
    /// 命中的上游名。
    pub upstream: Option<String>,
    /// 已知成本（视图口径：任一有量桶未定价则为 `None`）。
    pub cost_micros: Option<i64>,
}

/// [`Storage::gateway_requests_page`] 的返回载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequestsPage {
    /// 当页行，按时间倒序。
    pub rows: Vec<GatewayRequestRow>,
    /// 过滤后的总行数（分页用）。
    pub total: i64,
}

/// [`Storage::gateway_overview`] 的返回载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayOverviewPayload {
    /// 全局总量。
    pub totals: GatewayTotals,
    /// 按模型聚合，已知成本降序。
    pub by_model: Vec<ModelUsageRow>,
}

// ── 明细页（usage_records 的服务端筛选/排序/分页）─────────────────

/// 明细页的一组筛选；`None` / 空集合 = 该维度不筛选。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UsageFilters {
    /// 时间下界（含，epoch ms）。
    pub time_start: Option<i64>,
    /// 时间上界（含，epoch ms）。
    pub time_end: Option<i64>,
    /// 工具 id 白名单。
    pub tools: Vec<String>,
    /// 归一化模型 id 白名单。
    pub models: Vec<String>,
    /// 项目目录白名单；`""` 是"无项目"哨兵（含已删除会话的孤儿行，
    /// project_dir IS NULL）——项目不是事实表自带的，它随会话一起消失。
    pub projects: Vec<String>,
    /// 会话 id / 项目目录的包含式搜索词。
    pub search: Option<String>,
}

/// [`Storage::usage_records_page`] 的输入。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UsageRecordsQuery {
    /// 维度筛选（时间/工具/模型/项目/搜索）。
    pub filters: UsageFilters,
    /// 被禁用工具的 id 列表（前端偏好）：禁用工具的记录不出现。
    pub disabled: Vec<String>,
    /// 排序键，白名单见 [`usage_sort_expr`]；`None` = 按请求时间排（方向随 [`Self::sort_desc`]）。
    pub sort_key: Option<String>,
    /// `true` = 降序。
    pub sort_desc: bool,
    /// 分页偏移（页码 × 页大小，由调用方换算）。
    pub offset: i64,
    /// 页大小；上限由调用方约束。
    pub limit: i64,
}

/// 明细页的一行（成本与分桶费用走视图口径）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordRow {
    /// 行 id。
    pub id: i64,
    /// 请求时间（epoch ms，UTC）。
    pub ts: i64,
    /// 工具 id。
    pub tool: String,
    /// 归一化模型 id。
    pub model: String,
    /// 工具侧会话 id；会话已删除则为 `None`。
    pub session_external_id: Option<String>,
    /// 项目目录；会话已删除或会话未记录项目则为 `None`。
    pub project_dir: Option<String>,
    /// 输入桶。
    pub input_tokens: i64,
    /// 输出桶（净量，不含推理）。
    pub output_tokens: i64,
    /// 缓存读桶。
    pub cache_read_tokens: i64,
    /// 缓存写桶。
    pub cache_write_tokens: i64,
    /// 老日志可能没有。
    pub reasoning_tokens: Option<i64>,
    /// 请求耗时（毫秒）；日志没带计时则为 `None`。
    pub duration_ms: Option<i64>,
    /// 总分（视图口径：任一有量桶未定价则为 `None`）。
    pub cost_micros: Option<i64>,
    /// 分桶费用；某桶有量而无单价时该桶 `None`（此时总分也 `None`）。
    /// 输出桶的计价量含 reasoning_tokens（按输出价计费）。
    pub input_cost_micros: Option<i64>,
    /// 输出桶费用。
    pub output_cost_micros: Option<i64>,
    /// 缓存读桶费用。
    pub cache_read_cost_micros: Option<i64>,
    /// 缓存写桶费用。
    pub cache_write_cost_micros: Option<i64>,
}

/// [`Storage::usage_records_page`] 的返回载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsPage {
    /// 当页行。
    pub rows: Vec<UsageRecordRow>,
    /// 筛选后的总行数（分页用）。
    pub total: i64,
}

/// 筛选下拉的一个选项。`value` 为 `None` 只出现在项目维度 = "无项目"。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageFilterOption {
    /// 选项值；项目维度的 `None` = 无项目。
    pub value: Option<String>,
    /// 该值在分面口径（其余维度已筛选）下的记录数。
    pub count: i64,
}

/// [`Storage::usage_filter_options`] 的返回：三个维度的选项与分面计数，
/// 按条数降序。计数随当前筛选即时刷新，但排除禁用工具。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageFilterOptionsPayload {
    /// 工具维度选项。
    pub tools: Vec<UsageFilterOption>,
    /// 归一化模型维度选项。
    pub models: Vec<UsageFilterOption>,
    /// 项目目录维度选项（含无项目）。
    pub projects: Vec<UsageFilterOption>,
}

/// 目录同步的结果（给前端展示）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSyncPayload {
    /// 目录快照的条目数。
    pub entries: usize,
    /// 与本地标准模型匹配上的个数。
    pub matched: usize,
    /// 本次实际写入/刷新的价格行数。
    pub priced: usize,
    /// 反向补映射改指的变种个数（auto/suggested 映射借目录找到了更准的归属）。
    pub remapped: usize,
    /// 本次同步时间（epoch ms）。
    pub synced_at: i64,
}

// ── 定价页（模型目录、价格来源与变种映射）─────────────────────────

/// 定价页一行的标准模型：现价、目录建议价、变种映射与用量。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingModelRow {
    /// 标准模型 id。
    pub id: String,
    /// 展示名（建档时与 id 相同，目录同步后可换社区名）。
    pub display_name: String,
    /// 现价来源（`catalog`/`user`）；无价格行为 `None`。
    pub price_source: Option<String>,
    /// 输入桶现价。
    pub input_per_mtok_micros: Option<i64>,
    /// 输出桶现价。
    pub output_per_mtok_micros: Option<i64>,
    /// 缓存读现价。
    pub cache_read_per_mtok_micros: Option<i64>,
    /// 缓存写现价。
    pub cache_write_per_mtok_micros: Option<i64>,
    /// 目录建议价（收敛到一条后）；目录里没有该模型为 `None`。
    pub catalog: Option<pricing_overview::CatalogHint>,
    /// 该模型的用量条数（明细表口径）。
    pub usage_count: i64,
    /// 指向本标准模型的原始变种名（含未产生用量的）。
    pub variants: Vec<pricing_overview::PricingVariant>,
    /// 变种映射来源的"最弱一环"：任一变种是 suggested 即 `suggested`，否则有
    /// auto 即 `auto`，全 user 才是 `user`；无变种的模型为 `None`。页面据此
    /// 区分已确认 / 待确认——非 user 映射都是匹配成功的默认归并，待用户过目。
    pub mapping_source: Option<String>,
}

/// models.dev 目录候选的一行（每模型收敛一条，规则与建议价同）：标准名改名
/// 下拉框的数据源。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModelBrief {
    /// 目录模型 id（改名的目标标准名）。
    pub model_id: String,
    /// 供应商标识（同名候选收敛时提示来源用）。
    pub provider: String,
    /// 目录展示名。
    pub display_name: Option<String>,
    /// `active` / `deprecated`。
    pub status: Option<String>,
    /// 输入桶建议价（复制价格选单展示与套用用）。
    pub input_per_mtok_micros: Option<i64>,
    /// 输出桶建议价。
    pub output_per_mtok_micros: Option<i64>,
    /// 缓存读桶建议价。
    pub cache_read_per_mtok_micros: Option<i64>,
    /// 缓存写桶建议价。
    pub cache_write_per_mtok_micros: Option<i64>,
}

/// 定价页载荷。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PricingOverviewPayload {
    /// 目录最近一次同步时间；从未同步为 `None`。
    pub synced_at: Option<i64>,
    /// 全部标准模型（有用量的在前）。
    pub models: Vec<PricingModelRow>,
    /// 目录候选（按 id 排序）。
    pub catalog_models: Vec<CatalogModelBrief>,
}

// ── 会话页（sessions 的服务端筛选/分页 + 用量聚合）─────────────────

/// 会话页的一组筛选；`None` / 空集合 = 该维度不筛选。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionFilters {
    /// 最近活跃时间的下界（含，epoch ms）。
    pub time_start: Option<i64>,
    /// 最近活跃时间的上界（含，epoch ms）。
    pub time_end: Option<i64>,
    /// 工具 id 白名单。
    pub tools: Vec<String>,
    /// 标题 / 会话 id / 项目目录的包含式搜索词。
    pub search: Option<String>,
}

/// [`Storage::sessions_page`] 的输入。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SessionsPageQuery {
    /// 维度筛选（时间/工具/搜索）。
    pub filters: SessionFilters,
    /// 被禁用工具的 id 列表（前端偏好）：禁用工具的会话不出现。
    pub disabled: Vec<String>,
    /// 排序键，白名单见 [`sessions_sort_expr`]；`None` = 按最近活跃排。
    pub sort_key: Option<String>,
    /// `true` = 降序。
    pub sort_desc: bool,
    /// 分页偏移（页码 × 页大小，由调用方换算）。
    pub offset: i64,
    /// 页大小；上限由调用方约束。
    pub limit: i64,
}

/// 会话页的一行：会话事实 + 该会话全部用量记录的聚合（成本不在会话维度展示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    /// 行 id（删除会话等命令的参数）。
    pub id: i64,
    /// 工具 id。
    pub tool: String,
    /// 工具侧会话 id（转录命令的定位键之一）。
    pub external_id: String,
    /// 标题（首条用户消息 / 工具自带）；还没有则 `None`。
    pub title: Option<String>,
    /// 项目目录；会话未记录则为 `None`。
    pub project_dir: Option<String>,
    /// 最近活跃时间（epoch ms，UTC）。
    pub last_activity_at: i64,
    /// 该会话的用量记录数（≈ 请求轮次）。
    pub request_count: i64,
    /// 输入桶合计。
    pub input_tokens: i64,
    /// 输出桶合计（净量，不含推理）。
    pub output_tokens: i64,
    /// 缓存读桶合计。
    pub cache_read_tokens: i64,
    /// 缓存写桶合计。
    pub cache_write_tokens: i64,
}

/// [`Storage::sessions_page`] 的返回载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsPage {
    /// 当页行。
    pub rows: Vec<SessionRow>,
    /// 筛选后的总会话数（分页用）。
    pub total: i64,
}

/// 打开（或创建）主库：建父目录、施加 PRAGMA、应用未运行的迁移。
///
/// `db_path` 应来自 `paths::main_db_path()`——文件名的单一事实源在那里。
pub fn open(db_path: &Path) -> Result<Storage> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::DataFile {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let conn = Connection::open(db_path)?;
    conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    // journal_mode 的 PRAGMA 会返回一行结果，execute_batch 对此有容错。
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;",
    )?;

    let storage = Storage { conn };
    storage.migrate()?;
    Ok(storage)
}

impl Storage {
    /// 仅测试：扫描管道等跨模块测试需要直接断言库内容，不想为此放宽字段封装。
    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 依序应用未运行的迁移；每个迁移连同 `user_version` 在独立事务里提交，
    /// 中断后重开从断点续跑，已提交的不会重复执行。
    ///
    /// 事务用 BEGIN IMMEDIATE 且**拿到写锁后重读** user_version：两个连接（如首次
    /// 计量落库与壳命令并发）在同一个新库上同时开迁移时，后到者会先等写锁、再发现
    /// 迁移已被跑完——否则双方都按旧版本号执行 v1，后者撞 "table already exists"。
    fn migrate(&self) -> Result<()> {
        'pending: loop {
            let current: i64 = self
                .conn
                .query_row("PRAGMA user_version", [], |row| row.get(0))?;
            for (idx, sql) in migrations::MIGRATIONS.iter().enumerate() {
                let version = (idx + 1) as i64;
                if version <= current {
                    continue;
                }
                self.conn.execute_batch("BEGIN IMMEDIATE")?;
                // 拿到写锁后版本号可能已被并发的迁移推进，重读后再决定。
                let locked: i64 = self
                    .conn
                    .query_row("PRAGMA user_version", [], |row| row.get(0))?;
                if version <= locked {
                    self.conn.execute_batch("COMMIT")?;
                    continue 'pending;
                }
                self.conn.execute_batch(sql)?;
                self.conn.pragma_update(None, "user_version", version)?;
                self.conn.execute_batch("COMMIT")?;
            }
            return Ok(());
        }
    }

    /// 登记一个扫描到的日志文件；已存在（按规范化路径）则更新簿记并清除"消失"标记。
    ///
    /// 返回该文件的行 id。`parsed_bytes` 是增量游标——文件被轮转/重写后由扫描层
    /// 重置为 0 并整体重解析，去重交给 [`Storage::insert_usage_record`]。
    pub fn upsert_scanned_file(
        &self,
        path: &str,
        tool: &str,
        content_hash: &[u8],
        size: i64,
        parsed_bytes: i64,
        now: i64,
    ) -> Result<i64> {
        self.conn.query_row(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                 tool = excluded.tool,
                 content_hash = excluded.content_hash,
                 size = excluded.size,
                 parsed_bytes = excluded.parsed_bytes,
                 missing_since = NULL,
                 updated_at = excluded.updated_at
             RETURNING id",
            params![path, tool, content_hash, size, parsed_bytes, now],
            |row| row.get(0),
        )
        .map_err(|err| err.into())
    }

    /// 标记（或清除）文件消失。扫描层发现文件不存在时置时间戳；**用量数据保留
    /// 不动**——用户自行删日志是真实发生的历史，与"经 Toktol 删除会话"是两条
    /// 语义。
    ///
    /// 置 missing 的同时清掉该文件的转录索引：索引条目是源文件里的**字节区间**，
    /// 源没了就永远无法解引用，而转录读取路径在解引用之前就会报
    /// `core.source_gone`，留着只是死重。索引是纯缓存：文件若失而复得，状态行
    /// 已不在，读取路径自然按未建站从头重建。对没建过索引的文件是空操作。
    pub fn set_file_missing(&self, file_id: i64, missing_since: Option<i64>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE scanned_files SET missing_since = ?2 WHERE id = ?1",
            params![file_id, missing_since],
        )?;
        if missing_since.is_some() {
            tx.execute(
                "DELETE FROM transcript_entries WHERE file_id = ?1",
                params![file_id],
            )?;
            tx.execute(
                "DELETE FROM transcript_index_state WHERE file_id = ?1",
                params![file_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 读取扫描簿记（游标与内容指纹）；从未见过该文件则 `None`。
    pub fn scanned_file_state(&self, path: &str) -> Result<Option<ScannedFileState>> {
        self.conn
            .query_row(
                "SELECT id, parsed_bytes, content_hash FROM scanned_files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(ScannedFileState {
                        id: row.get(0)?,
                        parsed_bytes: row.get(1)?,
                        content_hash: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 某工具全部已登记文件的 (id, 路径)，供扫描层做"本轮没见到 → 标记消失"的对账。
    pub fn scanned_file_paths(&self, tool: &str) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM scanned_files WHERE tool = ?1")?;
        let rows = stmt
            .query_map(params![tool], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 转录索引的建站状态；从未建过则 `None`。
    pub fn transcript_index_state(&self, file_id: i64) -> Result<Option<TranscriptIndexState>> {
        self.conn
            .query_row(
                "SELECT built_offset, size, mtime_ms, complete,
                        baseline_offset, baseline_len,
                        pending_offset, pending_len, pending_ts, pending_model
                 FROM transcript_index_state WHERE file_id = ?1",
                params![file_id],
                |row| {
                    Ok(TranscriptIndexState {
                        built_offset: row.get(0)?,
                        size: row.get(1)?,
                        mtime_ms: row.get(2)?,
                        complete: row.get::<_, i64>(3)? != 0,
                        baseline_offset: row.get(4)?,
                        baseline_len: row.get(5)?,
                        pending_offset: row.get(6)?,
                        pending_len: row.get(7)?,
                        pending_ts: row.get(8)?,
                        pending_model: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 清空某源文件的转录索引（全新建站 / 指纹失效重建前调用）。
    pub fn reset_transcript_index(&self, file_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transcript_entries WHERE file_id = ?1",
            params![file_id],
        )?;
        self.conn.execute(
            "DELETE FROM transcript_index_state WHERE file_id = ?1",
            params![file_id],
        )?;
        Ok(())
    }

    /// 撤掉一行已提交的转录条目（增量续建时撤销"建完时物化的末轮响应"用，
    /// 见 zcode index 的 append 续建）。
    pub fn delete_transcript_entry(&self, file_id: i64, seq: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transcript_entries WHERE file_id = ?1 AND seq = ?2",
            params![file_id, seq],
        )?;
        Ok(())
    }

    /// 提交一次建站检查点：新增条目行 + 建站状态，单事务落库。
    /// 已提交的行保留（续建从 built_offset 继续），条目 seq 必须全局递增。
    pub fn commit_transcript_index(
        &self,
        file_id: i64,
        entries: &[NewTranscriptEntry],
        state: &TranscriptIndexState,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO transcript_entries
                     (file_id, seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for entry in entries {
                stmt.execute(params![
                    file_id,
                    entry.seq,
                    entry.role,
                    entry.ts_ms,
                    entry.model,
                    entry.kind,
                    entry.frag_offset,
                    entry.frag_len,
                    entry.skip_blocks,
                    entry.frag_meta,
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO transcript_index_state
                 (file_id, built_offset, size, mtime_ms, complete,
                  baseline_offset, baseline_len, pending_offset, pending_len,
                  pending_ts, pending_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(file_id) DO UPDATE SET
                 built_offset = excluded.built_offset,
                 size = excluded.size,
                 mtime_ms = excluded.mtime_ms,
                 complete = excluded.complete,
                 baseline_offset = excluded.baseline_offset,
                 baseline_len = excluded.baseline_len,
                 pending_offset = excluded.pending_offset,
                 pending_len = excluded.pending_len,
                 pending_ts = excluded.pending_ts,
                 pending_model = excluded.pending_model",
            params![
                file_id,
                state.built_offset,
                state.size,
                state.mtime_ms,
                state.complete as i64,
                state.baseline_offset,
                state.baseline_len,
                state.pending_offset,
                state.pending_len,
                state.pending_ts,
                state.pending_model,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 按序取一页转录条目：`seq > after_seq` 的前 `limit` 行（升序）。
    pub fn transcript_entries_page(
        &self,
        file_id: i64,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<TranscriptEntryRow>> {
        self.transcript_entries_page_impl(file_id, after_seq, limit, false)
    }

    /// 按序取一页转录条目：`seq < before_seq` 的**末** `limit` 行（升序返回），
    /// 供窗口向前加载。
    pub fn transcript_entries_before(
        &self,
        file_id: i64,
        before_seq: i64,
        limit: i64,
    ) -> Result<Vec<TranscriptEntryRow>> {
        self.transcript_entries_page_impl(file_id, before_seq, limit, true)
    }

    fn transcript_entries_page_impl(
        &self,
        file_id: i64,
        anchor: i64,
        limit: i64,
        before: bool,
    ) -> Result<Vec<TranscriptEntryRow>> {
        let sql = if before {
            "SELECT seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta
             FROM transcript_entries WHERE file_id = ?1 AND seq < ?2
             ORDER BY seq DESC LIMIT ?3"
        } else {
            "SELECT seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta
             FROM transcript_entries WHERE file_id = ?1 AND seq > ?2
             ORDER BY seq ASC LIMIT ?3"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt
            .query_map(params![file_id, anchor, limit], |row| {
                Ok(TranscriptEntryRow {
                    seq: row.get(0)?,
                    role: row.get(1)?,
                    ts_ms: row.get(2)?,
                    model: row.get(3)?,
                    kind: row.get(4)?,
                    frag_offset: row.get(5)?,
                    frag_len: row.get(6)?,
                    skip_blocks: row.get(7)?,
                    frag_meta: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if before {
            rows.reverse();
        }
        Ok(rows)
    }

    /// 转录条目总数（seq 可能因读取时空块过滤出现空洞，总数以行数为准）。
    pub fn transcript_entry_count(&self, file_id: i64) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_entries WHERE file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// 目录（轮次列表）的候选行：user 角色的消息片段（压缩摘要行的判定
    /// 需要解析片段内容，在调用方做）。
    pub fn transcript_turn_candidates(&self, file_id: i64) -> Result<Vec<TranscriptEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta
             FROM transcript_entries WHERE file_id = ?1 AND role = 'user' AND kind = 0
             ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![file_id], |row| {
                Ok(TranscriptEntryRow {
                    seq: row.get(0)?,
                    role: row.get(1)?,
                    ts_ms: row.get(2)?,
                    model: row.get(3)?,
                    kind: row.get(4)?,
                    frag_offset: row.get(5)?,
                    frag_len: row.get(6)?,
                    skip_blocks: row.get(7)?,
                    frag_meta: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 会话删除编排所需的会话信息；不存在则 `None`。
    pub fn session_info(&self, session_id: i64) -> Result<Option<SessionInfo>> {
        self.conn
            .query_row(
                "SELECT id, tool, external_id, source_file_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(SessionInfo {
                        id: row.get(0)?,
                        tool: row.get(1)?,
                        external_id: row.get(2)?,
                        source_file_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 按 (tool, external_id) 定位会话（会话详情视图只拿到这对自然键）；
    /// 不存在则 `None`。
    pub fn session_info_by_external(
        &self,
        tool: &str,
        external_id: &str,
    ) -> Result<Option<SessionInfo>> {
        self.conn
            .query_row(
                "SELECT id, tool, external_id, source_file_id FROM sessions
                 WHERE tool = ?1 AND external_id = ?2",
                params![tool, external_id],
                |row| {
                    Ok(SessionInfo {
                        id: row.get(0)?,
                        tool: row.get(1)?,
                        external_id: row.get(2)?,
                        source_file_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 簿记里登记的文件路径；行不存在则 `None`。
    pub fn scanned_file_path(&self, file_id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT path FROM scanned_files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 引用某源文件的会话数：删除编排靠它判断文件是否可以进回收站。
    pub fn count_sessions_for_file(&self, file_id: i64) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE source_file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// 取回会话；不存在则创建。返回 (会话 id, 是否本次新建)。
    /// 项目目录**缺补不覆写**：老库里的会话可能建于适配器尚无项目事实的时期
    /// （如 workbuddy/zcode），重扫同一行时把 NULL 补上，已有的不漂移——
    /// 同一会话不会换项目，首见与补见只会其一。
    pub fn get_or_create_session(&self, session: &NewSession, now: i64) -> Result<(i64, bool)> {
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO sessions
                 (tool, external_id, external_id_is_derived, title, project_dir,
                  source_file_id, first_seen_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                session.tool,
                session.external_id,
                session.external_id_is_derived,
                session.title,
                session.project_dir,
                session.source_file_id,
                now
            ],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE tool = ?1 AND external_id = ?2",
            params![session.tool, session.external_id],
            |row| row.get(0),
        )?;
        if inserted == 0
            && let Some(dir) = &session.project_dir
        {
            tx.execute(
                "UPDATE sessions SET project_dir = ?2 WHERE id = ?1 AND project_dir IS NULL",
                params![id, dir],
            )?;
        }
        tx.commit()?;
        Ok((id, inserted > 0))
    }

    /// 更新会话活跃时间（取更晚者）与标题；会话不存在时静默无事——扫描层总是先建会话。
    /// 标题**首见为准**：适配器给的首条标题是稳定的（如首条用户消息），后续重扫
    /// 不会把标题漂移成更晚的消息。
    pub fn update_session_activity(
        &self,
        session_id: i64,
        ts: i64,
        title: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions
             SET last_activity_at = MAX(last_activity_at, ?2),
                 title = COALESCE(title, ?3)
             WHERE id = ?1",
            params![session_id, ts, title],
        )?;
        Ok(())
    }

    /// 新建会话的初始活动时间：直接落定为事实时间戳。backfill 场景（db 源
    /// 首次入账被工具回收过日志的老会话）里，建行的 `now` 会把老会话在列表
    /// 里顶成"今天"；后续更新仍走 [`Self::update_session_activity`] 的 MAX
    /// 语义，只增不减。
    pub fn init_session_activity(&self, session_id: i64, ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET last_activity_at = ?2 WHERE id = ?1",
            params![session_id, ts],
        )?;
        Ok(())
    }

    /// 删除会话（用户显式触发）。日志文件进回收站由扫描层负责，本函数只管库。
    ///
    /// 产品需求：**用量必须保留**——`usage_records.session_id` 置 NULL、`sessions` 行
    /// 删除；`scanned_files` 行仅在没有其它会话引用时删。墓碑由会话删除编排层
    /// 落（[`crate::sessions`]），本函数不重复记。
    pub fn delete_session(&self, session_id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let source_file_id: Option<i64> = tx
            .query_row(
                "SELECT source_file_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;

        tx.execute(
            "UPDATE usage_records SET session_id = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;

        if let Some(file_id) = source_file_id {
            let refs: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sessions WHERE source_file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )?;
            if refs == 0 {
                tx.execute("DELETE FROM scanned_files WHERE id = ?1", params![file_id])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// 记已删会话的墓碑：扫描层建行前查此表，共享日志轮转重解析不再把已删
    /// 会话重建为 0 用量空壳。重复删除幂等（INSERT OR IGNORE）。
    pub fn record_deleted_session(&self, tool: &str, external_id: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO deleted_sessions (tool, external_id, deleted_at)
             VALUES (?1, ?2, ?3)",
            params![tool, external_id, now],
        )?;
        Ok(())
    }

    /// 会话是否已被用户删除（墓碑在册）。
    pub fn is_session_deleted(&self, tool: &str, external_id: &str) -> Result<bool> {
        let hit: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM deleted_sessions WHERE tool = ?1 AND external_id = ?2",
            params![tool, external_id],
            |row| row.get(0),
        )?;
        Ok(hit > 0)
    }

    /// 插入一条用量明细；返回是否真的插入（`false` = 去重键命中，重复入账被拦）。
    /// 单事务内顺带完成模型建档与 auto 映射登记。映射是**全局**的（v5 起）：
    /// 同一原始变种名在所有工具下指向同一个标准模型。建议值先过
    /// [`crate::pricing::canonicalize_model_id`]（小写、去前缀），修正归属是
    /// 定价页上用户确认的事。只记 token 事实，不算成本——计价的唯一入口是
    /// 重算管线。
    pub fn insert_usage_record(&self, record: &NewUsageRecord) -> Result<bool> {
        let tx = self.conn.unchecked_transaction()?;

        let existing: Option<String> = tx
            .query_row(
                "SELECT model_id FROM model_mappings WHERE raw_model = ?1",
                params![record.model_raw],
                |row| row.get(0),
            )
            .optional()?;

        let model = match existing {
            Some(mapped) => mapped,
            None => {
                let canonical = crate::pricing::canonicalize_model_id(&record.model);
                let (suggestion, source) = self.resolve_suggestion(&canonical);
                tx.execute(
                    "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
                     VALUES (?1, ?1, ?2)",
                    params![suggestion, record.ts],
                )?;
                // 只登记不更新：修正既有映射是 set_model_mapping（user）的事。
                tx.execute(
                    "INSERT OR IGNORE INTO model_mappings
                         (raw_model, model_id, source, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![record.model_raw, suggestion, source, record.ts],
                )?;
                suggestion
            }
        };

        let inserted = tx.execute(
            "INSERT OR IGNORE INTO usage_records
                 (session_id, tool, model_raw, model, ts,
                  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                  reasoning_tokens, duration_ms, request_count, dedup_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                record.session_id,
                record.tool,
                record.model_raw,
                model,
                record.ts,
                record.input_tokens,
                record.output_tokens,
                record.cache_read_tokens,
                record.cache_write_tokens,
                record.reasoning_tokens,
                record.duration_ms,
                record.request_count,
                record.dedup_key,
            ],
        )?;
        tx.commit()?;
        Ok(inserted > 0)
    }

    /// 插入一条网关请求计量，同一事务内建档并落分项成本，返回行 id。
    ///
    /// 计价走 reprice 管线（对两张事实表各跑一遍），实现仍然只有一份——这是
    /// "写请求记录、查定价、落成本单事务"（见
    /// `paths::main_db_path` 的单库理由）同时不出现第二套计价代码的唯一方式。
    pub fn insert_gateway_request(&self, request: &NewGatewayRequest) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;

        let model = match &request.tool {
            Some(_) => {
                let existing: Option<String> = tx
                    .query_row(
                        "SELECT model_id FROM model_mappings WHERE raw_model = ?1",
                        params![request.model_raw],
                        |row| row.get(0),
                    )
                    .optional()?;
                match existing {
                    Some(mapped) => mapped,
                    None => {
                        let canonical = crate::pricing::canonicalize_model_id(&request.model);
                        let (suggestion, source) = self.resolve_suggestion(&canonical);
                        tx.execute(
                            "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
                             VALUES (?1, ?1, ?2)",
                            params![suggestion, request.ts],
                        )?;
                        tx.execute(
                            "INSERT OR IGNORE INTO model_mappings
                                 (raw_model, model_id, source, updated_at)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![request.model_raw, suggestion, source, request.ts],
                        )?;
                        suggestion
                    }
                }
            }
            // 无发起方就没有映射键：只建档，建议值直接生效。
            None => {
                let canonical = crate::pricing::canonicalize_model_id(&request.model);
                let (suggestion, _) = self.resolve_suggestion(&canonical);
                tx.execute(
                    "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
                     VALUES (?1, ?1, ?2)",
                    params![suggestion, request.ts],
                )?;
                suggestion
            }
        };

        tx.execute(
            "INSERT INTO gateway_requests
                 (tool, model_raw, model, ts,
                  input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                  reasoning_tokens, status_code, latency_ms, upstream, token_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                request.tool,
                request.model_raw,
                model,
                request.ts,
                request.input_tokens,
                request.output_tokens,
                request.cache_read_tokens,
                request.cache_write_tokens,
                request.reasoning_tokens,
                request.status_code,
                request.latency_ms,
                request.upstream,
                request.token_hash,
            ],
        )?;
        let id = tx.last_insert_rowid();
        // 网关逐条入库是高频路径：只重算这一个模型的行，不做全库陪跑。
        reprice_scoped(&tx, Some(&[model]))?;
        tx.commit()?;
        Ok(id)
    }

    /// 登记**用户**修正的归一化映射（`source='user'`，永远覆盖 auto）。映射是
    /// 全局的：同一原始变种名在所有工具下指向同一个标准模型。只写映射表；
    /// 事实表改写与计价走 [`Storage::apply_model_mapping_changes`]。
    pub fn upsert_model_mapping(&self, raw_model: &str, model_id: &str, now: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
             VALUES (?1, ?1, ?2)",
            params![model_id, now],
        )?;
        tx.execute(
            "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
             VALUES (?1, ?2, 'user', ?3)
             ON CONFLICT(raw_model) DO UPDATE SET
                 model_id = excluded.model_id,
                 source = 'user',
                 updated_at = excluded.updated_at",
            params![raw_model, model_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 应用映射变更：把 `changed` 中各原始变种名的用量明细改写到映射的新模型
    /// （映射是全局的，两张事实表一起改写），同一事务内重算成本。映射缺失的
    /// 变种（不应发生）被跳过而不是写 NULL。`gateway_requests` 里 `tool` 为 NULL
    /// 的行没有映射键，不参与改写（其 model 在入库时已定，重算按 model 落价）。
    pub fn apply_model_mapping_changes(&self, changed: &[&str]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut updated = 0;
        for raw in changed {
            for table in FACT_TABLES {
                updated += tx.execute(
                    &format!(
                        "UPDATE {table}
                         SET model = (SELECT m.model_id FROM model_mappings AS m
                                      WHERE m.raw_model = ?1)
                         WHERE model_raw = ?1
                           AND EXISTS (SELECT 1 FROM model_mappings AS m
                                       WHERE m.raw_model = ?1)"
                    ),
                    params![raw],
                )?;
            }
        }
        // 受影响范围 = 这些变种改写后指向的映射目标；旧目标留在原地的行不动。
        let targets: Vec<String> = {
            let marks = vec!["?"; changed.len()].join(",");
            let mut stmt = tx.prepare(&format!(
                "SELECT DISTINCT model_id FROM model_mappings WHERE raw_model IN ({marks})"
            ))?;
            stmt.query_map(params_from_iter(changed.iter()), |row| row.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        updated += reprice_scoped(&tx, Some(&targets))?;
        tx.commit()?;
        Ok(updated)
    }

    /// 按当前价格表全量重算分项成本（快照语义 = 重算时点的价格）。
    /// 手动全量重算入口；单模型的增量重算走各写路径内的 [`reprice_scoped`]。
    pub fn recompute_costs(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let updated = reprice(&tx)?;
        tx.commit()?;
        Ok(updated)
    }

    /// 整份替换 models.dev 目录快照（同步是"全量拉、整份换"，不做增量）。
    pub fn replace_catalog_entries(
        &self,
        entries: &[crate::pricing::catalog::CatalogEntry],
        now: i64,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM catalog_entries", [])?;
        for entry in entries {
            tx.execute(
                "INSERT INTO catalog_entries
                     (provider, model_id, display_name, status,
                      input_per_mtok_micros, output_per_mtok_micros,
                      cache_read_per_mtok_micros, cache_write_per_mtok_micros,
                      reasoning_per_mtok_micros, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    entry.provider,
                    entry.model_id,
                    entry.display_name,
                    entry.status,
                    entry.input_per_mtok_micros,
                    entry.output_per_mtok_micros,
                    entry.cache_read_per_mtok_micros,
                    entry.cache_write_per_mtok_micros,
                    entry.reasoning_per_mtok_micros,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 目录最近一次同步时间。
    pub fn catalog_synced_at(&self) -> Result<Option<i64>> {
        self.conn
            .query_row("SELECT MAX(fetched_at) FROM catalog_entries", [], |row| {
                row.get(0)
            })
            .map_err(Into::into)
    }

    /// 目录同步的完整落库流程：整份换快照 → 按三级来源挂价 → 自动重算。
    /// 解析在调用方（壳命令）完成，这里只管写入与聚合结果。
    pub fn apply_catalog(
        &self,
        entries: &[crate::pricing::catalog::CatalogEntry],
    ) -> Result<CatalogSyncPayload> {
        let now = now_ms();
        self.replace_catalog_entries(entries, now)?;
        let (matched, priced, remapped) = self.sync_catalog_prices()?;
        Ok(CatalogSyncPayload {
            entries: entries.len(),
            matched,
            priced,
            remapped,
            synced_at: now,
        })
    }

    /// 把目录价挂到标准模型上。两级来源 `catalog < user`：只补缺和刷新
    /// `catalog` 来源的行；`user` 是用户改过的价，目录永不覆盖。
    /// deprecated 模型照常挂价（历史用量同样要算钱）。
    /// 同一 model_id 多供应商都有价时取确定的一条：在售优先，再按供应商名字典序。
    /// 挂价前先做**反向补映射**：auto/suggested 映射指到的标准模型在目录里没有、
    /// 但变种名重新解析（归一 → 显示名 → 启发候选）能在目录命中时，改指过去并
    /// 改写事实表——user 映射永不参与。
    /// 返回 (匹配到的标准模型数, 实际写价/刷价次数, 补映射改指数)；末尾自动重算。
    pub fn sync_catalog_prices(&self) -> Result<(usize, usize, usize)> {
        // 目录侧先收敛成 model_id → 一条（见函数注释的选取规则）。
        let mut best: HashMap<String, crate::pricing::catalog::CatalogEntry> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT provider, model_id, display_name, status,
                    input_per_mtok_micros, output_per_mtok_micros,
                    cache_read_per_mtok_micros, cache_write_per_mtok_micros,
                    reasoning_per_mtok_micros
             FROM (SELECT *, ROW_NUMBER() OVER (
                           PARTITION BY model_id
                           ORDER BY (status = 'deprecated') ASC, provider ASC) AS rn
                   FROM catalog_entries)
             WHERE rn = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(crate::pricing::catalog::CatalogEntry {
                provider: row.get(0)?,
                model_id: row.get(1)?,
                display_name: row.get(2)?,
                status: row.get(3)?,
                input_per_mtok_micros: row.get(4)?,
                output_per_mtok_micros: row.get(5)?,
                cache_read_per_mtok_micros: row.get(6)?,
                cache_write_per_mtok_micros: row.get(7)?,
                reasoning_per_mtok_micros: row.get(8)?,
            })
        })?;
        for entry in rows {
            let entry = entry?;
            best.insert(entry.model_id.clone(), entry);
        }

        let now = now_ms();
        let tx = self.conn.unchecked_transaction()?;
        // 受影响模型清单：改指的新目标 + 挂价/刷价的模型；末尾只重算这些。
        let mut repriced_models: Vec<String> = Vec::new();

        // 反向补映射：只看非 user 映射。显示名索引用目录收敛结果建
        // （display_name 归一 → 目录 model_id），与挂价同一条选取规则。
        let mut remapped = 0usize;
        {
            let mut by_display: HashMap<String, String> = HashMap::new();
            for entry in best.values() {
                if let Some(name) = &entry.display_name
                    && !name.is_empty()
                {
                    by_display
                        .entry(crate::pricing::canonicalize_model_id(name))
                        .or_insert_with(|| entry.model_id.clone());
                }
            }
            let mappings: Vec<(String, String)> = {
                let mut stmt = tx.prepare(
                    "SELECT raw_model, model_id FROM model_mappings
                         WHERE source IN ('auto', 'suggested')",
                )?;
                stmt.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for (raw, model_id) in mappings {
                // 用户挂过价的目标是用户意志：哪怕指向看起来不对也不动。
                let user_priced = tx
                    .query_row(
                        "SELECT 1 FROM model_prices WHERE model_id = ?1 AND source = 'user'",
                        params![model_id],
                        |_| Ok(()),
                    )
                    .is_ok();
                if user_priced {
                    continue;
                }
                let canonical = crate::pricing::canonicalize_model_id(&raw);
                // 优先级：已在目录里 → 不动；遗留自映射壳（raw → 自己，老版本
                // 建档的产物）→ 格式归一名必更优，无需目录证据；其余按
                // 归一 / 显示名 / 启发候选在目录里找，目录没收录时剥到最深
                // 母型号（产品决策，标 suggested 待用户过目）。
                let target = if best.contains_key(&model_id) {
                    None
                } else if (model_id == raw && canonical != model_id)
                    || best.contains_key(&canonical)
                {
                    // 自映射壳：格式归一名必更优，无需目录证据；或归一名在目录命中。
                    Some((canonical.clone(), "auto"))
                } else if let Some(id) = by_display.get(&canonical) {
                    Some((id.clone(), "auto"))
                } else {
                    let candidates = crate::pricing::heuristic_candidates(&canonical);
                    candidates
                        .iter()
                        .find(|candidate| best.contains_key(*candidate))
                        .or_else(|| candidates.last())
                        .map(|candidate| (candidate.clone(), "suggested"))
                };
                if let Some((new_id, source)) = target
                    && new_id != model_id
                {
                    repoint_mapping_tx(&tx, &raw, &new_id, source, now)?;
                    repriced_models.push(new_id);
                    remapped += 1;
                }
            }
            delete_orphan_models(&tx)?;
        }

        let model_ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM models")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut matched = 0;
        let mut priced = 0;
        for model_id in &model_ids {
            let Some(entry) = best.get(model_id) else {
                continue;
            };
            matched += 1;
            let has_any_price = [
                entry.input_per_mtok_micros,
                entry.output_per_mtok_micros,
                entry.cache_read_per_mtok_micros,
                entry.cache_write_per_mtok_micros,
            ]
            .iter()
            .any(|v| v.is_some());
            if !has_any_price {
                continue; // 目录也没价：快照里有它，但绝不估价。
            }
            let source: Option<String> = tx
                .query_row(
                    "SELECT source FROM model_prices WHERE model_id = ?1",
                    params![model_id],
                    |row| row.get(0),
                )
                .optional()?;
            match source.as_deref() {
                None => {
                    priced += tx.execute(
                        "INSERT INTO model_prices
                             (model_id, input_per_mtok_micros, output_per_mtok_micros,
                              cache_read_per_mtok_micros, cache_write_per_mtok_micros, source)
                         VALUES (?1, ?2, ?3, ?4, ?5, 'catalog')",
                        params![
                            model_id,
                            entry.input_per_mtok_micros,
                            entry.output_per_mtok_micros,
                            entry.cache_read_per_mtok_micros,
                            entry.cache_write_per_mtok_micros
                        ],
                    )?;
                    repriced_models.push(model_id.clone());
                }
                Some("catalog") => {
                    priced += tx.execute(
                        "UPDATE model_prices
                         SET input_per_mtok_micros = ?2, output_per_mtok_micros = ?3,
                             cache_read_per_mtok_micros = ?4, cache_write_per_mtok_micros = ?5
                         WHERE model_id = ?1",
                        params![
                            model_id,
                            entry.input_per_mtok_micros,
                            entry.output_per_mtok_micros,
                            entry.cache_read_per_mtok_micros,
                            entry.cache_write_per_mtok_micros
                        ],
                    )?;
                    repriced_models.push(model_id.clone());
                }
                // user：目录不覆盖。
                Some(_) => {}
            }
        }
        // 挂价/刷价/改指可能改变成本：只重算受影响模型，页面无需再触发。
        reprice_scoped(&tx, Some(&repriced_models))?;
        tx.commit()?;
        Ok((matched, priced, remapped))
    }

    /// 定价页：读路径聚合。目录侧与 [`Self::sync_catalog_prices`] 用同一条
    /// 收敛规则（在售优先、供应商字典序），页面上的建议价与实际挂价一致。
    pub fn pricing_overview(&self) -> Result<PricingOverviewPayload> {
        let mut catalog: HashMap<String, pricing_overview::CatalogHint> = HashMap::new();
        let mut catalog_models: Vec<CatalogModelBrief> = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT provider, model_id, display_name, status,
                        input_per_mtok_micros, output_per_mtok_micros,
                        cache_read_per_mtok_micros, cache_write_per_mtok_micros
                 FROM (SELECT *, ROW_NUMBER() OVER (
                               PARTITION BY model_id
                               ORDER BY (status = 'deprecated') ASC, provider ASC) AS rn
                       FROM catalog_entries)
                 WHERE rn = 1",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    pricing_overview::CatalogHint {
                        provider: row.get(0)?,
                        model_id: row.get(1)?,
                        display_name: row.get(2)?,
                        status: row.get(3)?,
                        input_per_mtok_micros: row.get(4)?,
                        output_per_mtok_micros: row.get(5)?,
                        cache_read_per_mtok_micros: row.get(6)?,
                        cache_write_per_mtok_micros: row.get(7)?,
                    },
                ))
            })?;
            for row in rows {
                let (model_id, hint) = row?;
                catalog_models.push(CatalogModelBrief {
                    model_id: model_id.clone(),
                    provider: hint.provider.clone(),
                    display_name: hint.display_name.clone(),
                    status: hint.status.clone(),
                    input_per_mtok_micros: hint.input_per_mtok_micros,
                    output_per_mtok_micros: hint.output_per_mtok_micros,
                    cache_read_per_mtok_micros: hint.cache_read_per_mtok_micros,
                    cache_write_per_mtok_micros: hint.cache_write_per_mtok_micros,
                });
                catalog.insert(model_id, hint);
            }
        }
        catalog_models.sort_by(|a, b| a.model_id.cmp(&b.model_id));

        let usage: HashMap<String, i64> = {
            let mut stmt = self.conn.prepare(
                "SELECT model, COALESCE(SUM(request_count), 0) FROM usage_records GROUP BY model",
            )?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter().collect()
        };

        // 变种映射：raw_model → 标准模型（带映射来源）。刻意不做工具清单与
        // 每变种计数——那要两条 usage_records 全表 GROUP BY，纯展示不值得。
        let mut variants: HashMap<String, Vec<pricing_overview::PricingVariant>> = HashMap::new();
        // 每个标准模型的映射来源"最弱一环"：suggested > auto > user。
        let mut worst: HashMap<String, (u8, String)> = HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT raw_model, model_id, source FROM model_mappings ORDER BY raw_model",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (raw, model_id, source) in rows {
                let rank = match source.as_str() {
                    "user" => 0u8,
                    "auto" => 1,
                    _ => 2,
                };
                let entry = worst.entry(model_id.clone()).or_insert((0, source.clone()));
                if rank > entry.0 {
                    *entry = (rank, source.clone());
                }
                variants
                    .entry(model_id)
                    .or_default()
                    .push(pricing_overview::PricingVariant {
                        raw_model: raw.clone(),
                        source,
                    });
            }
        }

        let mut rows = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT m.id, m.display_name,
                        p.source, p.input_per_mtok_micros, p.output_per_mtok_micros,
                        p.cache_read_per_mtok_micros, p.cache_write_per_mtok_micros
                 FROM models AS m
                 LEFT JOIN model_prices AS p ON p.model_id = m.id",
            )?;
            let iter = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                ))
            })?;
            for row in iter {
                let (id, display_name, source, input, output, cache_read, cache_write) = row?;
                rows.push(PricingModelRow {
                    usage_count: usage.get(&id).copied().unwrap_or(0),
                    variants: variants.remove(&id).unwrap_or_default(),
                    catalog: catalog.remove(&id),
                    mapping_source: worst.remove(&id).map(|(_, source)| source),
                    id,
                    display_name,
                    price_source: source,
                    input_per_mtok_micros: input,
                    output_per_mtok_micros: output,
                    cache_read_per_mtok_micros: cache_read,
                    cache_write_per_mtok_micros: cache_write,
                });
            }
        }
        // 有用量的排前面，其次待确认的显眼，最后是没用的空壳；同层按 id。
        rows.sort_by(|a, b| (b.usage_count, a.id.as_str()).cmp(&(a.usage_count, b.id.as_str())));

        Ok(PricingOverviewPayload {
            synced_at: self.catalog_synced_at()?,
            models: rows,
            catalog_models,
        })
    }

    /// 用户设置/修改一个标准模型的四桶价（`source='user'`，目录同步永不覆盖），
    /// 同一事务内自动重算——确认与修改即计价，不需要第二个人工动作。
    pub fn set_model_price(
        &self,
        model_id: &str,
        input_per_mtok_micros: Option<i64>,
        output_per_mtok_micros: Option<i64>,
        cache_read_per_mtok_micros: Option<i64>,
        cache_write_per_mtok_micros: Option<i64>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        // 确认即建档：定价页可以对目录里见过、本地还没有用量的模型先挂价。
        tx.execute(
            "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
             VALUES (?1, ?1, ?2)",
            params![model_id, now_ms()],
        )?;
        tx.execute(
            "INSERT INTO model_prices
                 (model_id, input_per_mtok_micros, output_per_mtok_micros,
                  cache_read_per_mtok_micros, cache_write_per_mtok_micros, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 'user')
             ON CONFLICT(model_id) DO UPDATE SET
                 input_per_mtok_micros = excluded.input_per_mtok_micros,
                 output_per_mtok_micros = excluded.output_per_mtok_micros,
                 cache_read_per_mtok_micros = excluded.cache_read_per_mtok_micros,
                 cache_write_per_mtok_micros = excluded.cache_write_per_mtok_micros,
                 source = 'user'",
            params![
                model_id,
                input_per_mtok_micros,
                output_per_mtok_micros,
                cache_read_per_mtok_micros,
                cache_write_per_mtok_micros
            ],
        )?;
        // 只重算这个模型的行：改价不牵连其他模型。
        reprice_scoped(&tx, Some(&[model_id.to_string()]))?;
        tx.commit()?;
        Ok(())
    }

    /// 入库建议值的目录解析：归一名 → 启发候选 依次反查 models.dev 快照。
    /// 归一名（或显示名归一后）命中目录 → (目录 id, 'auto')；启发候选命中 →
    /// (候选, 'suggested')；都没命中 → (归一名, 'auto')。只在入库 miss 分支
    /// 调用：走 idx_catalog_model 索引，每次至多 1 + 候选数次点查。
    fn resolve_suggestion(&self, canonical: &str) -> (String, &'static str) {
        let in_catalog = |id: &str| {
            self.conn
                .query_row(
                    "SELECT 1 FROM catalog_entries WHERE model_id = ?1 LIMIT 1",
                    params![id],
                    |_| Ok(()),
                )
                .is_ok()
        };
        if in_catalog(canonical) {
            return (canonical.to_string(), "auto");
        }
        let candidates = crate::pricing::heuristic_candidates(canonical);
        for candidate in &candidates {
            if in_catalog(candidate) {
                return (candidate.clone(), "suggested");
            }
        }
        // 目录没收录也剥（产品决策）：修饰尾缀（-free /:free /日期等）的母型号
        // 语义明确，标 suggested 待用户过目——剥错在定价页一键改回，绝不静默。
        if let Some(deepest) = candidates.last() {
            return (deepest.clone(), "suggested");
        }
        (canonical.to_string(), "auto")
    }

    /// 用户确认一个标准模型的自动映射：该模型下所有 auto/suggested 变种翻成
    /// user（目录同步永不覆盖）。纯元数据操作——指向与价格都不变，无需重算。
    pub fn confirm_model(&self, model_id: &str, now: i64) -> Result<usize> {
        let updated = self.conn.execute(
            "UPDATE model_mappings SET source = 'user', updated_at = ?2
             WHERE model_id = ?1 AND source IN ('auto', 'suggested')",
            params![model_id, now],
        )?;
        Ok(updated)
    }

    /// 映射自愈（启动/扫描时跑，幂等）：修两类历史遗留——①自映射壳（raw →
    /// 自己，老版本建档时还没有归一化的产物）改指格式归一名；②标准名自带修饰
    /// 尾缀（-free /:free /日期等）的改指到最深母型号。两者都清掉因此空掉的壳
    /// 模型行，有改动就重算。user 映射与挂了 user 价的目标不碰——前者是用户
    /// 意志，后者可能正被用户刻意定价。返回改指的映射条数。
    pub fn normalize_auto_mappings(&self) -> Result<usize> {
        let now = now_ms();
        let tx = self.conn.unchecked_transaction()?;
        let mappings: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT raw_model, model_id FROM model_mappings
                 WHERE source IN ('auto', 'suggested')",
            )?;
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut remapped = 0;
        let mut repriced_models: Vec<String> = Vec::new();
        for (raw, model_id) in mappings {
            // 用户挂过价的目标是用户意志：哪怕看着像壳也不动。
            let user_priced = tx
                .query_row(
                    "SELECT 1 FROM model_prices WHERE model_id = ?1 AND source = 'user'",
                    params![model_id],
                    |_| Ok(()),
                )
                .is_ok();
            if user_priced {
                continue;
            }
            let canonical = crate::pricing::canonicalize_model_id(&raw);
            let (target, source) = if model_id == raw && canonical != model_id {
                (canonical.clone(), "auto")
            } else if model_id == canonical {
                match crate::pricing::heuristic_candidates(&canonical).last() {
                    Some(deepest) => (deepest.clone(), "suggested"),
                    None => continue,
                }
            } else {
                continue;
            };
            if target != model_id {
                repoint_mapping_tx(&tx, &raw, &target, source, now)?;
                repriced_models.push(target);
                remapped += 1;
            }
        }
        if remapped > 0 {
            delete_orphan_models(&tx)?;
            reprice_scoped(&tx, Some(&repriced_models))?;
        }
        tx.commit()?;
        Ok(remapped)
    }

    /// 用户改一条变种映射并立即生效：同一事务内完成"写 user 映射 + 改写两张
    /// 事实表 + 重算"。变种不存在映射时也会登记（新 raw 名直接指到目标模型）。
    pub fn set_model_mapping(&self, raw_model: &str, model_id: &str, now: i64) -> Result<usize> {
        let _ = now; // bind_variants 自取 now_ms；单条与批量不许有两种时间源。
        self.bind_variants(&[raw_model.to_string()], model_id)
    }

    /// 把一批原始变种名批量改指到一个标准模型（`source='user'`）：映射 upsert、
    /// 两张事实表归并、孤儿壳清理与重算在**同一事务**里完成。定价页"收编变体"
    /// 的入口——多个变体归入一个标准模型是常态操作，逐条调用会重算中间态。
    /// 返回事实表改写行数 + 重算行数（与 set_model_mapping 口径一致）。
    pub fn bind_variants(&self, raws: &[String], model_id: &str) -> Result<usize> {
        let now = now_ms();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
             VALUES (?1, ?1, ?2)",
            params![model_id, now],
        )?;
        let mut updated = 0usize;
        for raw in raws {
            tx.execute(
                "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                 VALUES (?1, ?2, 'user', ?3)
                 ON CONFLICT(raw_model) DO UPDATE SET
                     model_id = excluded.model_id,
                     source = 'user',
                     updated_at = excluded.updated_at",
                params![raw, model_id, now],
            )?;
            for table in FACT_TABLES {
                updated += tx.execute(
                    &format!("UPDATE {table} SET model = ?2 WHERE model_raw = ?1"),
                    params![raw, model_id],
                )?;
            }
        }
        delete_orphan_models(&tx)?;
        // 受影响范围 = 新目标：变体行已改写到它名下，按它的价格重算即可；
        // 旧目标留在原地的其他行不受影响，不做全库陪跑。
        let repriced = reprice_scoped(&tx, Some(&[model_id.to_string()]))?;
        tx.commit()?;
        // 改指可能把变体还给了刚重建档的模型（典型：撤销误合并，目标壳已被
        // 删、库里无价）：借本地目录快照给目标补缺价并重算（与 rename_model
        // 同口径），恢复一步到位。幂等，目录空时是空操作。
        self.sync_catalog_prices()?;
        Ok(updated + repriced)
    }

    /// 把标准模型 `from` 并入 `into`：全部变种映射改指、两张事实表的 model 改写、
    /// 价格行搬家（目标已有 user 价则保留目标的）、删除 `from` 的壳。同一事务内
    /// 重算。用于把非标准命名的历史模型（建 currently 无法改名的）收编。
    pub fn merge_models(&self, from_id: &str, into_id: &str) -> Result<usize> {
        if from_id == into_id {
            return Err(Error::Internal("模型不能并入自身".to_string()));
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO models (id, display_name, first_seen_at)
             VALUES (?1, ?1, ?2)",
            params![into_id, now_ms()],
        )?;
        let mut updated = tx.execute(
            "UPDATE model_mappings SET model_id = ?2, source = 'user'
             WHERE model_id = ?1",
            params![from_id, into_id],
        )?;
        for table in FACT_TABLES {
            updated += tx.execute(
                &format!("UPDATE {table} SET model = ?2 WHERE model = ?1"),
                params![from_id, into_id],
            )?;
        }
        // 价格搬家：目标没有价才搬（user 价优先保留），然后清掉来源行。
        tx.execute(
            "INSERT INTO model_prices
                 (model_id, input_per_mtok_micros, output_per_mtok_micros,
                  cache_read_per_mtok_micros, cache_write_per_mtok_micros, source)
             SELECT ?2, input_per_mtok_micros, output_per_mtok_micros,
                    cache_read_per_mtok_micros, cache_write_per_mtok_micros, source
             FROM model_prices WHERE model_id = ?1
             ON CONFLICT(model_id) DO NOTHING",
            params![from_id, into_id],
        )?;
        tx.execute(
            "DELETE FROM model_prices WHERE model_id = ?1",
            params![from_id],
        )?;
        tx.execute("DELETE FROM models WHERE id = ?1", params![from_id])?;
        // 受影响范围 = 并入目标：来源的行已全部搬走，来源侧无需重算。
        let repriced = reprice_scoped(&tx, Some(&[into_id.to_string()]))?;
        tx.commit()?;
        Ok(updated + repriced)
    }

    /// 标准模型改名（定价页把本地名改成 models.dev 的规范名）：借合并语义收编
    /// 变种映射、事实与价格，随后按本地目录快照给改名后的模型补缺价——改名
    /// 即完成关联与挂价，无需再手动同步。补价走 [`Self::sync_catalog_prices`]
    /// 的全表"只补缺"（与目录同步同口径，user 价永不覆盖），无网络。
    pub fn rename_model(&self, from_id: &str, into_id: &str) -> Result<usize> {
        let moved = self.merge_models(from_id, into_id)?;
        let (_matched, priced, _remapped) = self.sync_catalog_prices()?;
        Ok(moved + priced)
    }

    /// 总览聚合：分项与总分都走视图 `v_usage_cost`，与行级口径严格一致。已知成本合计不含未知行；未知行数单独给出，
    /// 由前端标注"部分未知"，绝不混进总数冒充精确值。
    /// `disabled` 是被禁用工具的 id 列表（前端偏好）：聚合只统计启用的工具；
    /// 禁用不删数据，重新启用后数字原样回来。
    pub fn overview(&self, disabled: &[String]) -> Result<OverviewPayload> {
        // 每个子查询共用同一组编号占位符（?1..?n），整条语句只需绑定一遍清单。
        let clause = tool_not_in_clause(disabled);
        let totals = self.conn.query_row(
            &format!(
                "SELECT
                (SELECT COALESCE(SUM(input_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(output_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(cache_read_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(cache_write_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(reasoning_tokens), 0) FROM usage_records{clause}),
                (SELECT COUNT(*) FROM sessions{clause}),
                (SELECT COALESCE(SUM(request_count), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(cost_micros), 0) FROM v_usage_cost{clause}),
                (SELECT COUNT(*) FROM (SELECT tool, cost_micros FROM v_usage_cost{clause}) WHERE cost_micros IS NULL)"
            ),
            params_from_iter(disabled),
            |row| {
                Ok(UsageTotals {
                    input_tokens: row.get(0)?,
                    output_tokens: row.get(1)?,
                    cache_read_tokens: row.get(2)?,
                    cache_write_tokens: row.get(3)?,
                    reasoning_tokens: row.get(4)?,
                    session_count: row.get(5)?,
                    record_count: row.get(6)?,
                    known_cost_micros: row.get(7)?,
                    unknown_cost_rows: row.get(8)?,
                })
            },
        )?;

        let mut stmt = self.conn.prepare(&format!(
            "SELECT model,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(cost_micros), 0),
                    COUNT(*) FILTER (WHERE cost_micros IS NULL)
             FROM v_usage_cost{clause}
             GROUP BY model
             ORDER BY 7 DESC, 1 ASC"
        ))?;
        let by_model = stmt
            .query_map(params_from_iter(disabled), |row| {
                Ok(ModelUsageRow {
                    model: row.get(0)?,
                    input_tokens: row.get(1)?,
                    output_tokens: row.get(2)?,
                    cache_read_tokens: row.get(3)?,
                    cache_write_tokens: row.get(4)?,
                    reasoning_tokens: row.get(5)?,
                    known_cost_micros: row.get(6)?,
                    unknown_cost_rows: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(OverviewPayload { totals, by_model })
    }

    /// 仪表盘聚合：一次给出总览页全部卡片的切片。筛选与明细页同语义
    /// （[`UsageFilters`]，projects 的 `""` = 无项目），`disabled` 是被禁用
    /// 工具的 id 列表（前端偏好），禁用的工具不参与聚合。
    ///
    /// 口径不变式：瀑布分项之和 = 已知总成本。input/cache_read/cache_write
    /// 直接合计物化分桶；推理费 = Σ 推理量 × 输出价（与重算的"输出桶计价量
    /// 含推理"一致），从输出桶毛费里拆出，净输出 = 毛费 − 推理费——分项之和
    /// 仍落回四个物化分桶，即视图的已知总分。
    pub fn dashboard(
        &self,
        filters: &UsageFilters,
        disabled: &[String],
    ) -> Result<DashboardPayload> {
        let (where_sql, args) = usage_where(filters, disabled);
        let cte = dashboard_cte(&where_sql);
        let bind = || params_from_iter(args.iter());

        // 库里没有数据时全部切片归零：从 1970 起铺桶序列没有意义。
        let (min_ts, max_ts): (i64, i64) = self.conn.query_row(
            &format!("{cte} SELECT COALESCE(MIN(ts), 0), COALESCE(MAX(ts), 0) FROM filtered"),
            bind(),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if max_ts <= 0 {
            return Ok(empty_dashboard("day"));
        }

        // 窗口端点：显式筛选优先；"不限时间"回落数据的 MIN/MAX（上界再与现在取大，
        // 兜住时钟误差导致的时间戳落到未来）。
        let start = filters.time_start.unwrap_or(min_ts);
        let end = filters.time_end.unwrap_or(max_ts.max(now_ms()));
        let grain = trend_grain(end.saturating_sub(start));
        // 脏时间戳（如落在 1970 的记录）会把"不限时间"撑成上千桶：趋势放弃，
        // 其余切片照常（它们按维度聚合，规模有界）。
        let starts = bucket_starts(start, end, grain);
        let starts = if starts.len() > 400 {
            Vec::new()
        } else {
            starts
        };
        let bucket_index: HashMap<i64, usize> = starts
            .iter()
            .copied()
            .enumerate()
            .map(|(i, ms)| (ms, i))
            .collect();

        // ── 使用时间（活跃段并集）────────────────────────────
        // 相邻间隔 ≤ 30 分钟的记录归为同一活跃段；段裁剪到筛选窗口后取
        // 并集。旧口径是"窗口内出现的会话按首末跨度求和"：长寿会话窗口
        // 外的时长、挂机空闲、并行会话的重叠全都重复计入，7 天窗口能算
        // 出 18 天；新口径恒不超过窗口本身。
        const ACTIVE_GAP_MS: i64 = 30 * 60 * 1000;
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT session_id, ts FROM filtered
                     WHERE session_id IS NOT NULL ORDER BY session_id, ts"
        ))?;
        let mut intervals: Vec<(i64, i64)> = Vec::new();
        let mut rows = stmt.query(bind())?;
        let mut cur_sid: Option<i64> = None;
        let mut seg = (0_i64, 0_i64);
        while let Some(row) = rows.next()? {
            let sid: i64 = row.get(0)?;
            let ts: i64 = row.get(1)?;
            if cur_sid == Some(sid) && ts - seg.1 <= ACTIVE_GAP_MS {
                seg.1 = ts;
            } else {
                if cur_sid.is_some() {
                    intervals.push(seg);
                }
                cur_sid = Some(sid);
                seg = (ts, ts);
            }
        }
        if cur_sid.is_some() {
            intervals.push(seg);
        }
        let active_duration_ms = union_duration_ms(intervals, start, end);

        // ── 总量 ─────────────────────────────────────────────
        let totals = self.conn.query_row(
            &format!(
                "{cte} SELECT
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*),
                    COUNT(DISTINCT session_id),
                    COUNT(DISTINCT CASE WHEN project_dir <> '' THEN project_dir END),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COUNT(DISTINCT tool)
                 FROM filtered"
            ),
            bind(),
            |row| {
                Ok(DashboardTotals {
                    cost_micros: row.get(0)?,
                    tokens: row.get(1)?,
                    calls: row.get(2)?,
                    sessions: row.get(3)?,
                    projects: row.get(4)?,
                    cache_read_tokens: row.get(5)?,
                    active_duration_ms,
                    active_tools: row.get(6)?,
                })
            },
        )?;

        // ── 趋势（按粒度分桶，缺失的桶补 0）───────────────────
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT {bucket} AS bucket_ms,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_read_cost_micros), 0)
                 FROM filtered
                 GROUP BY bucket_ms",
            bucket = grain.bucket_expr(),
        ))?;
        let trend_raw: HashMap<i64, [i64; 5]> = stmt
            .query_map(bind(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    [
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ],
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        let trend: Vec<DashboardTrendBucket> = starts
            .iter()
            .copied()
            .map(|ms| {
                let cell = trend_raw.get(&ms).copied().unwrap_or_default();
                DashboardTrendBucket {
                    start_ms: ms,
                    cost_micros: cell[0],
                    tokens: cell[1],
                    calls: cell[2],
                    cache_read_tokens: cell[3],
                    cache_read_cost_micros: cell[4],
                }
            })
            .collect();

        // ── 按模型趋势（数组与 trend 的桶一一对齐）────────────
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT {bucket} AS bucket_ms, model,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY bucket_ms, model",
            bucket = grain.bucket_expr(),
        ))?;
        let model_trend_raw: HashMap<(i64, String), (i64, i64, i64)> = stmt
            .query_map(bind(), |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, String>(1)?),
                    (row.get(2)?, row.get(3)?, row.get(4)?),
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;

        // ── 构成（模型维度）；模型序是 modelTrend / spread 的权威顺序 ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT model,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*),
                    COUNT(*) FILTER (WHERE cost_micros IS NULL)
                 FROM filtered
                 GROUP BY model
                 ORDER BY 2 DESC, model ASC"
        ))?;
        let composition: Vec<DashboardBreakdownRow> = stmt
            .query_map(bind(), |row| {
                Ok(DashboardBreakdownRow {
                    label: row.get(0)?,
                    cost_micros: row.get(1)?,
                    tokens: row.get(2)?,
                    calls: row.get(3)?,
                    unknown_cost: row.get::<_, i64>(4)? > 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let model_index: HashMap<&str, usize> = composition
            .iter()
            .enumerate()
            .map(|(i, row)| (row.label.as_str(), i))
            .collect();
        let mut model_trend: Vec<DashboardModelSeries> = composition
            .iter()
            .map(|row| DashboardModelSeries {
                model: row.label.clone(),
                cost_micros: vec![0; starts.len()],
                tokens: vec![0; starts.len()],
                calls: vec![0; starts.len()],
            })
            .collect();
        for ((ms, model), (cost, tokens, calls)) in model_trend_raw {
            if let (Some(&bucket), Some(&series)) =
                (bucket_index.get(&ms), model_index.get(model.as_str()))
            {
                let series = &mut model_trend[series];
                series.cost_micros[bucket] = cost;
                series.tokens[bucket] = tokens;
                series.calls[bucket] = calls;
            }
        }

        // ── 构成（项目维度）；unknown 是模型维度的概念，恒 false ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT project_dir,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY project_dir
                 ORDER BY 2 DESC, project_dir ASC"
        ))?;
        let projects: Vec<DashboardBreakdownRow> = stmt
            .query_map(bind(), |row| {
                Ok(DashboardBreakdownRow {
                    label: row.get(0)?,
                    cost_micros: row.get(1)?,
                    tokens: row.get(2)?,
                    calls: row.get(3)?,
                    unknown_cost: false,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // ── 流向：工具 → 模型、模型 → 项目两段 ────────────────
        let mut flow: Vec<DashboardFlowLink> = Vec::new();
        for sql in [
            format!(
                "{cte} SELECT tool, model, COALESCE(SUM(cost_micros), 0), COALESCE(SUM({TOKENS_SUM_EXPR}), 0), COUNT(*) FROM filtered GROUP BY tool, model"
            ),
            format!(
                "{cte} SELECT model, project_dir, COALESCE(SUM(cost_micros), 0), COALESCE(SUM({TOKENS_SUM_EXPR}), 0), COUNT(*) FROM filtered GROUP BY model, project_dir"
            ),
        ] {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(bind(), |row| {
                    Ok(DashboardFlowLink {
                        source: row.get(0)?,
                        target: row.get(1)?,
                        cost_micros: row.get(2)?,
                        tokens: row.get(3)?,
                        calls: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            flow.extend(rows);
        }
        flow.sort_by(|a, b| {
            b.cost_micros
                .cmp(&a.cost_micros)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.target.cmp(&b.target))
        });

        // ── 活跃热力图：本地星期（0=周一）× 小时，恒 7×24 格 ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT (CAST(strftime('%w', ts/1000, 'unixepoch', 'localtime') AS INTEGER) + 6) % 7 AS day,
                    CAST(strftime('%H', ts/1000, 'unixepoch', 'localtime') AS INTEGER) AS hour,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY day, hour"
        ))?;
        let activity_raw: HashMap<(i64, i64), (i64, i64, i64)> = stmt
            .query_map(bind(), |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                    (row.get(2)?, row.get(3)?, row.get(4)?),
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        let mut activity: Vec<DashboardActivityCell> = Vec::with_capacity(7 * 24);
        for day in 0..7 {
            for hour in 0..24 {
                let cell = activity_raw.get(&(day, hour)).copied().unwrap_or((0, 0, 0));
                activity.push(DashboardActivityCell {
                    day,
                    hour,
                    cost_micros: cell.0,
                    tokens: cell.1,
                    calls: cell.2,
                });
            }
        }

        // ── 日历：不吃时间筛选（窗口是卡片自己的设置），其余筛选照常 ──
        let mut daily_filters = filters.clone();
        daily_filters.time_start = None;
        daily_filters.time_end = None;
        let (daily_where, daily_args) = usage_where(&daily_filters, disabled);
        let daily_cte = dashboard_cte(&daily_where);
        let mut stmt = self.conn.prepare(&format!(
            "{daily_cte} SELECT strftime('%Y-%m-%d', ts/1000, 'unixepoch', 'localtime') AS d,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY d
                 ORDER BY d ASC"
        ))?;
        let daily: Vec<DashboardDailyPoint> = stmt
            .query_map(params_from_iter(daily_args.iter()), |row| {
                Ok(DashboardDailyPoint {
                    date: row.get(0)?,
                    cost_micros: row.get(1)?,
                    tokens: row.get(2)?,
                    calls: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // ── 箱线图：只看有价行，每行一次调用的开销与 token，Rust 侧排序取分位 ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT model, cost_micros, {TOKENS_SUM_EXPR}
                 FROM filtered
                 WHERE cost_micros IS NOT NULL"
        ))?;
        let spread_rows = stmt
            .query_map(bind(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut spread_raw: HashMap<String, (Vec<i64>, Vec<i64>)> = HashMap::new();
        for (model, cost, tokens) in spread_rows {
            let entry = spread_raw.entry(model).or_default();
            entry.0.push(cost);
            entry.1.push(tokens);
        }
        let spread: Vec<DashboardSpread> = composition
            .iter()
            .filter_map(|row| {
                let (mut costs, mut tokens) = spread_raw.remove(&row.label)?;
                costs.sort_unstable();
                tokens.sort_unstable();
                let five = |sorted: &[i64]| -> Vec<i64> {
                    [0.0, 0.25, 0.5, 0.75, 1.0]
                        .iter()
                        .map(|&q| quantile(sorted, q))
                        .collect()
                };
                Some(DashboardSpread {
                    label: row.label.clone(),
                    cost_micros: five(&costs),
                    tokens: five(&tokens),
                })
            })
            .collect();

        // ── 会话消耗气泡：一会话（× 模型）一点。全部行未定价的会话没有费用
        // 可画（SUM 全 NULL），HAVING 排除；部分未定价按已知部分计入。
        // 分组必须带 tool：external_id 只在工具内唯一，(tool, external_id) 才是
        // 会话自然键。首末时间给前端 tooltip 定位"这是哪次工作"。
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT tool, external_id, project_dir, model, COUNT(*),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0), SUM(cost_micros), MIN(ts), MAX(ts)
             FROM filtered
             WHERE external_id IS NOT NULL
             GROUP BY tool, external_id, project_dir, model
             HAVING SUM(cost_micros) IS NOT NULL
             ORDER BY 7 DESC"
        ))?;
        let sessions: Vec<DashboardSessionPoint> = stmt
            .query_map(bind(), |row| {
                Ok(DashboardSessionPoint {
                    tool: row.get(0)?,
                    external_id: row.get(1)?,
                    project_dir: row.get(2)?,
                    model: row.get(3)?,
                    calls: row.get(4)?,
                    tokens: row.get(5)?,
                    cost_micros: row.get(6)?,
                    start_ms: row.get(7)?,
                    end_ms: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // ── 瀑布：四个物化分桶 + 推理费（输出价 × 推理量）拆分 ──
        let (input_cost, output_gross, reasoning_cost, cache_read_cost, cache_write_cost): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = self.conn.query_row(
            &format!(
                "{cte} SELECT
                    COALESCE(SUM(input_cost_micros), 0),
                    COALESCE(SUM(output_cost_micros), 0),
                    COALESCE(SUM(COALESCE(reasoning_tokens, 0) * COALESCE(output_per_mtok_micros, 0) / {MICROS_PER_MTK}), 0),
                    COALESCE(SUM(cache_read_cost_micros), 0),
                    COALESCE(SUM(cache_write_cost_micros), 0)
                 FROM filtered"
            ),
            bind(),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        // 整除截断都向下，毛费 ≥ 推理费逐行成立；max(0) 只是形式兜底。
        let cost_composition = DashboardCostComposition {
            input_cost_micros: input_cost,
            output_cost_micros: (output_gross - reasoning_cost).max(0),
            reasoning_cost_micros: reasoning_cost,
            cache_read_cost_micros: cache_read_cost,
            cache_write_cost_micros: cache_write_cost,
        };

        let unknown_model_count = composition.iter().filter(|row| row.unknown_cost).count() as i64;

        Ok(DashboardPayload {
            totals,
            trend,
            trend_grain: grain.as_str().to_string(),
            model_trend,
            composition,
            projects,
            flow,
            activity,
            daily,
            spread,
            cost_composition,
            unknown_model_count,
            sessions,
        })
    }

    /// 网关侧总览：与 [`Storage::overview`] 完全同构的口径，但只看 `gateway_requests`。
    /// 双计未解决前两份聚合绝不合并。
    pub fn gateway_overview(&self) -> Result<GatewayOverviewPayload> {
        let totals = self.conn.query_row(
            "SELECT
                (SELECT COALESCE(SUM(input_tokens), 0) FROM gateway_requests),
                (SELECT COALESCE(SUM(output_tokens), 0) FROM gateway_requests),
                (SELECT COALESCE(SUM(cache_read_tokens), 0) FROM gateway_requests),
                (SELECT COALESCE(SUM(cache_write_tokens), 0) FROM gateway_requests),
                (SELECT COALESCE(SUM(reasoning_tokens), 0) FROM gateway_requests),
                (SELECT COUNT(*) FROM gateway_requests),
                (SELECT COUNT(*) FROM gateway_requests WHERE status_code >= 400),
                (SELECT COALESCE(SUM(cost_micros), 0) FROM v_gateway_cost),
                (SELECT COUNT(*) FROM v_gateway_cost WHERE cost_micros IS NULL)",
            [],
            |row| {
                Ok(GatewayTotals {
                    input_tokens: row.get(0)?,
                    output_tokens: row.get(1)?,
                    cache_read_tokens: row.get(2)?,
                    cache_write_tokens: row.get(3)?,
                    reasoning_tokens: row.get(4)?,
                    request_count: row.get(5)?,
                    error_count: row.get(6)?,
                    known_cost_micros: row.get(7)?,
                    unknown_cost_rows: row.get(8)?,
                })
            },
        )?;

        let mut stmt = self.conn.prepare(
            "SELECT model,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(cost_micros), 0),
                    COUNT(*) FILTER (WHERE cost_micros IS NULL)
             FROM v_gateway_cost
             GROUP BY model
             ORDER BY 7 DESC, 1 ASC",
        )?;
        let by_model = stmt
            .query_map([], |row| {
                Ok(ModelUsageRow {
                    model: row.get(0)?,
                    input_tokens: row.get(1)?,
                    output_tokens: row.get(2)?,
                    cache_read_tokens: row.get(3)?,
                    cache_write_tokens: row.get(4)?,
                    reasoning_tokens: row.get(5)?,
                    known_cost_micros: row.get(6)?,
                    unknown_cost_rows: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(GatewayOverviewPayload { totals, by_model })
    }

    /// 流量明细分页：按时间倒序的一页 + 总行数。成本走 `v_gateway_cost` 视图口径。
    /// `offset`/`limit` 由调用方换算（页码 × 页大小）；limit 上限由调用方约束。
    pub fn gateway_requests_page(&self, offset: i64, limit: i64) -> Result<GatewayRequestsPage> {
        let total: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM gateway_requests", [], |row| {
                    row.get(0)
                })?;

        let mut stmt = self.conn.prepare(
            "SELECT
                r.id, r.ts, r.model_raw, r.model,
                r.input_tokens, r.output_tokens,
                r.status_code, r.latency_ms, r.upstream, v.cost_micros
             FROM gateway_requests AS r
             JOIN v_gateway_cost AS v ON v.id = r.id
             ORDER BY r.ts DESC, r.id DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt
            .query_map(params![limit, offset], |row| {
                Ok(GatewayRequestRow {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    model_raw: row.get(2)?,
                    model: row.get(3)?,
                    input_tokens: row.get(4)?,
                    output_tokens: row.get(5)?,
                    status_code: row.get(6)?,
                    latency_ms: row.get(7)?,
                    upstream: row.get(8)?,
                    cost_micros: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(GatewayRequestsPage { rows, total })
    }

    /// 明细页请求级分页：筛选/排序/分页都在 SQL 里做（行数无上界，不能整表拉回前端）。
    /// 成本走 `v_usage_cost` 视图，与聚合口径严格一致；会话归属 LEFT JOIN——
    /// 删除会话后 `session_id` 为 NULL，用量行保留。
    pub fn usage_records_page(&self, query: &UsageRecordsQuery) -> Result<UsageRecordsPage> {
        let (where_sql, args) = usage_where(&query.filters, &query.disabled);
        let from = "FROM usage_records AS r
             JOIN v_usage_cost AS v ON v.id = r.id
             LEFT JOIN sessions AS s ON s.id = r.session_id";

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) {from}{where_sql}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        // NULL 排序：SQLite 默认 ASC 时 NULL 在最前、DESC 在最后——"未知成本 /
        // 无耗时不与数值混排"（未知不冒充 0）。r.id DESC 做稳定 tiebreaker。
        let order = usage_sort_expr(query.sort_key.as_deref());
        let dir = if query.sort_desc { "DESC" } else { "ASC" };

        let mut stmt = self.conn.prepare(&format!(
            "SELECT
                r.id, r.ts, r.tool, r.model,
                r.input_tokens, r.output_tokens, r.cache_read_tokens, r.cache_write_tokens,
                r.reasoning_tokens, r.duration_ms,
                v.cost_micros, v.input_cost_micros, v.output_cost_micros,
                v.cache_read_cost_micros, v.cache_write_cost_micros,
                s.external_id, s.project_dir
             {from}{where_sql}
             ORDER BY {order} {dir}, r.id DESC
             LIMIT ? OFFSET ?"
        ))?;
        let mut bindings = args.clone();
        bindings.push(Value::Integer(query.limit.max(0)));
        bindings.push(Value::Integer(query.offset.max(0)));
        let rows = stmt
            .query_map(params_from_iter(bindings.iter()), |row| {
                Ok(UsageRecordRow {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    tool: row.get(2)?,
                    model: row.get(3)?,
                    session_external_id: row.get(15)?,
                    project_dir: row.get(16)?,
                    input_tokens: row.get(4)?,
                    output_tokens: row.get(5)?,
                    cache_read_tokens: row.get(6)?,
                    cache_write_tokens: row.get(7)?,
                    reasoning_tokens: row.get(8)?,
                    duration_ms: row.get(9)?,
                    cost_micros: row.get(10)?,
                    input_cost_micros: row.get(11)?,
                    output_cost_micros: row.get(12)?,
                    cache_read_cost_micros: row.get(13)?,
                    cache_write_cost_micros: row.get(14)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(UsageRecordsPage { rows, total })
    }

    /// 明细页筛选下拉的选项与计数：分面口径——每个维度的计数用其余维度的
    /// 筛选条件（不含本维度，否则选中即把他项归零）加时间与搜索；按条数降序；
    /// 禁用工具的记录不进选项（与其它读路径一致）。
    pub fn usage_filter_options(
        &self,
        filters: &UsageFilters,
        disabled: &[String],
    ) -> Result<UsageFilterOptionsPayload> {
        // 各维度去掉自己的白名单：分面计数（faceted search）的标准口径。
        let mut tool_filters = filters.clone();
        tool_filters.tools.clear();
        let mut model_filters = filters.clone();
        model_filters.models.clear();
        let mut project_filters = filters.clone();
        project_filters.projects.clear();

        let (tool_where, tool_args) = usage_where(&tool_filters, disabled);
        let (model_where, model_args) = usage_where(&model_filters, disabled);
        let (project_where, project_args) = usage_where(&project_filters, disabled);

        let query_options = |sql: String, args: &[Value]| -> Result<Vec<UsageFilterOption>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(args.iter()), |row| {
                    Ok(UsageFilterOption {
                        value: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        };

        // 项目与搜索子句引用 s. 别名，三组查询统一带上 sessions JOIN。
        let from = "FROM usage_records AS r
             LEFT JOIN sessions AS s ON s.id = r.session_id";
        let tools = query_options(
            format!(
                "SELECT r.tool, COUNT(*) {from}{tool_where}
                 GROUP BY r.tool ORDER BY 2 DESC, 1 ASC"
            ),
            &tool_args,
        )?;
        let models = query_options(
            format!(
                "SELECT r.model, COUNT(*) {from}{model_where}
                 GROUP BY r.model ORDER BY 2 DESC, 1 ASC"
            ),
            &model_args,
        )?;
        let projects = query_options(
            format!(
                "SELECT s.project_dir, COUNT(*) {from}{project_where}
                 GROUP BY s.project_dir ORDER BY 2 DESC, 1 ASC"
            ),
            &project_args,
        )?;

        Ok(UsageFilterOptionsPayload {
            tools,
            models,
            projects,
        })
    }

    /// 会话页分页：筛选/排序/分页都在 SQL 里做；用量按会话聚合。零用量的
    /// 会话壳（如只写会话头的日志）保留，聚合列为 0——"看过但没花钱"也是事实。
    /// 成本不在会话维度展示（产品决策）：会话行的成本既不查询也不返回。
    pub fn sessions_page(&self, query: &SessionsPageQuery) -> Result<SessionsPage> {
        let (where_sql, args) = sessions_where(&query.filters, &query.disabled);
        let from = "FROM sessions AS s
             LEFT JOIN usage_records AS r ON r.session_id = s.id";

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM sessions AS s{where_sql}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        let order = sessions_sort_expr(query.sort_key.as_deref());
        let dir = if query.sort_desc { "DESC" } else { "ASC" };

        let mut stmt = self.conn.prepare(&format!(
            "SELECT
                s.id, s.tool, s.external_id, s.title, s.project_dir, s.last_activity_at,
                COALESCE(SUM(r.request_count), 0),
                COALESCE(SUM(r.input_tokens), 0),
                COALESCE(SUM(r.output_tokens), 0),
                COALESCE(SUM(r.cache_read_tokens), 0),
                COALESCE(SUM(r.cache_write_tokens), 0)
             {from}{where_sql}
             GROUP BY s.id
             ORDER BY {order} {dir}, s.id DESC
             LIMIT ? OFFSET ?"
        ))?;
        let mut bindings = args.clone();
        bindings.push(Value::Integer(query.limit.max(0)));
        bindings.push(Value::Integer(query.offset.max(0)));
        let rows = stmt
            .query_map(params_from_iter(bindings.iter()), |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    tool: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    project_dir: row.get(4)?,
                    last_activity_at: row.get(5)?,
                    request_count: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    cache_read_tokens: row.get(9)?,
                    cache_write_tokens: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(SessionsPage { rows, total })
    }
}

/// 会话页排序键 → SQL 表达式的白名单（聚合列直接展开表达式，不依赖 SELECT
/// 别名）；白名单外退回最近活跃。
fn sessions_sort_expr(key: Option<&str>) -> &'static str {
    match key {
        Some("title") => "s.title",
        Some("requests") => "COALESCE(SUM(r.request_count), 0)",
        Some("tokens") => {
            "(COALESCE(SUM(r.input_tokens), 0) + COALESCE(SUM(r.output_tokens), 0)
              + COALESCE(SUM(r.cache_read_tokens), 0) + COALESCE(SUM(r.cache_write_tokens), 0))"
        }
        _ => "s.last_activity_at",
    }
}

/// 会话页查询的 WHERE 子句与绑定参数。筛选只落在 sessions 自身的列上
/// （时间是最近活跃），聚合不参与筛选——HAVING 会让零用量壳永远不可见。
fn sessions_where(filters: &SessionFilters, disabled: &[String]) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();

    if !disabled.is_empty() {
        clauses.push(format!(
            "s.tool NOT IN ({})",
            question_marks(disabled.len())
        ));
        args.extend(disabled.iter().map(|t| Value::Text(t.clone())));
    }
    if let Some(start) = filters.time_start {
        clauses.push("s.last_activity_at >= ?".into());
        args.push(Value::Integer(start));
    }
    if let Some(end) = filters.time_end {
        clauses.push("s.last_activity_at <= ?".into());
        args.push(Value::Integer(end));
    }
    if !filters.tools.is_empty() {
        clauses.push(format!(
            "s.tool IN ({})",
            question_marks(filters.tools.len())
        ));
        args.extend(filters.tools.iter().map(|t| Value::Text(t.clone())));
    }
    if let Some(term) = filters
        .search
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let pattern = like_pattern(term);
        clauses.push(
            "(s.title LIKE ? ESCAPE '\\'
              OR s.external_id LIKE ? ESCAPE '\\'
              OR s.project_dir LIKE ? ESCAPE '\\')"
                .into(),
        );
        args.extend(std::iter::repeat_n(Value::Text(pattern), 3));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, args)
}

/// 排序键 → SQL 表达式的白名单。白名单外的键（含 `None`）一律退回时间排序——
/// 排序只是视图偏好，不值得为它开错误通道。
fn usage_sort_expr(key: Option<&str>) -> &'static str {
    match key {
        Some("tool") => "r.tool",
        Some("model") => "r.model",
        Some("project") => "s.project_dir",
        Some("session") => "s.external_id",
        // 四桶合计（量级），与前端旧口径一致。
        Some("tokens") => {
            "(r.input_tokens + r.output_tokens + r.cache_read_tokens + r.cache_write_tokens)"
        }
        Some("duration") => "r.duration_ms",
        Some("cost") => "v.cost_micros",
        _ => "r.ts",
    }
}

/// 明细查询的 WHERE 子句与绑定参数。占位符全用匿名 `?`（每处绑定只用一次，
/// 不像 overview 那样需要跨子查询复用编号）。
fn usage_where(filters: &UsageFilters, disabled: &[String]) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();

    if !disabled.is_empty() {
        clauses.push(format!(
            "r.tool NOT IN ({})",
            question_marks(disabled.len())
        ));
        args.extend(disabled.iter().map(|t| Value::Text(t.clone())));
    }
    if let Some(start) = filters.time_start {
        clauses.push("r.ts >= ?".into());
        args.push(Value::Integer(start));
    }
    if let Some(end) = filters.time_end {
        clauses.push("r.ts <= ?".into());
        args.push(Value::Integer(end));
    }
    if !filters.tools.is_empty() {
        clauses.push(format!(
            "r.tool IN ({})",
            question_marks(filters.tools.len())
        ));
        args.extend(filters.tools.iter().map(|t| Value::Text(t.clone())));
    }
    if !filters.models.is_empty() {
        clauses.push(format!(
            "r.model IN ({})",
            question_marks(filters.models.len())
        ));
        args.extend(filters.models.iter().map(|m| Value::Text(m.clone())));
    }
    // 项目筛选："" 是"无项目"哨兵 → project_dir IS NULL，其余按目录精确匹配。
    let wants_none = filters.projects.iter().any(String::is_empty);
    let named: Vec<&String> = filters.projects.iter().filter(|p| !p.is_empty()).collect();
    if wants_none || !named.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if wants_none {
            parts.push("s.project_dir IS NULL".into());
        }
        if !named.is_empty() {
            parts.push(format!(
                "s.project_dir IN ({})",
                question_marks(named.len())
            ));
            args.extend(named.iter().map(|p| Value::Text((*p).clone())));
        }
        clauses.push(format!("({})", parts.join(" OR ")));
    }
    if let Some(term) = filters
        .search
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let pattern = like_pattern(term);
        clauses
            .push("(s.external_id LIKE ? ESCAPE '\\' OR s.project_dir LIKE ? ESCAPE '\\')".into());
        args.push(Value::Text(pattern.clone()));
        args.push(Value::Text(pattern));
    }

    let sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (sql, args)
}

fn question_marks(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// 包含式 LIKE 模式；`\`、`%`、`_` 转义，配合语句里的 `ESCAPE '\'`——搜索词
/// 是用户输入，路径里的下划线不该当通配符。
fn like_pattern(term: &str) -> String {
    let mut out = String::from("%");
    for ch in term.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// 禁用工具的过滤子句：` WHERE tool NOT IN (?1,?2)`，空清单返回空串。
/// 编号占位符让同一条语句里的多个子查询复用同一组绑定。
fn tool_not_in_clause(disabled: &[String]) -> String {
    if disabled.is_empty() {
        return String::new();
    }
    let list = disabled
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    format!(" WHERE tool NOT IN ({list})")
}

// ── 仪表盘聚合的 SQL 骨架与桶序列 ─────────────────────────────

/// 五桶 token 合计表达式（缓存读是子集，不做减法）。
const TOKENS_SUM_EXPR: &str = "(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens + COALESCE(reasoning_tokens, 0))";

/// 仪表盘全部查询共用的基础 CTE：事实 + 成本视图（已知总分口径）+ 会话项目归属。
/// 项目空值就地归一为 `""`（无项目哨兵）；external_id 供会话消耗切片分组；
/// LEFT JOIN model_prices 只为推理费取输出单价，主键关联不放大行数。
/// `usage_where` 的子句引用 `r.` / `s.` 别名。
fn dashboard_cte(where_sql: &str) -> String {
    format!(
        "WITH filtered AS (
             SELECT r.*, v.cost_micros, p.output_per_mtok_micros, s.external_id,
                    COALESCE(s.project_dir, '') AS project_dir
             FROM usage_records AS r
             JOIN v_usage_cost AS v ON v.id = r.id
             LEFT JOIN sessions AS s ON s.id = r.session_id
             LEFT JOIN model_prices AS p ON p.model_id = r.model{where_sql}
         )"
    )
}

/// 趋势分桶粒度：跨度 ≤2 天按小时，≤62 天按天，否则按月。与载荷的
/// `trend_grain` 一同交给前端，label 的粒度跟它走。
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrendGrain {
    Hour,
    Day,
    Month,
}

impl TrendGrain {
    fn as_str(self) -> &'static str {
        match self {
            TrendGrain::Hour => "hour",
            TrendGrain::Day => "day",
            TrendGrain::Month => "month",
        }
    }

    /// 把 ts（ms）对齐到本地时区桶起点的 SQL 表达式。'localtime' 先转到本地
    /// 墙钟、取粒度起点，再以 'utc' 转回 epoch——漏掉最后一步会把本地墙钟
    /// 当 UTC 解释，起点恰好偏移一个时区。注意 SQLite 没有 'start of hour'
    /// 修饰符（"start of" 只到 day），小时粒度改为格式化到整点字符串再转回。
    fn bucket_expr(self) -> &'static str {
        match self {
            TrendGrain::Hour => {
                "CAST(strftime('%s', strftime('%Y-%m-%d %H:00:00', ts/1000, 'unixepoch', 'localtime'), 'utc') AS INTEGER) * 1000"
            }
            TrendGrain::Day => {
                "CAST(strftime('%s', ts/1000, 'unixepoch', 'localtime', 'start of day', 'utc') AS INTEGER) * 1000"
            }
            TrendGrain::Month => {
                "CAST(strftime('%s', ts/1000, 'unixepoch', 'localtime', 'start of month', 'utc') AS INTEGER) * 1000"
            }
        }
    }
}

/// 跨度 → 粒度。
fn trend_grain(span_ms: i64) -> TrendGrain {
    const DAY_MS: i64 = 86_400_000;
    if span_ms <= 2 * DAY_MS {
        TrendGrain::Hour
    } else if span_ms <= 62 * DAY_MS {
        TrendGrain::Day
    } else {
        TrendGrain::Month
    }
}

/// 本地墙钟分量 → epoch ms。歧义（DST 回拨的一小时出现两次）取较早者；
/// 不存在的墙钟（DST 春跳把某个小时跳没了，floor 可能落进去）按 UTC 近似——
/// 桶起点仍唯一且单调，只是那一格的墙钟语义略偏。
fn local_ms(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
    match Local.with_ymd_and_hms(y, m, d, h, min, 0) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.timestamp_millis(),
        LocalResult::None => NaiveDate::from_ymd_opt(y, m, d)
            .and_then(|date| date.and_hms_opt(h, min, 0))
            .map_or(0, |naive| naive.and_utc().timestamp_millis()),
    }
}

/// epoch ms → 本地时区的粒度起点。
fn floor_local_ms(ms: i64, grain: TrendGrain) -> i64 {
    let Some(dt) = Local.timestamp_millis_opt(ms).single() else {
        return ms;
    };
    match grain {
        TrendGrain::Hour => local_ms(dt.year(), dt.month(), dt.day(), dt.hour(), 0),
        TrendGrain::Day => local_ms(dt.year(), dt.month(), dt.day(), 0, 0),
        TrendGrain::Month => local_ms(dt.year(), dt.month(), 1, 0, 0),
    }
}

/// 覆盖 [start, end] 的桶起点序列（本地对齐，严格递增）。窗口倒挂返回空。
/// DST 回拨可能把相邻两步 floor 到同一墙钟：去重而非重复。
fn bucket_starts(start_ms: i64, end_ms: i64, grain: TrendGrain) -> Vec<i64> {
    let last = floor_local_ms(end_ms, grain);
    let mut out: Vec<i64> = Vec::new();

    if grain == TrendGrain::Month {
        let Some(dt) = Local.timestamp_millis_opt(start_ms).single() else {
            return out;
        };
        let (mut y, mut m) = (dt.year(), dt.month());
        loop {
            let start = local_ms(y, m, 1, 0, 0);
            if start > last {
                break;
            }
            if out.last().is_none_or(|prev| *prev < start) {
                out.push(start);
            }
            if m == 12 {
                y += 1;
                m = 1;
            } else {
                m += 1;
            }
        }

        return out;
    }

    // Hour/Day 用 epoch 步进再 floor：floor 单调 + 去重，天然防回拨回环。
    let step: i64 = if grain == TrendGrain::Hour {
        3_600_000
    } else {
        86_400_000
    };
    let mut cursor = floor_local_ms(start_ms, grain);
    while cursor <= last {
        if out.last().is_none_or(|prev| *prev < cursor) {
            out.push(cursor);
        }
        cursor = floor_local_ms(cursor.saturating_add(step), grain);
    }

    out
}

/// 线性插值分位（R-7，Excel/NumPy 同法）；`sorted` 必须升序。
fn quantile(sorted: &[i64], q: f64) -> i64 {
    match sorted.len() {
        0 => 0,
        1 => sorted[0],
        n => {
            let pos = q * (n - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(n - 1);
            let frac = pos - lo as f64;
            (sorted[lo] as f64 + frac * (sorted[hi] - sorted[lo]) as f64).round() as i64
        }
    }
}

/// 区间并集的覆盖总时长：先裁剪到 [lo, hi]，排序合并重叠后累加长度。
/// 并行会话的活跃段借此互不重复计时；倒挂窗口与空输入得 0。
fn union_duration_ms(mut intervals: Vec<(i64, i64)>, lo: i64, hi: i64) -> i64 {
    intervals.sort_unstable();
    let mut total = 0_i64;
    let mut merged: Option<(i64, i64)> = None;
    for (s, e) in intervals {
        let (s, e) = (s.max(lo), e.min(hi));
        if s >= e {
            continue;
        }
        match merged {
            Some((ms, me)) if s <= me => merged = Some((ms, me.max(e))),
            _ => {
                if let Some((ms, me)) = merged.replace((s, e)) {
                    total += me - ms;
                }
            }
        }
    }
    if let Some((ms, me)) = merged {
        total += me - ms;
    }
    total
}

/// 空库 / 脏窗口时的零值载荷：activity 仍给全 168 格（热力图按网格画）。
fn empty_dashboard(grain: &str) -> DashboardPayload {
    let mut activity = Vec::with_capacity(7 * 24);
    for day in 0..7 {
        for hour in 0..24 {
            activity.push(DashboardActivityCell {
                day,
                hour,
                cost_micros: 0,
                tokens: 0,
                calls: 0,
            });
        }
    }

    DashboardPayload {
        totals: DashboardTotals {
            cost_micros: 0,
            tokens: 0,
            calls: 0,
            sessions: 0,
            projects: 0,
            cache_read_tokens: 0,
            active_duration_ms: 0,
            active_tools: 0,
        },
        trend: Vec::new(),
        trend_grain: grain.to_string(),
        model_trend: Vec::new(),
        composition: Vec::new(),
        projects: Vec::new(),
        flow: Vec::new(),
        activity,
        daily: Vec::new(),
        spread: Vec::new(),
        sessions: Vec::new(),
        cost_composition: DashboardCostComposition {
            input_cost_micros: 0,
            output_cost_micros: 0,
            reasoning_cost_micros: 0,
            cache_read_cost_micros: 0,
            cache_write_cost_micros: 0,
        },
        unknown_model_count: 0,
    }
}

/// 全量重算成本（[`reprice_scoped`] 传 `None`）：扫描入库与手动全量重算走这里。
/// 单事务内调用；返回被改写（重算或清 NULL）的行数。
fn reprice(conn: &Connection) -> Result<usize> {
    reprice_scoped(conn, None)
}

/// 重算成本的两步：无价格行的模型先清全部分项，有价格行的按 token 分桶计价。
/// 对 [`FACT_TABLES`] 里每张表各跑一遍。`models = None` 不限范围；`Some(list)`
/// 只碰清单里的标准模型——单模型的定价/改指操作没有理由让全库事实表陪跑
/// （几万行无谓改写既慢，又把返回的"重算行数"撑成全库总量）。清单自动去重，
/// 空清单视为"没有受影响模型"，直接免跑。
///
/// 单事务内调用；返回被改写（重算或清 NULL）的行数。
fn reprice_scoped(conn: &Connection, models: Option<&[String]>) -> Result<usize> {
    let Some(list) = models else {
        return reprice_full(conn);
    };
    let mut uniq: Vec<&str> = list.iter().map(String::as_str).collect();
    uniq.sort_unstable();
    uniq.dedup();
    if uniq.is_empty() {
        return Ok(0);
    }
    let marks = vec!["?"; uniq.len()].join(",");
    let mut updated = 0;
    for table in FACT_TABLES {
        // 前置清 NULL：只限受影响模型里没有价格行的。
        updated += conn.execute(
            &format!(
                "UPDATE {table} SET
                     input_cost_micros = NULL,
                     output_cost_micros = NULL,
                     cache_read_cost_micros = NULL,
                     cache_write_cost_micros = NULL
                 WHERE model NOT IN (SELECT model_id FROM model_prices)
                   AND model IN ({marks})"
            ),
            params_from_iter(uniq.iter()),
        )?;
        // 分桶计价：绑定顺序 = 模板里的 4 个除数在前，IN 清单在后。
        let mut bind: Vec<Value> = vec![Value::Integer(MICROS_PER_MTK); 4];
        bind.extend(uniq.iter().map(|m| Value::Text((*m).to_string())));
        updated += conn.execute(
            &REPRICE_SQL_TEMPLATE
                .replace("{t}", table)
                .replace("{models}", &format!(" AND r.model IN ({marks})")),
            params_from_iter(bind.iter()),
        )?;
    }
    Ok(updated)
}

/// 全量版两步语句（无范围过滤），与 [`reprice_scoped`] 的受限版逐句同构。
fn reprice_full(conn: &Connection) -> Result<usize> {
    let mut updated = 0;
    for table in FACT_TABLES {
        updated += conn.execute(
            &format!(
                "UPDATE {table} SET
                     input_cost_micros = NULL,
                     output_cost_micros = NULL,
                     cache_read_cost_micros = NULL,
                     cache_write_cost_micros = NULL
                 WHERE model NOT IN (SELECT model_id FROM model_prices)"
            ),
            [],
        )?;
        updated += conn.execute(
            &REPRICE_SQL_TEMPLATE
                .replace("{t}", table)
                .replace("{models}", ""),
            params![
                MICROS_PER_MTK,
                MICROS_PER_MTK,
                MICROS_PER_MTK,
                MICROS_PER_MTK
            ],
        )?;
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// 每个测试独享一个临时库文件，避免相互干扰；退出前尽力清掉三件套（db/wal/shm）。
    fn test_db(tag: &str) -> (Storage, PathBuf) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toktol-storage-test-{}-{}-{tag}.db",
            std::process::id(),
            seq
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(path.with_extension(format!("db{suffix}")));
        }
        (open(&path).expect("打开测试库"), path)
    }

    /// 测试库三件套清理（Storage 已 drop，连接已关）。
    fn cleanup(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(path.with_extension(format!("db{suffix}")));
        }
    }

    fn record(session_id: Option<i64>, dedup: u8) -> NewUsageRecord {
        NewUsageRecord {
            session_id,
            tool: "claude-code".into(),
            model_raw: "claude-sonnet-4-5-20250929".into(),
            model: "claude-sonnet-4-5".into(),
            ts: 1_000,
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
            duration_ms: None,
            request_count: 1,
            dedup_key: vec![dedup],
        }
    }

    #[test]
    fn migrations_are_idempotent_and_wal_is_on() {
        let (storage, path) = test_db("migrate");
        let version: i64 = storage
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version as usize, migrations::MIGRATIONS.len());

        let journal: String = storage
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");

        drop(storage);
        let reopened = open(&path).expect("重开已迁移的库不应再跑迁移");
        let version2: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version2 as usize, migrations::MIGRATIONS.len());
        drop(reopened);
        cleanup(&path);
    }

    /// v13 把 opencode 库源的指纹置空：指纹不失效，"指纹未变→跳过"分支会
    /// 挡住 v12 删除行的重建（实测踩坑）。模拟库停在 v12 重开，v13 必须执行。
    #[test]
    fn v13_invalidates_opencode_fingerprint() {
        let (storage, path) = test_db("v13-fingerprint");
        storage
            .conn
            .execute(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
                 VALUES ('C:/opencode.db', 'opencode', x'ABCD', 1, 0, 1)",
                [],
            )
            .unwrap();
        // 库已停在最高版本；回拨到 v12 重开，只重跑 v13。
        storage
            .conn
            .execute("PRAGMA user_version = 12", [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let hash: Vec<u8> = reopened
            .conn
            .query_row(
                "SELECT content_hash FROM scanned_files WHERE tool = 'opencode'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(hash.is_empty(), "v13 必须把 opencode 指纹置空");
        // 其他工具不受影响——用另一行验证置空是定向的。
        reopened
            .conn
            .execute(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
                 VALUES ('C:/codex.jsonl', 'codex', x'EE', 1, 0, 1)",
                [],
            )
            .unwrap();
        let codex_hash: Vec<u8> = reopened
            .conn
            .query_row(
                "SELECT content_hash FROM scanned_files WHERE tool = 'codex'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(codex_hash, vec![0xEE]);
        drop(reopened);
        cleanup(&path);
    }

    /// v19 回填请求耗时：删两工具在册会话的用量行、游标归零重扫重建；已删
    /// 会话的孤儿行（统计保留的载体，墓碑会拦住重建）必须留下，其他工具不动。
    #[test]
    fn v19_deletes_linked_usage_and_resets_cursors_for_replay() {
        let (storage, path) = test_db("v19-replay");
        storage
            .conn
            .execute_batch(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                    ('C:/cc/s.jsonl',  'claude-code', x'AB', 10, 10, 1),
                    ('C:/cb/a.log',    'codebuddy',   x'CD', 10, 10, 1),
                    ('C:/z/r.jsonl',   'zcode',       x'EE', 10, 10, 1);
                 INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                    (1, 'claude-code', 'cc-live',    1, 1),
                    (2, 'claude-code', 'cc-deleted', 1, 1);
                 INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key) VALUES
                    (1,    'claude-code', 'm', 'm', 1, x'01'),
                    (NULL, 'claude-code', 'm', 'm', 2, x'02'),
                    (1,    'codebuddy',   'm', 'm', 3, x'03'),
                    (1,    'zcode',       'm', 'm', 4, x'04');",
            )
            .unwrap();
        // 库已停在最高版本；回拨到 v18 重开，v19 起的迁移依次重跑（v21 会删
        // codex 行，所以"未涉及工具"用 zcode 验证）。
        storage
            .conn
            .execute("PRAGMA user_version = 18", [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let rows: Vec<(String, Option<i64>)> = reopened
            .conn
            .prepare("SELECT tool, session_id FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![("claude-code".into(), None), ("zcode".into(), Some(1)),],
            "在册行删除重建，孤儿行与未涉及工具保留"
        );
        let cursors: Vec<(String, i64)> = reopened
            .conn
            .prepare("SELECT tool, parsed_bytes FROM scanned_files ORDER BY path")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            cursors,
            vec![
                ("codebuddy".into(), 0),
                ("claude-code".into(), 0),
                ("zcode".into(), 10),
            ],
            "两工具游标归零，其他工具不动"
        );
        drop(reopened);
        cleanup(&path);
    }

    /// v21 回填 pi/codex 请求耗时：删两工具在册会话的用量行、游标归零重扫
    /// 重建；已删会话的孤儿行（统计保留的载体）必须留下，其他工具不动
    /// （v22 会删 grok 在册行，"未涉及工具"用 zcode 验证）。
    #[test]
    fn v21_deletes_linked_usage_and_resets_cursors_for_replay() {
        let (storage, path) = test_db("v21-replay");
        storage
            .conn
            .execute_batch(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                    ('C:/pi/s.jsonl',    'pi',    x'AB', 10, 10, 1),
                    ('C:/codex/r.jsonl', 'codex', x'CD', 10, 10, 1),
                    ('C:/zcode/u.jsonl', 'zcode', x'EE', 10, 10, 1);
                 INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                    (1, 'pi', 'pi-live',    1, 1);
                 INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key) VALUES
                    (1,    'pi',    'm', 'm', 1, x'01'),
                    (NULL, 'pi',    'm', 'm', 2, x'02'),
                    (1,    'codex', 'm', 'm', 3, x'03'),
                    (1,    'zcode', 'm', 'm', 4, x'04');",
            )
            .unwrap();
        // 库已停在最高版本；回拨到 v20 重开，只重跑 v21。
        storage
            .conn
            .execute("PRAGMA user_version = 20", [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let rows: Vec<(String, Option<i64>)> = reopened
            .conn
            .prepare("SELECT tool, session_id FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![("pi".into(), None), ("zcode".into(), Some(1)),],
            "在册行删除重建，孤儿行与未涉及工具保留"
        );
        let cursors: Vec<(String, i64)> = reopened
            .conn
            .prepare("SELECT tool, parsed_bytes FROM scanned_files ORDER BY path")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            cursors,
            vec![("codex".into(), 0), ("pi".into(), 0), ("zcode".into(), 10),],
            "两工具游标归零，其他工具不动"
        );
        drop(reopened);
        cleanup(&path);
    }

    /// v22 落行级请求数：usage_records 重建后 request_count 列生效（存量默认
    /// 1）；grok 在册会话的用量行删除、游标归零待重扫拆分重建；孤儿行与
    /// 其他工具不动。
    #[test]
    fn v22_adds_request_count_and_resets_grok_for_replay() {
        let (storage, path) = test_db("v22-request-count");
        storage
            .conn
            .execute_batch(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                    ('C:/grok/u.jsonl',  'grok',  x'AB', 10, 10, 1),
                    ('C:/codex/r.jsonl', 'codex', x'CD', 10, 10, 1);
                 INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                    (1, 'grok', 'grok-live', 1, 1);
                 INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key) VALUES
                    (1,    'grok',  'm', 'm', 1, x'01'),
                    (NULL, 'grok',  'm', 'm', 2, x'02'),
                    (1,    'codex', 'm', 'm', 3, x'03');",
            )
            .unwrap();
        // 库已停在最高版本；回拨到 v21 重开，只重跑 v22。
        storage
            .conn
            .execute("PRAGMA user_version = 21", [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let rows: Vec<(String, Option<i64>, i64)> = reopened
            .conn
            .prepare("SELECT tool, session_id, request_count FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![("grok".into(), None, 1), ("codex".into(), Some(1), 1),],
            "grok 在册行删除待重扫，孤儿行保留，存量请求数默认 1"
        );
        let cursor: i64 = reopened
            .conn
            .query_row(
                "SELECT parsed_bytes FROM scanned_files WHERE tool = 'grok'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 0, "grok 游标归零，重扫按请求级拆分重建");
        drop(reopened);
        cleanup(&path);
    }

    /// v23 回填 grok 请求耗时：与 v22 同款重建（dedup 键不变，旧行锁死），
    /// 在册行删除待重扫、孤儿行保留、其他工具不动。
    #[test]
    fn v23_resets_grok_for_duration_replay() {
        let (storage, path) = test_db("v23-duration");
        storage
            .conn
            .execute_batch(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                    ('C:/grok/u.jsonl',  'grok',  x'AB', 10, 10, 1),
                    ('C:/codex/r.jsonl', 'codex', x'CD', 10, 10, 1);
                 INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                    (1, 'grok', 'grok-live', 1, 1);
                 INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key, request_count) VALUES
                    (1,    'grok',  'm', 'm', 1, x'01', 1),
                    (NULL, 'grok',  'm', 'm', 2, x'02', 1),
                    (1,    'codex', 'm', 'm', 3, x'03', 1);",
            )
            .unwrap();
        // 库已停在最高版本；回拨到 v22 重开，只重跑 v23。
        storage
            .conn
            .execute("PRAGMA user_version = 22", [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let rows: Vec<(String, Option<i64>)> = reopened
            .conn
            .prepare("SELECT tool, session_id FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![("grok".into(), None), ("codex".into(), Some(1)),],
            "grok 在册行删除待重扫，孤儿行与其他工具保留"
        );
        let cursor: i64 = reopened
            .conn
            .query_row(
                "SELECT parsed_bytes FROM scanned_files WHERE tool = 'grok'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor, 0, "grok 游标归零，重扫回填推导耗时");
        drop(reopened);
        cleanup(&path);
    }

    /// v16 清洗 codebuddy 统计行吃进来的尾部 `)` 模型名：净名同指的脏映射删除、
    /// 净名空缺的就地改名、指向分歧的保守保留；事实表只清 raw，model 列不动。
    #[test]
    fn v16_strips_trailing_paren_from_model_raws() {
        let (storage, path) = test_db("v16-paren");
        for id in ["hy4-preview", "solo-model", "model-a", "model-b", "orphan"] {
            storage
                .conn
                .execute(
                    "INSERT INTO models (id, display_name, first_seen_at) VALUES (?1, ?1, 1)",
                    params![id],
                )
                .unwrap();
        }
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, source)
                 VALUES ('hy4-preview', 845000, 'catalog')",
                [],
            )
            .unwrap();
        for (raw, id, source) in [
            ("hy4-preview-f", "hy4-preview", "user"),
            ("hy4-preview-f)", "hy4-preview", "user"),
            ("divergent", "model-b", "user"),
            ("divergent)", "model-a", "user"),
            ("solo-raw)", "solo-model", "auto"),
        ] {
            storage
                .conn
                .execute(
                    "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                     VALUES (?1, ?2, ?3, 1)",
                    params![raw, id, source],
                )
                .unwrap();
        }
        storage
            .conn
            .execute(
                "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
                 VALUES ('codebuddy', 'hy4-preview-f)', 'hy4-preview', 1, x'01'),
                        ('codebuddy', 'hy4-preview-f', 'hy4-preview', 2, x'02')",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO gateway_requests (ts, tool, model_raw, model)
                 VALUES (1, NULL, 'hy4-preview-f)', 'hy4-preview')",
                [],
            )
            .unwrap();

        // 库已停在最高版本；回拨到 v15 重开，只重跑 v16。
        storage
            .conn
            .execute("PRAGMA user_version = 15", [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let usage: Vec<(String, String)> = reopened
            .conn
            .prepare("SELECT model_raw, model FROM usage_records ORDER BY ts")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            usage,
            [
                ("hy4-preview-f".to_string(), "hy4-preview".to_string()),
                ("hy4-preview-f".to_string(), "hy4-preview".to_string()),
            ],
            "脏 raw 清洗，model 列与净行不动"
        );
        let gateway: String = reopened
            .conn
            .query_row("SELECT model_raw FROM gateway_requests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gateway, "hy4-preview-f");
        let mappings: Vec<(String, String)> = reopened
            .conn
            .prepare("SELECT raw_model, model_id FROM model_mappings ORDER BY raw_model")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            mappings,
            vec![
                ("divergent".to_string(), "model-b".to_string()),
                ("divergent)".to_string(), "model-a".to_string()),
                ("hy4-preview-f".to_string(), "hy4-preview".to_string()),
                ("solo-raw".to_string(), "solo-model".to_string()),
            ],
            "同指脏行删除、无净名脏行改名、指向分歧保留"
        );
        let orphan: i64 = reopened
            .conn
            .query_row("SELECT COUNT(*) FROM models WHERE id = 'orphan'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(orphan, 0, "失去引用的空壳模型删除");
        let kept: i64 = reopened
            .conn
            .query_row(
                "SELECT COUNT(*) FROM models
                 WHERE id IN ('hy4-preview','solo-model','model-a','model-b')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, 4, "仍被引用或挂价的模型保留");
        drop(reopened);
        cleanup(&path);
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let (storage, path) = test_db("fk");
        let mut rec = record(Some(9_999), 1);
        rec.dedup_key = vec![1, 0, 0];
        assert!(storage.insert_usage_record(&rec).is_err());
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn usage_insert_is_deduped_and_seeds_auto_mapping() {
        let (storage, path) = test_db("dedup");
        assert!(storage.insert_usage_record(&record(None, 1)).unwrap());
        assert!(!storage.insert_usage_record(&record(None, 1)).unwrap());

        let (model, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings
                 WHERE raw_model = 'claude-sonnet-4-5-20250929'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "claude-sonnet-4-5");
        assert_eq!(source, "auto");

        // 已有 auto 映射时，调用方建议值被映射覆盖——事实表的 model 与映射一致。
        let mut rec = record(None, 2);
        rec.model = "wrong-normalization".into();
        assert!(storage.insert_usage_record(&rec).unwrap());
        let got: String = storage
            .conn
            .query_row(
                "SELECT model FROM usage_records WHERE dedup_key = X'02'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(got, "claude-sonnet-4-5");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn delete_session_keeps_usage_and_shared_files() {
        let (storage, path) = test_db("delete");
        let file_a = storage
            .upsert_scanned_file("/logs/a.jsonl", "claude-code", &[0xAA], 10, 10, 0)
            .unwrap();
        let session = |external_id: &str, title: Option<&str>| NewSession {
            tool: "claude-code".into(),
            external_id: external_id.into(),
            external_id_is_derived: false,
            title: title.map(str::to_string),
            project_dir: None,
            source_file_id: file_a,
        };
        let (s1, created1) = storage
            .get_or_create_session(&session("sess-1", Some("t")), 0)
            .unwrap();
        assert!(created1);
        let (s2, created2) = storage
            .get_or_create_session(&session("sess-2", None), 0)
            .unwrap();
        assert!(created2);
        // 再取同一会话：命中已有行，不算新建。
        let (_, re_fetched) = storage
            .get_or_create_session(&session("sess-2", None), 0)
            .unwrap();
        assert!(!re_fetched);
        storage.insert_usage_record(&record(Some(s1), 1)).unwrap();
        storage.insert_usage_record(&record(Some(s2), 2)).unwrap();

        storage.delete_session(s1).unwrap();

        let orphan: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan, 1, "删除会话后用量必须保留");
        let sessions: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 1, "会话行删除，不留墓碑");
        let files: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM scanned_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "s2 仍引用该文件，簿记行必须保留");

        storage.delete_session(s2).unwrap();
        let files: i64 = storage
            .conn
            .query_row("SELECT COUNT(*) FROM scanned_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 0, "最后一个引用者删除后文件簿记行消失");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn user_mapping_beats_auto_and_recompute_follows() {
        let (storage, path) = test_db("mapping");
        storage.insert_usage_record(&record(None, 1)).unwrap();

        // 用户把该原始名改归一化到一个未定价模型。
        storage
            .upsert_model_mapping("claude-sonnet-4-5-20250929", "some-unpriced-model", 2_000)
            .unwrap();
        storage
            .apply_model_mapping_changes(&["claude-sonnet-4-5-20250929"])
            .unwrap();

        let (model, cost): (String, Option<i64>) = storage
            .conn
            .query_row(
                "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "some-unpriced-model", "事实表跟随 user 映射");
        assert_eq!(cost, None, "未定价模型总分必须是 NULL，绝不编造");

        // auto 无法覆盖 user。
        storage.insert_usage_record(&record(None, 3)).unwrap();
        let (model, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings
                 WHERE raw_model = 'claude-sonnet-4-5-20250929'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "some-unpriced-model");
        assert_eq!(source, "user");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn overview_filters_disabled_tools_without_touching_data() {
        let (storage, path) = test_db("overview-disabled");
        storage.insert_usage_record(&record(None, 1)).unwrap();
        // 另一个工具的记录。
        let mut other = record(None, 2);
        other.tool = "codex".into();
        other.dedup_key = vec![2];
        storage.insert_usage_record(&other).unwrap();

        let all = storage.overview(&[]).unwrap().totals;
        assert_eq!(all.record_count, 2);
        let filtered = storage.overview(&["claude-code".into()]).unwrap().totals;
        assert_eq!(filtered.record_count, 1, "禁用工具的记录不参与聚合");
        assert_eq!(filtered.input_tokens, 1_000, "只算 codex 那条");

        // 禁用不删数据：恢复后数字原样回来。
        assert_eq!(storage.overview(&[]).unwrap().totals.record_count, 2);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn overview_aggregates_known_cost_and_counts_unknown_rows() {
        let (storage, path) = test_db("overview");
        storage.insert_usage_record(&record(None, 1)).unwrap();
        let mut rec = record(None, 2);
        rec.dedup_key = vec![2];
        rec.model = "unpriced-model".into();
        rec.model_raw = "unpriced-raw".into();
        storage.insert_usage_record(&rec).unwrap();
        // model_prices 外键指向 models：先用量建档再挂价。
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        storage.recompute_costs().unwrap();

        let payload = storage.overview(&[]).unwrap();
        let totals = payload.totals;
        assert_eq!(totals.record_count, 2);
        assert_eq!(totals.session_count, 0, "record(None, …) 不建会话");
        assert_eq!(totals.input_tokens, 2_000);
        // 已知成本只含定价行；未知行数单独给，不混进总数。
        assert_eq!(totals.known_cost_micros, 10_500);
        assert_eq!(totals.unknown_cost_rows, 1);

        let by_model: Vec<(String, i64, i64)> = payload
            .by_model
            .iter()
            .map(|row| {
                (
                    row.model.clone(),
                    row.known_cost_micros,
                    row.unknown_cost_rows,
                )
            })
            .collect();
        assert_eq!(
            by_model,
            vec![
                ("claude-sonnet-4-5".into(), 10_500, 0),
                ("unpriced-model".into(), 0, 1),
            ],
            "按已知成本降序"
        );
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn dashboard_aggregates_slices_and_conserves_totals() {
        let (storage, path) = test_db("dashboard");

        // 会话归属：一个有项目、一个无项目（NULL → "" 哨兵）。usage_records 的
        // session 外键指向 sessions，先建归属再插记录。
        storage
            .conn
            .execute(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
                 VALUES ('/f1', 'claude-code', x'01', 1, 0, 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO sessions (tool, external_id, external_id_is_derived, project_dir, source_file_id, first_seen_at)
                 VALUES ('claude-code', 's1', 0, '/proj/a', 1, 1)",
                [],
            )
            .unwrap();
        let s1 = storage.conn.last_insert_rowid();
        storage
            .conn
            .execute(
                "INSERT INTO sessions (tool, external_id, external_id_is_derived, project_dir, source_file_id, first_seen_at)
                 VALUES ('claude-code', 's2', 0, NULL, 1, 1)",
                [],
            )
            .unwrap();
        let s2 = storage.conn.last_insert_rowid();

        // ts 用固定锚点 + 相对偏移：守恒型断言不依赖测试机的时区。
        // - 会话 s1 两条（相隔 1h → 时长 1h），有价模型；
        // - 会话 s2 一条（单条 → 时长 0），未定价模型 + 另一个工具，落在次日。
        const T0: i64 = 1_700_000_000_000;
        let mut r1 = record(Some(s1), 11);
        r1.ts = T0;
        storage.insert_usage_record(&r1).unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        let mut r2 = record(Some(s1), 12);
        // 距 r1 十分钟（≤ 30 分钟空闲阈值）→ 与 r1 同一活跃段。
        r2.ts = T0 + 600_000;
        r2.input_tokens = 2_000;
        r2.output_tokens = 1_000;
        let mut r3 = record(Some(s2), 13);
        r3.ts = T0 + 86_400_000;
        r3.tool = "codex".into();
        r3.model = "unpriced-model".into();
        r3.model_raw = "unpriced-raw".into();
        storage.insert_usage_record(&r2).unwrap();
        storage.insert_usage_record(&r3).unwrap();
        storage.recompute_costs().unwrap();

        let filters = UsageFilters {
            time_start: Some(T0),
            time_end: Some(T0 + 3 * 86_400_000),
            ..UsageFilters::default()
        };
        let payload = storage.dashboard(&filters, &[]).unwrap();

        // 总量：已知成本 = 10_500 + 21_000（未定价行不计）；五桶合计 6_000。
        let totals = payload.totals;
        assert_eq!(totals.calls, 3);
        assert_eq!(totals.cost_micros, 31_500);
        assert_eq!(totals.tokens, 6_000);
        assert_eq!(totals.sessions, 2);
        assert_eq!(totals.projects, 1, "无项目的会话不算项目");
        assert_eq!(totals.active_tools, 2);
        assert_eq!(
            totals.active_duration_ms, 600_000,
            "s1 一段 [T0, T0+10min]；r3 单条记录贡献 0"
        );
        assert_eq!(totals.cache_read_tokens, 0);

        // 会话消耗切片：s1（有价行）入切片；s2 只有未定价行 → SUM 全 NULL，被
        // HAVING 排除（全部未定价的会话没有费用可画）。
        assert_eq!(payload.sessions.len(), 1);
        let session = &payload.sessions[0];
        assert_eq!(session.tool, "claude-code");
        assert_eq!(session.external_id, "s1");
        assert_eq!(session.project_dir, "/proj/a");
        assert_eq!(session.model, "claude-sonnet-4-5");
        assert_eq!(session.calls, 2);
        assert_eq!(session.cost_micros, 31_500);
        assert_eq!(session.tokens, 4_500, "五桶合计 6_000 − 未定价行 1_500");
        assert_eq!(session.start_ms, T0, "首条记录 r1");
        assert_eq!(session.end_ms, T0 + 600_000, "末条记录 r2");

        // 趋势：窗口 3 天 → 天粒度 4 桶；守恒断言与时区无关。
        assert_eq!(payload.trend_grain, "day");
        assert_eq!(payload.trend.len(), 4);
        assert_eq!(payload.trend.iter().map(|b| b.calls).sum::<i64>(), 3);
        assert_eq!(
            payload.trend.iter().map(|b| b.cost_micros).sum::<i64>(),
            31_500
        );
        assert_eq!(payload.trend.iter().map(|b| b.tokens).sum::<i64>(), 6_000);
        // 未定价记录所在的桶：量照常、成本为 0。
        let rec3 = payload
            .trend
            .iter()
            .find(|b| b.calls == 1)
            .expect("r1/r2 同桶（2 次），r3 独占一桶");
        assert_eq!(rec3.cost_micros, 0);
        assert_eq!(rec3.tokens, 1_500);

        // 按模型趋势：模型数与构成一致，数组与 trend 桶对齐，总量守恒。
        assert_eq!(payload.model_trend.len(), 2);
        let claude = &payload.model_trend[0];
        assert_eq!(claude.model, "claude-sonnet-4-5");
        assert_eq!(claude.cost_micros.len(), payload.trend.len());
        assert_eq!(claude.cost_micros.iter().sum::<i64>(), 31_500);
        assert_eq!(claude.calls.iter().sum::<i64>(), 2);

        // 构成：已知成本降序；未定价模型标注 unknown。
        assert_eq!(
            payload
                .composition
                .iter()
                .map(|r| (r.label.as_str(), r.cost_micros, r.calls, r.unknown_cost))
                .collect::<Vec<_>>(),
            vec![
                ("claude-sonnet-4-5", 31_500, 2, false),
                ("unpriced-model", 0, 1, true),
            ]
        );
        assert_eq!(payload.unknown_model_count, 1);

        // 项目构成："" 是无项目哨兵。
        assert_eq!(
            payload
                .projects
                .iter()
                .map(|r| (r.label.as_str(), r.cost_micros, r.calls))
                .collect::<Vec<_>>(),
            vec![("/proj/a", 31_500, 2), ("", 0, 1)]
        );

        // 流向：工具 → 模型、模型 → 项目两段；未定价段成本 0 但量照常。
        let flow: Vec<(String, String, i64, i64)> = payload
            .flow
            .iter()
            .map(|l| (l.source.clone(), l.target.clone(), l.cost_micros, l.calls))
            .collect();
        assert_eq!(flow.len(), 4);
        assert!(flow.contains(&("claude-code".into(), "claude-sonnet-4-5".into(), 31_500, 2)));
        assert!(flow.contains(&("codex".into(), "unpriced-model".into(), 0, 1)));
        assert!(flow.contains(&("claude-sonnet-4-5".into(), "/proj/a".into(), 31_500, 2)));
        assert!(flow.contains(&("unpriced-model".into(), String::new(), 0, 1)));

        // 活跃热力图：恒 168 格，量与成本守恒。
        assert_eq!(payload.activity.len(), 168);
        assert_eq!(payload.activity.iter().map(|c| c.calls).sum::<i64>(), 3);
        assert_eq!(
            payload.activity.iter().map(|c| c.cost_micros).sum::<i64>(),
            31_500
        );

        // 日历：不吃时间筛选（两天各一行、日期升序）；守恒。
        assert_eq!(payload.daily.len(), 2);
        assert!(payload.daily.windows(2).all(|w| w[0].date < w[1].date));
        assert_eq!(payload.daily.iter().map(|d| d.calls).sum::<i64>(), 3);
        assert_eq!(
            payload.daily.iter().map(|d| d.cost_micros).sum::<i64>(),
            31_500
        );

        // 箱线图：只含有价模型；两样本 [10_500, 21_000] 的线性插值五数概括。
        assert_eq!(payload.spread.len(), 1);
        assert_eq!(payload.spread[0].label, "claude-sonnet-4-5");
        assert_eq!(
            payload.spread[0].cost_micros,
            vec![10_500, 13_125, 15_750, 18_375, 21_000]
        );

        // 瀑布：分项之和精确等于已知总成本。
        let cc = payload.cost_composition;
        assert_eq!(cc.input_cost_micros, 9_000);
        assert_eq!(cc.output_cost_micros, 22_500);
        assert_eq!(cc.reasoning_cost_micros, 0, "record() 不带推理量");
        assert_eq!(cc.cache_read_cost_micros, 0);
        assert_eq!(cc.cache_write_cost_micros, 0);
        assert_eq!(
            cc.input_cost_micros
                + cc.output_cost_micros
                + cc.reasoning_cost_micros
                + cc.cache_read_cost_micros
                + cc.cache_write_cost_micros,
            31_500
        );

        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn dashboard_empty_database_returns_zero_payload() {
        let (storage, path) = test_db("dashboard-empty");
        let payload = storage.dashboard(&UsageFilters::default(), &[]).unwrap();
        assert_eq!(payload.totals.calls, 0);
        assert!(payload.trend.is_empty());
        assert!(payload.composition.is_empty());
        assert!(payload.daily.is_empty());
        assert_eq!(payload.activity.len(), 168, "热力图按网格画，空数据补 0");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn dashboard_filters_and_disabled_narrow_slices() {
        let (storage, path) = test_db("dashboard-filters");
        storage
            .conn
            .execute(
                "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
                 VALUES ('/f1', 'claude-code', x'01', 1, 0, 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO sessions (tool, external_id, external_id_is_derived, project_dir, source_file_id, first_seen_at)
                 VALUES ('claude-code', 's1', 0, '/proj/a', 1, 1)",
                [],
            )
            .unwrap();
        let s1 = storage.conn.last_insert_rowid();
        let r1 = record(Some(s1), 21);
        let mut r2 = record(None, 22);
        r2.tool = "codex".into();
        storage.insert_usage_record(&r1).unwrap();
        storage.insert_usage_record(&r2).unwrap();

        // 工具筛选只留有会话归属的那条。
        let by_tool = storage
            .dashboard(
                &UsageFilters {
                    tools: vec!["claude-code".into()],
                    ..UsageFilters::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(by_tool.totals.calls, 1);
        assert_eq!(by_tool.totals.projects, 1);

        // 项目筛选的 "" 哨兵：孤儿行（无会话 → project_dir IS NULL）。
        let by_none = storage
            .dashboard(
                &UsageFilters {
                    projects: vec![String::new()],
                    ..UsageFilters::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(by_none.totals.calls, 1);
        assert_eq!(by_none.projects.len(), 1);
        assert_eq!(by_none.projects[0].label, "");

        // 禁用工具与筛选同效，但数据原样保留：恢复后数字回来。
        let disabled = storage
            .dashboard(&UsageFilters::default(), &["codex".to_string()])
            .unwrap();
        assert_eq!(disabled.totals.calls, 1);
        assert_eq!(
            storage
                .dashboard(&UsageFilters::default(), &[])
                .unwrap()
                .totals
                .calls,
            2
        );
        drop(storage);
        cleanup(&path);
    }

    // 前端"今天"预设的窗口（本地 0 点起、跨度不足 2 天）会命中 Hour 粒度分支，
    // 与默认 7 天的 Day 粒度走的是不同 SQL/桶序列，这里单独覆盖。
    #[test]
    fn dashboard_today_window_uses_hour_grain() {
        let (storage, path) = test_db("dashboard-today");
        let mut r = record(None, 31);
        r.ts = Local::now().timestamp_millis();
        storage.insert_usage_record(&r).unwrap();

        // 与前端 presetRange("today") 同窗口：本地 0 点起、当日 23:59:59.999 止。
        let now = Local::now();
        let today_start = Local
            .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
            .single()
            .expect("本地 0 点是唯一时刻")
            .timestamp_millis();
        let payload = storage
            .dashboard(
                &UsageFilters {
                    time_start: Some(today_start),
                    time_end: Some(today_start + 86_400_000 - 1),
                    ..UsageFilters::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(payload.trend_grain, "hour");
        assert_eq!(payload.trend.len(), 24);
        assert_eq!(payload.totals.calls, 1);
        // 趋势守恒：唯一一条记录恰好落进一个桶。
        assert_eq!(
            payload.trend.iter().map(|b| b.cost_micros).sum::<i64>(),
            payload.totals.cost_micros
        );
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn union_duration_merges_overlaps_and_clips() {
        // 重叠合并为一段。
        assert_eq!(union_duration_ms(vec![(0, 100), (50, 150)], 0, 1_000), 150);
        // 裁剪到窗口：窗口外部分不计入。
        assert_eq!(union_duration_ms(vec![(-100, 50), (80, 200)], 0, 100), 70);
        // 端点相接的区间合并；并行会话因此不重复计时。
        assert_eq!(union_duration_ms(vec![(0, 10), (10, 20)], 0, 100), 20);
        // 单点区间（一次调用）与空输入、倒挂窗口均为 0。
        assert_eq!(union_duration_ms(vec![(5, 5)], 0, 100), 0);
        assert_eq!(union_duration_ms(vec![], 0, 100), 0);
        assert_eq!(union_duration_ms(vec![(10, 20)], 30, 10), 0);
    }

    #[test]
    fn recompute_prices_cache_buckets_fall_back_to_input_price() {
        let (storage, path) = test_db("reprice");
        // 先有用量（它会给 models 建档），再挂价格——model_prices 外键指向 models。
        storage.insert_usage_record(&record(None, 1)).unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        storage.recompute_costs().unwrap();

        // input 1_000 × 3_000_000 = 3_000；output 500 × 15_000_000 = 7_500。
        let (input, output, total): (i64, i64, i64) = storage
            .conn
            .query_row(
                "SELECT input_cost_micros, output_cost_micros, cost_micros
                 FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((input, output, total), (3_000, 7_500, 10_500));

        // 缓存桶无单价但有量 → 按输入价计：10 × 3_000_000 = 30，总分不再未知。
        storage
            .conn
            .execute(
                "UPDATE usage_records SET cache_write_tokens = 10 WHERE dedup_key = X'01'",
                [],
            )
            .unwrap();
        storage.recompute_costs().unwrap();
        let (input, cache_write, total): (i64, i64, i64) = storage
            .conn
            .query_row(
                "SELECT input_cost_micros, cache_write_cost_micros, cost_micros
                 FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(cache_write, 30, "cache_write 无单价按输入价计");
        assert_eq!(total, 10_530, "缓存回落输入价后总分已知");
        assert_eq!(input, 3_000, "输入桶不受影响");

        // 输入价也缺：输入与缓存两桶一起 NULL、总分未知；已定价的输出分项保留。
        storage
            .conn
            .execute(
                "UPDATE model_prices SET input_per_mtok_micros = NULL
                 WHERE model_id = 'claude-sonnet-4-5'",
                [],
            )
            .unwrap();
        storage.recompute_costs().unwrap();
        let (input, cache_write, output, total): (
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<i64>,
        ) = storage
            .conn
            .query_row(
                "SELECT input_cost_micros, cache_write_cost_micros,
                            output_cost_micros, cost_micros
                     FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(input, None, "输入价缺失，输入桶 NULL");
        assert_eq!(cache_write, None, "输入价也缺时缓存桶才 NULL");
        assert_eq!(output, Some(7_500), "已定价桶的分项不受其它桶连坐");
        assert_eq!(total, None, "有量桶未定价，总分必须 NULL");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn reasoning_tokens_are_billed_as_output() {
        let (storage, path) = test_db("reasoning");
        storage.insert_usage_record(&record(None, 1)).unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "UPDATE usage_records SET reasoning_tokens = 200 WHERE dedup_key = X'01'",
                [],
            )
            .unwrap();
        storage.recompute_costs().unwrap();

        // (output 500 + reasoning 200) × 15_000_000 / 1e6 = 10_500；总分 = 分项之和。
        let (output, total): (i64, i64) = storage
            .conn
            .query_row(
                "SELECT output_cost_micros, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(output, 10_500, "推理量并入输出桶计价");
        assert_eq!(total, 3_000 + 10_500, "总分 = 分项之和");
        drop(storage);
        cleanup(&path);
    }

    fn gateway_request(model_raw: &str, model: &str, tool: Option<&str>) -> NewGatewayRequest {
        NewGatewayRequest {
            tool: tool.map(str::to_string),
            model_raw: model_raw.into(),
            model: model.into(),
            ts: 1_000,
            input_tokens: 1_000,
            output_tokens: 500,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
            status_code: Some(200),
            latency_ms: Some(10),
            upstream: Some("mock".into()),
            token_hash: Some(vec![7u8; 32]),
        }
    }

    #[test]
    fn gateway_insert_costs_in_the_same_transaction() {
        let (storage, path) = test_db("gateway-price");
        // 映射建档 + 挂价发生在入库前：insert 返回时成本必须已经落库（单事务）。
        storage
            .upsert_model_mapping("raw-x", "claude-sonnet-4-5", 0)
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();

        let request = gateway_request("raw-x", "wrong-suggestion", Some("codex"));
        let id = storage.insert_gateway_request(&request).unwrap();

        let (model, input, output, total): (String, i64, i64, Option<i64>) = storage
            .conn
            .query_row(
                "SELECT model, input_cost_micros, output_cost_micros, cost_micros
                 FROM v_gateway_cost WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(model, "claude-sonnet-4-5", "已有映射时建议值被覆盖");
        assert_eq!(
            (input, output),
            (3_000, 7_500),
            "成本在 insert 的同一事务内落库"
        );
        assert_eq!(total, Some(10_500));

        let payload = storage.gateway_overview().unwrap();
        assert_eq!(payload.totals.request_count, 1);
        assert_eq!(payload.totals.known_cost_micros, 10_500);
        assert_eq!(payload.totals.unknown_cost_rows, 0);
        assert_eq!(payload.totals.input_tokens, 1_000);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn gateway_unpriced_model_stays_null_and_is_reported_unknown() {
        let (storage, path) = test_db("gateway-null");
        let request = gateway_request("never-priced", "never-priced", None);
        storage.insert_gateway_request(&request).unwrap();

        let costs: (Option<i64>, Option<i64>, Option<i64>) = storage
            .conn
            .query_row(
                "SELECT input_cost_micros, output_cost_micros, cost_micros
                 FROM v_gateway_cost",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(costs, (None, None, None), "无价格绝不编造数字");

        let payload = storage.gateway_overview().unwrap();
        assert_eq!(payload.totals.known_cost_micros, 0);
        assert_eq!(payload.totals.unknown_cost_rows, 1);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn mapping_change_rewrites_gateway_rows_and_reprices() {
        let (storage, path) = test_db("gateway-remap");
        storage
            .upsert_model_mapping("raw-x", "claude-sonnet-4-5", 0)
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        storage
            .insert_gateway_request(&gateway_request(
                "raw-x",
                "claude-sonnet-4-5",
                Some("codex"),
            ))
            .unwrap();

        // 用户把映射改到新模型：同事务改写 gateway_requests 并按新模型重算。
        storage
            .upsert_model_mapping("raw-x", "claude-opus-4-1", 1)
            .unwrap();
        let changed = storage.apply_model_mapping_changes(&["raw-x"]).unwrap();
        assert!(changed > 0);

        let model: String = storage
            .conn
            .query_row("SELECT model FROM gateway_requests", [], |row| row.get(0))
            .unwrap();
        assert_eq!(model, "claude-opus-4-1");
        // 新模型无价格行：分项被清 NULL，走"未知"而不是沿用旧价。
        let input: Option<i64> = storage
            .conn
            .query_row(
                "SELECT input_cost_micros FROM gateway_requests",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(input, None);
        drop(storage);
        cleanup(&path);
    }

    fn catalog_entry(
        provider: &str,
        model_id: &str,
        status: Option<&str>,
        input: Option<f64>,
        output: Option<f64>,
    ) -> crate::pricing::catalog::CatalogEntry {
        crate::pricing::catalog::CatalogEntry {
            provider: provider.to_string(),
            model_id: model_id.to_string(),
            display_name: None,
            status: status.map(str::to_string),
            input_per_mtok_micros: input.map(|v| (v * 1_000_000.0) as i64),
            output_per_mtok_micros: output.map(|v| (v * 1_000_000.0) as i64),
            cache_read_per_mtok_micros: None,
            cache_write_per_mtok_micros: None,
            reasoning_per_mtok_micros: None,
        }
    }

    #[test]
    fn catalog_sync_fills_missing_and_respects_higher_sources() {
        let (storage, path) = test_db("catalog");
        // 建档：claude-sonnet-4-5（record 建议值）与 glm-4.6（经映射建档）。
        storage.insert_usage_record(&record(None, 1)).unwrap();
        storage
            .upsert_model_mapping("raw-glm", "glm-4.6", 1)
            .unwrap();

        // 同 id 两供应商：deprecated 的 azure 输给在售的 anthropic。
        storage
            .replace_catalog_entries(
                &[
                    catalog_entry(
                        "anthropic",
                        "claude-sonnet-4-5",
                        None,
                        Some(3.0),
                        Some(15.0),
                    ),
                    catalog_entry(
                        "azure",
                        "claude-sonnet-4-5",
                        Some("deprecated"),
                        Some(9.9),
                        Some(9.9),
                    ),
                    // 目录没给 cost：只进快照，绝不估价。
                    catalog_entry("zhipuai", "glm-4.6", None, None, None),
                ],
                100,
            )
            .unwrap();

        let (matched, priced, remapped) = storage.sync_catalog_prices().unwrap();
        assert_eq!(matched, 2, "glm-4.6 也在目录里（匹配），只是没价");
        assert_eq!(priced, 1);
        assert_eq!(remapped, 0, "映射已指向目录模型，无需补指");
        let (source, input): (String, i64) = storage
            .conn
            .query_row(
                "SELECT source, input_per_mtok_micros FROM model_prices
                 WHERE model_id = 'claude-sonnet-4-5'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((source.as_str(), input), ("catalog", 3_000_000));
        let glm_price: Option<i64> = storage
            .conn
            .query_row(
                "SELECT input_per_mtok_micros FROM model_prices WHERE model_id = 'glm-4.6'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert_eq!(glm_price, None, "目录没价绝不估价");

        // 用户改价后，目录刷新（价格变了）不覆盖 user 来源。
        storage
            .set_model_price(
                "claude-sonnet-4-5",
                Some(1_000_000),
                Some(2_000_000),
                None,
                None,
            )
            .unwrap();
        storage
            .replace_catalog_entries(
                &[catalog_entry(
                    "anthropic",
                    "claude-sonnet-4-5",
                    None,
                    Some(4.0),
                    Some(4.0),
                )],
                200,
            )
            .unwrap();
        let (_, priced, _) = storage.sync_catalog_prices().unwrap();
        assert_eq!(priced, 0, "user 价目录不碰");
        let (source, input): (String, i64) = storage
            .conn
            .query_row(
                "SELECT source, input_per_mtok_micros FROM model_prices
                 WHERE model_id = 'claude-sonnet-4-5'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((source.as_str(), input), ("user", 1_000_000));
        // 挂价即算：record 是 1000 in / 500 out，微美元 = tokens × 单价 / 1e6。
        let total: i64 = storage
            .conn
            .query_row(
                "SELECT cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 1_000 + 1_000, "set_model_price 后自动重算");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn set_model_mapping_repoints_facts_and_reprices() {
        let (storage, path) = test_db("remap-global");
        storage.insert_usage_record(&record(None, 1)).unwrap();
        storage
            .set_model_price(
                "claude-opus-4-1",
                Some(1_000_000),
                Some(1_000_000),
                None,
                None,
            )
            .unwrap();

        // 全局映射：record 的 raw 名改指 claude-opus-4-1，改写与重算同事务完成。
        let changed = storage
            .set_model_mapping("claude-sonnet-4-5-20250929", "claude-opus-4-1", 100)
            .unwrap();
        assert!(changed > 0);
        let (model, source, cost): (String, String, i64) = storage
            .conn
            .query_row(
                "SELECT r.model, m.source, v.cost_micros
                 FROM usage_records AS r
                 JOIN model_mappings AS m ON m.raw_model = r.model_raw
                 JOIN v_usage_cost AS v ON v.id = r.id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(model, "claude-opus-4-1");
        assert_eq!(source, "user");
        assert_eq!(cost, 1_000 + 500);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn ingest_resolves_via_catalog_with_heuristic_suggestions() {
        let (storage, path) = test_db("resolve");
        // 先放目录：deepseek-v4-flash（归一精确命中）与 gemini-2.5-pro（启发候选命中）。
        storage
            .replace_catalog_entries(
                &[
                    catalog_entry("deepseek", "deepseek-v4-flash", None, Some(1.0), Some(2.0)),
                    catalog_entry("google", "gemini-2.5-pro", None, Some(1.0), Some(2.0)),
                ],
                100,
            )
            .unwrap();

        // 空格写法归一后精确命中目录 → auto。
        let mut spacey = record(None, 1);
        spacey.model_raw = "DeepSeek V4 Flash".into();
        spacey.model = "DeepSeek V4 Flash".into();
        storage.insert_usage_record(&spacey).unwrap();

        // 日期尾缀是启发候选 → suggested（猜的，待用户过目）。
        let mut dated = record(None, 2);
        dated.model_raw = "gemini-2.5-pro-06-05".into();
        dated.model = "gemini-2.5-pro-06-05".into();
        storage.insert_usage_record(&dated).unwrap();

        let (id, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings
                 WHERE raw_model = 'DeepSeek V4 Flash'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (id.as_str(), source.as_str()),
            ("deepseek-v4-flash", "auto")
        );

        let (id, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings
                 WHERE raw_model = 'gemini-2.5-pro-06-05'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (id.as_str(), source.as_str()),
            ("gemini-2.5-pro", "suggested")
        );
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn sync_remaps_orphans_via_heuristic_and_confirm_pins() {
        let (storage, path) = test_db("remap-sync");
        // 直插 SQL 复刻遗留状态：自映射壳 raw → 自己（新入库路径已会剥尾缀）。
        storage
            .conn
            .execute(
                "INSERT INTO models (id, display_name, first_seen_at)
                 VALUES ('gpt-4-0613', 'gpt-4-0613', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                 VALUES ('gpt-4-0613', 'gpt-4-0613', 'auto', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO usage_records (tool, model_raw, model, ts,
                     input_tokens, output_tokens, dedup_key)
                 VALUES ('claude-code', 'gpt-4-0613', 'gpt-4-0613', 1, 1000, 500, X'01')",
                [],
            )
            .unwrap();

        // 目录同步带来 gpt-4：启发候选命中 → 映射改指（suggested）+ 事实改写 + 挂价重算。
        let payload = storage
            .apply_catalog(&[catalog_entry(
                "openai",
                "gpt-4",
                None,
                Some(30.0),
                Some(60.0),
            )])
            .unwrap();
        assert_eq!(payload.remapped, 1);
        assert_eq!(payload.priced, 1);

        let (id, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings WHERE raw_model = 'gpt-4-0613'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((id.as_str(), source.as_str()), ("gpt-4", "suggested"));
        let (model, cost): (String, Option<i64>) = storage
            .conn
            .query_row(
                "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "gpt-4", "事实表跟着映射改写");
        assert_eq!(cost, Some(60_000), "30 USD/M × 1000 + 60 USD/M × 500");

        // 确认：suggested → user；之后目录刷新价，映射与价刷新照常，来源不再变。
        assert_eq!(storage.confirm_model("gpt-4", 300).unwrap(), 1);
        let payload = storage
            .apply_catalog(&[catalog_entry(
                "openai",
                "gpt-4",
                None,
                Some(31.0),
                Some(60.0),
            )])
            .unwrap();
        assert_eq!(payload.remapped, 0, "user 与已指向目录的映射不再补指");
        let source: String = storage
            .conn
            .query_row(
                "SELECT source FROM model_mappings WHERE raw_model = 'gpt-4-0613'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(source, "user", "确认钉住，同步不回退");
        let cost: i64 = storage
            .conn
            .query_row(
                "SELECT cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cost, 61_000, "catalog 价刷新后重算");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn ingest_strips_free_suffix_without_catalog() {
        let (storage, path) = test_db("free-strip");
        // 目录为空：修饰尾缀照样剥到母型号，标 suggested 待用户过目。
        let mut free = record(None, 1);
        free.model_raw = "mimo-v2.5-free".into();
        free.model = "mimo-v2.5-free".into();
        storage.insert_usage_record(&free).unwrap();
        let (id, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings WHERE raw_model = 'mimo-v2.5-free'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((id.as_str(), source.as_str()), ("mimo-v2.5", "suggested"));
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn normalize_merges_legacy_self_shells_but_respects_user() {
        let (storage, path) = test_db("normalize");
        // 遗留自映射壳：老版本建档 raw → 自己（auto），用量也挂在壳下。
        // 当前的入库路径已会归一，只能直插 SQL 复刻老库状态。
        storage
            .conn
            .execute(
                "INSERT INTO models (id, display_name, first_seen_at)
                 VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                 VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 'auto', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
                 VALUES ('claude-code', 'z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1, X'01')",
                [],
            )
            .unwrap();
        // 第二类遗留：标准名自带 -free 尾缀（老版本没有剥尾缀逻辑）。
        storage
            .conn
            .execute(
                "INSERT INTO models (id, display_name, first_seen_at)
                 VALUES ('mimo-v2.5-free', 'mimo-v2.5-free', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                 VALUES ('mimo-v2.5-free', 'mimo-v2.5-free', 'auto', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
                 VALUES ('zcode', 'mimo-v2.5-free', 'mimo-v2.5-free', 1, X'02')",
                [],
            )
            .unwrap();

        // user 自映射是用户意志：不许动。
        storage
            .upsert_model_mapping("custom-x", "custom-x", 1)
            .unwrap();

        assert_eq!(storage.normalize_auto_mappings().unwrap(), 2);

        let (model_id, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings
                 WHERE raw_model = 'z-ai/glm-5.3-flash'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (model_id.as_str(), source.as_str()),
            ("glm-5.3-flash", "auto")
        );
        let fact_model: String = storage
            .conn
            .query_row(
                "SELECT model FROM usage_records WHERE dedup_key = X'01'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fact_model, "glm-5.3-flash", "事实表跟着改写");
        let (free_id, free_source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings WHERE raw_model = 'mimo-v2.5-free'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (free_id.as_str(), free_source.as_str()),
            ("mimo-v2.5", "suggested"),
            "free 尾缀剥到母型号，标启发"
        );
        let shell: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM models WHERE id = 'z-ai/glm-5.3-flash'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(shell, 0, "空壳模型行删除");
        let user_shell: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM models WHERE id = 'custom-x'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(user_shell, 1, "user 自映射与它的模型行保留");
        // 幂等：再跑一遍不再有改动。
        assert_eq!(storage.normalize_auto_mappings().unwrap(), 0);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn sync_repoints_legacy_self_shell_without_catalog_evidence() {
        let (storage, path) = test_db("remap-self");
        // 直插 SQL 复刻老库的自映射壳（当前入库路径已会归一，造不出这种行）。
        storage
            .conn
            .execute(
                "INSERT INTO models (id, display_name, first_seen_at)
                 VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                 VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 'auto', 1)",
                [],
            )
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
                 VALUES ('claude-code', 'z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1, X'01')",
                [],
            )
            .unwrap();

        let payload = storage
            .apply_catalog(&[catalog_entry(
                "zhipuai",
                "glm-5.3-flash",
                None,
                Some(2.0),
                Some(8.0),
            )])
            .unwrap();
        assert_eq!(payload.remapped, 1, "自映射壳借格式归一改指");
        assert_eq!(payload.priced, 1);
        let (model_id, source): (String, String) = storage
            .conn
            .query_row(
                "SELECT model_id, source FROM model_mappings
                 WHERE raw_model = 'z-ai/glm-5.3-flash'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (model_id.as_str(), source.as_str()),
            ("glm-5.3-flash", "auto")
        );
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn merge_models_moves_mappings_facts_and_price() {
        let (storage, path) = test_db("merge");
        storage.insert_usage_record(&record(None, 1)).unwrap();
        // raw 名现指 a-model；b-model 已有 user 价。
        storage
            .set_model_mapping("claude-sonnet-4-5-20250929", "a-model", 100)
            .unwrap();
        storage
            .set_model_price("b-model", Some(2_000_000), None, None, None)
            .unwrap();

        storage.merge_models("a-model", "b-model").unwrap();

        let (model, cost): (String, Option<i64>) = storage
            .conn
            .query_row(
                "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "b-model", "事实表改写到并入方");
        assert_eq!(cost, None, "并入方已有的价保留，不是 a-model 的");
        let mapping_target: String = storage
            .conn
            .query_row(
                "SELECT model_id FROM model_mappings
                 WHERE raw_model = 'claude-sonnet-4-5-20250929'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapping_target, "b-model");
        let a_left: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM models WHERE id = 'a-model'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(a_left, 0, "被并入的壳必须消失");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn rename_model_merges_into_catalog_name_and_fills_price() {
        let (storage, path) = test_db("rename");
        // 本地模型 hy4-preview-f：一条用量 + 一个自映射变种，无价格行。
        let mut rec = record(None, 1);
        rec.model_raw = "hy4-preview-f".into();
        rec.model = "hy4-preview-f".into();
        storage.insert_usage_record(&rec).unwrap();
        storage
            .set_model_mapping("hy4-preview-f", "hy4-preview-f", 100)
            .unwrap();
        // 目录快照里有目标规范名（带价），同步从未跑过也照补。
        storage
            .replace_catalog_entries(
                &[catalog_entry(
                    "zhipuai",
                    "glm-5.3",
                    None,
                    Some(1.0),
                    Some(2.0),
                )],
                200,
            )
            .unwrap();

        storage.rename_model("hy4-preview-f", "glm-5.3").unwrap();

        let (model, cost): (String, Option<i64>) = storage
            .conn
            .query_row(
                "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(model, "glm-5.3", "事实改写到新标准名");
        assert_eq!(
            cost,
            Some(1_000 + 1_000),
            "改名后目录价立即生效：1000×$1/M + 500×$2/M"
        );
        let mapping: String = storage
            .conn
            .query_row(
                "SELECT model_id FROM model_mappings WHERE raw_model = 'hy4-preview-f'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(mapping, "glm-5.3");
        let shell: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM models WHERE id = 'hy4-preview-f'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(shell, 0, "旧名的壳删除");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn pricing_overview_links_catalog_by_model_id_and_lists_candidates() {
        let (storage, path) = test_db("pricing-overview");
        let mut rec = record(None, 1);
        rec.model_raw = "hy4-preview-f".into();
        rec.model = "hy4-preview-f".into();
        storage.insert_usage_record(&rec).unwrap();
        storage
            .set_model_mapping("hy4-preview-f", "hy4-preview-f", 100)
            .unwrap();
        // provider 与 model_id 完全不同：目录若错按 provider 键关联必然 miss。
        storage
            .replace_catalog_entries(
                &[catalog_entry(
                    "zhipuai",
                    "hy4-preview-f",
                    None,
                    Some(1.0),
                    Some(2.0),
                )],
                200,
            )
            .unwrap();

        let payload = storage.pricing_overview().unwrap();

        let row = payload
            .models
            .iter()
            .find(|m| m.id == "hy4-preview-f")
            .expect("本地模型在列表里");
        let hint = row.catalog.as_ref().expect("目录按 model_id 关联");
        assert_eq!(hint.provider, "zhipuai");
        let brief = payload
            .catalog_models
            .iter()
            .find(|b| b.model_id == "hy4-preview-f")
            .expect("改名候选里是真实的目录模型 id");
        assert_eq!(brief.provider, "zhipuai");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn gateway_requests_page_orders_desc_and_counts_errors() {
        let (storage, path) = test_db("gateway-page");
        let mut request = gateway_request("raw-x", "claude-sonnet-4-5", None);
        storage
            .upsert_model_mapping("raw-x", "claude-sonnet-4-5", 0)
            .unwrap();
        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        storage.insert_gateway_request(&request).unwrap();
        // 第二条：更晚的时间、非成功状态。
        request.ts = 2_000;
        request.status_code = Some(502);
        request.model_raw = "raw-y".into();
        request.model = "raw-y".into(); // tool=None：建议值直接生效，raw-y 无价格。
        storage.insert_gateway_request(&request).unwrap();

        let totals = storage.gateway_overview().unwrap().totals;
        assert_eq!(totals.request_count, 2);
        assert_eq!(totals.error_count, 1, "502 计入错误数");

        let page = storage.gateway_requests_page(0, 1).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].ts, 2_000, "时间倒序");
        assert_eq!(page.rows[0].status_code, Some(502));
        assert_eq!(page.rows[0].cost_micros, None, "raw-y 无价格，成本未知");

        let page2 = storage.gateway_requests_page(1, 1).unwrap();
        assert_eq!(page2.rows.len(), 1);
        assert_eq!(page2.rows[0].ts, 1_000);
        assert_eq!(
            page2.rows[0].cost_micros,
            Some(3_000 + 7_500),
            "已有价格行的成本走视图"
        );
        drop(storage);
        cleanup(&path);
    }

    /// 明细页数据：会话内定价记录（ts 1000，四桶 1000/500/0/0，耗时 1500ms）
    /// + 孤儿未定价记录（ts 2000，tool codex，tokens 10/5，无会话无耗时）。
    fn usage_page_fixture(storage: &Storage) {
        let file_id = storage
            .upsert_scanned_file("/logs/a.jsonl", "claude-code", &[0xAA], 10, 10, 0)
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "claude-code".into(),
                    external_id: "sess-1".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: Some("E:/p".into()),
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let mut rec = record(Some(session_id), 1);
        rec.ts = 1_000;
        rec.duration_ms = Some(1_500);
        storage.insert_usage_record(&rec).unwrap();

        let mut orphan = record(None, 2);
        orphan.tool = "codex".into();
        orphan.model = "unpriced".into();
        orphan.model_raw = "unpriced-raw".into();
        orphan.ts = 2_000;
        orphan.input_tokens = 10;
        orphan.output_tokens = 5;
        storage.insert_usage_record(&orphan).unwrap();

        storage
            .conn
            .execute(
                "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
                 VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
                [],
            )
            .unwrap();
        storage.recompute_costs().unwrap();
    }

    #[test]
    fn usage_records_page_joins_sessions_and_views() {
        let (storage, path) = test_db("usage-page");
        usage_page_fixture(&storage);

        let page = storage
            .usage_records_page(&UsageRecordsQuery {
                sort_desc: true,
                limit: 10,
                ..UsageRecordsQuery::default()
            })
            .unwrap();
        assert_eq!(page.total, 2);

        // 时间倒序：孤儿记录（ts 2000）在前，会话归属为 NULL。
        let orphan = &page.rows[0];
        assert_eq!(orphan.tool, "codex");
        assert_eq!(orphan.session_external_id, None);
        assert_eq!(orphan.project_dir, None);
        assert_eq!(orphan.cost_micros, None, "未定价模型总分 NULL");
        assert_eq!(orphan.duration_ms, None);

        let priced = &page.rows[1];
        assert_eq!(priced.session_external_id.as_deref(), Some("sess-1"));
        assert_eq!(priced.project_dir.as_deref(), Some("E:/p"));
        assert_eq!(priced.cost_micros, Some(10_500), "成本走视图口径");
        assert_eq!(
            (priced.input_cost_micros, priced.output_cost_micros),
            (Some(3_000), Some(7_500)),
            "分桶费用由 IPC 直出，前端不再推导"
        );
        assert_eq!(priced.duration_ms, Some(1_500), "duration 经扫描入库存原样");
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn usage_records_page_filters_and_sorts_server_side() {
        let (storage, path) = test_db("usage-filters");
        usage_page_fixture(&storage);

        let query = |f: UsageFilters| {
            storage.usage_records_page(&UsageRecordsQuery {
                filters: f,
                limit: 10,
                ..UsageRecordsQuery::default()
            })
        };

        // 工具 / 模型 / 项目（含无项目哨兵）/ 搜索 / 时间窗。
        assert_eq!(
            query(UsageFilters {
                tools: vec!["codex".into()],
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1
        );
        assert_eq!(
            query(UsageFilters {
                models: vec!["unpriced".into()],
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1
        );
        assert_eq!(
            query(UsageFilters {
                projects: vec!["E:/p".into()],
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1,
            "有项目只中会话行"
        );
        assert_eq!(
            query(UsageFilters {
                projects: vec![String::new()],
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1,
            "空串哨兵只中孤儿行"
        );
        assert_eq!(
            query(UsageFilters {
                projects: vec![String::new(), "E:/p".into()],
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            2,
            "哨兵与具名项目可并用"
        );
        assert_eq!(
            query(UsageFilters {
                search: Some("sess".into()),
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1,
            "按会话 id 搜索"
        );
        assert_eq!(
            query(UsageFilters {
                search: Some("E:/p".into()),
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1,
            "按项目目录搜索"
        );
        assert_eq!(
            query(UsageFilters {
                search: Some("no-such-thing".into()),
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            0
        );
        assert_eq!(
            query(UsageFilters {
                time_start: Some(1_500),
                ..UsageFilters::default()
            })
            .unwrap()
            .total,
            1,
            "时间下界含端点"
        );

        // 排序：tokens DESC 量级大者在前；cost ASC 时未知（NULL）在最前。
        let by_tokens = storage
            .usage_records_page(&UsageRecordsQuery {
                sort_key: Some("tokens".into()),
                sort_desc: true,
                limit: 10,
                ..UsageRecordsQuery::default()
            })
            .unwrap();
        assert_eq!(by_tokens.rows[0].input_tokens, 1_000, "tokens 降序");

        let by_cost = storage
            .usage_records_page(&UsageRecordsQuery {
                sort_key: Some("cost".into()),
                sort_desc: false,
                limit: 10,
                ..UsageRecordsQuery::default()
            })
            .unwrap();
        assert_eq!(by_cost.rows[0].cost_micros, None, "升序时未知成本在最前");

        // 禁用工具与读路径其它入口同一语义：不出现，但不删数据。
        let disabled = storage
            .usage_records_page(&UsageRecordsQuery {
                disabled: vec!["claude-code".into()],
                limit: 10,
                ..UsageRecordsQuery::default()
            })
            .unwrap();
        assert_eq!(disabled.total, 1);
        assert_eq!(disabled.rows[0].tool, "codex");

        // 分页：limit 1 翻页能取全两条。
        let p1 = storage
            .usage_records_page(&UsageRecordsQuery {
                sort_desc: true,
                limit: 1,
                ..UsageRecordsQuery::default()
            })
            .unwrap();
        let p2 = storage
            .usage_records_page(&UsageRecordsQuery {
                sort_desc: true,
                offset: 1,
                limit: 1,
                ..UsageRecordsQuery::default()
            })
            .unwrap();
        assert_eq!(p1.rows.len() + p2.rows.len(), 2);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn usage_filter_options_faceted_counts_and_respect_disabled() {
        let (storage, path) = test_db("usage-options");
        usage_page_fixture(&storage);

        let options = storage
            .usage_filter_options(&UsageFilters::default(), &[])
            .unwrap();
        assert_eq!(
            options
                .tools
                .iter()
                .map(|o| (o.value.clone(), o.count))
                .collect::<Vec<_>>(),
            vec![(Some("claude-code".into()), 1), (Some("codex".into()), 1),],
            "计数相同时按名字升序"
        );
        assert!(
            options
                .projects
                .iter()
                .any(|o| o.value.is_none() && o.count == 1),
            "孤儿行以无项目选项出现"
        );
        assert_eq!(options.projects.len(), 2);

        // 分面口径：筛选模型后，工具/项目计数只剩命中该模型的记录，
        // 而模型维度自身照常列出全部选项（不含本维度筛选）。
        let by_model = storage
            .usage_filter_options(
                &UsageFilters {
                    models: vec!["unpriced".into()],
                    ..UsageFilters::default()
                },
                &[],
            )
            .unwrap();
        assert_eq!(
            by_model
                .tools
                .iter()
                .map(|o| (o.value.clone(), o.count))
                .collect::<Vec<_>>(),
            vec![(Some("codex".into()), 1)],
            "工具计数跟随模型筛选"
        );
        assert_eq!(
            by_model
                .projects
                .iter()
                .map(|o| (o.value.clone(), o.count))
                .collect::<Vec<_>>(),
            vec![(None, 1)],
            "项目计数跟随模型筛选，孤儿行即无项目"
        );
        assert_eq!(by_model.models.len(), 2, "模型维度不含自身筛选");

        let without_codex = storage
            .usage_filter_options(&UsageFilters::default(), &["codex".into()])
            .unwrap();
        assert_eq!(
            without_codex
                .tools
                .iter()
                .map(|o| o.value.clone())
                .collect::<Vec<_>>(),
            vec![Some("claude-code".into())],
            "禁用工具的记录不进选项"
        );
        assert_eq!(
            without_codex.projects.len(),
            1,
            "无项目选项随孤儿行一起消失（孤儿属于 codex）"
        );
        drop(storage);
        cleanup(&path);
    }

    /// 建两个会话：s1 有两条用量（其一未定价 → 成本未知），s2 只有会话壳。
    fn seed_sessions(storage: &Storage) -> (i64, i64) {
        let file_id = storage
            .upsert_scanned_file("/logs/a.jsonl", "claude-code", &[1], 3, 3, 0)
            .unwrap();
        let (s1, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "claude-code".into(),
                    external_id: "s1".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: Some("E:/proj/demo".into()),
                    source_file_id: file_id,
                },
                100,
            )
            .unwrap();
        storage
            .update_session_activity(s1, 2_000, Some("修 bug"))
            .unwrap();
        let (s2, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "codex".into(),
                    external_id: "s2".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                200,
            )
            .unwrap();
        storage
            .update_session_activity(s2, 3_000, Some("codex 会话"))
            .unwrap();
        storage
            .insert_usage_record(&NewUsageRecord {
                session_id: Some(s1),
                tool: "claude-code".into(),
                model_raw: "m".into(),
                model: "m".into(),
                ts: 1_500,
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 10,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                duration_ms: None,
                request_count: 1,
                dedup_key: vec![1],
            })
            .unwrap();
        // 未定价模型：聚合里进 unknown_cost_rows，不冒充已知成本。
        storage
            .insert_usage_record(&NewUsageRecord {
                session_id: Some(s1),
                tool: "claude-code".into(),
                model_raw: "unpriced".into(),
                model: "unpriced".into(),
                ts: 1_800,
                input_tokens: 200,
                output_tokens: 20,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                duration_ms: None,
                request_count: 1,
                dedup_key: vec![2],
            })
            .unwrap();
        (s1, s2)
    }

    #[test]
    fn sessions_page_aggregates_usage_per_session() {
        let (storage, path) = test_db("sessions-agg");
        let (s1, s2) = seed_sessions(&storage);

        let page = storage
            .sessions_page(&SessionsPageQuery {
                sort_desc: true,
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.total, 2);
        // 默认按最近活跃倒序：s2（3_000）在前。
        assert_eq!(page.rows[0].id, s2);
        assert_eq!(page.rows[0].request_count, 0, "零用量壳保留，聚合为 0");

        let row = &page.rows[1];
        assert_eq!(row.id, s1);
        assert_eq!(row.title.as_deref(), Some("修 bug"));
        assert_eq!(row.request_count, 2);
        assert_eq!(row.input_tokens, 300);
        assert_eq!(row.output_tokens, 70);
        assert_eq!(row.cache_read_tokens, 10);

        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn sessions_page_filters_search_and_disabled() {
        let (storage, path) = test_db("sessions-filter");
        seed_sessions(&storage);

        // 搜索命中标题。
        let hit = storage
            .sessions_page(&SessionsPageQuery {
                filters: SessionFilters {
                    search: Some("修 bug".into()),
                    ..Default::default()
                },
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hit.total, 1);
        assert_eq!(hit.rows[0].external_id, "s1");

        // 搜索命中项目目录。
        let by_project = storage
            .sessions_page(&SessionsPageQuery {
                filters: SessionFilters {
                    search: Some("E:/proj/demo".into()),
                    ..Default::default()
                },
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_project.total, 1);

        // 工具白名单。
        let codex_only = storage
            .sessions_page(&SessionsPageQuery {
                filters: SessionFilters {
                    tools: vec!["codex".into()],
                    ..Default::default()
                },
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(codex_only.total, 1);
        assert_eq!(codex_only.rows[0].external_id, "s2");

        // 禁用工具整体隐藏。
        let disabled = storage
            .sessions_page(&SessionsPageQuery {
                disabled: vec!["codex".into()],
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(disabled.total, 1);
        assert_eq!(disabled.rows[0].external_id, "s1");

        // 时间窗落在 s2 的最近活跃之后：只剩 s2。
        let recent = storage
            .sessions_page(&SessionsPageQuery {
                filters: SessionFilters {
                    time_start: Some(2_500),
                    ..Default::default()
                },
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(recent.total, 1);
        assert_eq!(recent.rows[0].external_id, "s2");

        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn sessions_page_sorts_by_whitelisted_keys_and_paginates() {
        let (storage, path) = test_db("sessions-sort");
        let (s1, _s2) = seed_sessions(&storage);

        // 按请求数排：s1（2 条）在前。
        let by_requests = storage
            .sessions_page(&SessionsPageQuery {
                sort_key: Some("requests".into()),
                sort_desc: true,
                limit: 1,
                offset: 0,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_requests.rows.len(), 1);
        assert_eq!(by_requests.rows[0].id, s1, "按请求数降序 s1 在前");

        // 白名单外的键退回最近活跃；分页偏移生效。
        let second_page = storage
            .sessions_page(&SessionsPageQuery {
                sort_key: Some("hacker".into()),
                sort_desc: true,
                limit: 1,
                offset: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(second_page.total, 2);
        assert_eq!(second_page.rows[0].id, s1, "活跃倒序的第二位是 s1");

        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn marking_file_missing_clears_its_transcript_index() {
        let (storage, path) = test_db("missing-index");
        let file_id = storage
            .upsert_scanned_file("/x/model-io-sess_a.jsonl", "zcode", b"h", 10, 10, 1)
            .unwrap();

        // 提交两条索引条目 + 建站状态，模拟已建好索引的文件。
        storage
            .commit_transcript_index(
                file_id,
                &[NewTranscriptEntry {
                    seq: 0,
                    role: "user".into(),
                    ts_ms: Some(1),
                    model: None,
                    kind: 0,
                    frag_offset: 0,
                    frag_len: 5,
                    skip_blocks: None,
                    frag_meta: None,
                }],
                &TranscriptIndexState {
                    built_offset: 10,
                    size: 10,
                    mtime_ms: 1,
                    complete: true,
                    baseline_offset: None,
                    baseline_len: None,
                    pending_offset: None,
                    pending_len: None,
                    pending_ts: None,
                    pending_model: None,
                },
            )
            .unwrap();

        // 标记消失：索引条目与状态必须一并清空——源没了，字节区间无法解引用。
        storage.set_file_missing(file_id, Some(99)).unwrap();
        let entries: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_entries WHERE file_id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(entries, 0, "文件消失后索引条目必须被清理");
        assert!(
            storage.transcript_index_state(file_id).unwrap().is_none(),
            "文件消失后建站状态必须被清理"
        );
        let missing: Option<i64> = storage
            .conn
            .query_row(
                "SELECT missing_since FROM scanned_files WHERE id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(missing, Some(99), "消失标记本身必须保留");

        // 清除消失标记（文件失而复得）：不再有索引行可清，状态保持"未建站"。
        storage.set_file_missing(file_id, None).unwrap();
        assert!(storage.transcript_index_state(file_id).unwrap().is_none());
        let missing: Option<i64> = storage
            .conn
            .query_row(
                "SELECT missing_since FROM scanned_files WHERE id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(missing, None);

        // 重复标记已消失的文件：幂等，不报错。
        storage.set_file_missing(file_id, Some(100)).unwrap();

        drop(storage);
        cleanup(&path);
    }
}
