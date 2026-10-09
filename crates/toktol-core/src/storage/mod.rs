//! SQLite 存储层：唯一主库（WAL）。本文件只保留载荷类型、连接打开与迁移引导、
//! 跨域共享的 SQL 辅助函数；各查询/写入域拆在同名子模块里。
//! 表结构与删除/重算流程的设计依据就近写在各 migration 的注释里。
//! 双计陷阱：`usage_records`（扫描）与 `gateway_requests`（网关）可能记到同一请求，
//! 汇总查询绝不能盲目相加——两表各自聚合，归属规则未拍板前不合并。

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

mod gateway;
mod ingest;
mod migrations;
mod overview;
mod pricing;
mod scan;
mod sessions;
mod usage;

#[cfg(test)]
mod tests;

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
    /// 排序键，白名单见 `usage_sort_expr`；`None` = 按请求时间排（方向随 [`Self::sort_desc`]）。
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
    /// 排序键，白名单见 `sessions_sort_expr`；`None` = 按最近活跃排。
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
