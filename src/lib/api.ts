/**
 * IPC 命令的薄封装：类型镜像 Rust 侧 serde 的 camelCase 输出，不做任何业务加工。
 * 字段的语义与"未知"口径见 crates/toktol-core/src/storage 的同名结构。
 */

import { invoke } from "@tauri-apps/api/core";
import { disable, enable, isEnabled } from "@tauri-apps/plugin-autostart";

export interface UsageTotals {
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly cacheReadTokens: number;
  readonly cacheWriteTokens: number;
  readonly reasoningTokens: number;
  readonly sessionCount: number;
  readonly recordCount: number;
  /** 已知成本合计；未知行不参与求和。 */
  readonly knownCostMicros: number;
  /** 总分未知的记录数，> 0 时须标注"部分未知"。 */
  readonly unknownCostRows: number;
}

export interface ModelUsageRow {
  readonly model: string;
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly cacheReadTokens: number;
  readonly cacheWriteTokens: number;
  readonly reasoningTokens: number;
  readonly knownCostMicros: number;
  readonly unknownCostRows: number;
}

export interface ScanReport {
  readonly filesScanned: number;
  readonly filesMissing: number;
  /** 超过单文件读取上限（256 MB）被跳过的文件数。 */
  readonly filesSkipped: number;
  readonly sessionsCreated: number;
  readonly recordsInserted: number;
  readonly parseErrors: number;
}

/** 首扫量级：流式适配器尚未扫入的字节总量（镜像 scan::ScanBacklog）。 */
export interface ScanBacklog {
  readonly files: number;
  readonly bytes: number;
}

/** 手动触发一轮扫描：常驻循环在 Rust 壳（与窗口生死无关），这里只是通知。 */
export function scanTrigger(): Promise<void> {
  return invoke("scan_trigger");
}

/** 首扫量级查询：禁用清单在壳内读取（与扫描循环同一事实源），无需传参。 */
export function fetchScanBacklog(): Promise<ScanBacklog> {
  return invoke("scan_backlog");
}

// ── 明细页 ──────────────────────────────────────────────────
// 类型镜像 storage 的 UsageRecordsQuery / UsageRecordRow（serde camelCase）。

/** 维度筛选；null / 空数组 = 该维度不筛选。projects 里的 "" 是"无项目"哨兵。 */
export interface UsageFilters {
  readonly timeStart: number | null;
  readonly timeEnd: number | null;
  readonly tools: readonly string[];
  readonly models: readonly string[];
  readonly projects: readonly string[];
  readonly search: string | null;
}

/** 明细分页查询的完整输入；排序键/分页由服务端执行，白名单外的键退回时间排序。 */
export interface UsageRecordsQuery {
  readonly filters: UsageFilters;
  readonly disabled: readonly string[];
  readonly sortKey: string | null;
  readonly sortDesc: boolean;
  readonly offset: number;
  readonly limit: number;
}

/** 明细页的一行；sessionId/projectDir 为 null = 会话已删除（用量保留，归属消失）。 */
export interface UsageRecordRow {
  readonly id: number;
  readonly ts: number;
  readonly tool: string;
  readonly model: string;
  readonly sessionExternalId: string | null;
  readonly projectDir: string | null;
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly cacheReadTokens: number;
  readonly cacheWriteTokens: number;
  readonly reasoningTokens: number | null;
  /** 请求耗时（毫秒）；日志没带计时则为 null。 */
  readonly durationMs: number | null;
  /** 视图口径：任一有量桶未定价则为 null。 */
  readonly costMicros: number | null;
  readonly inputCostMicros: number | null;
  readonly outputCostMicros: number | null;
  readonly cacheReadCostMicros: number | null;
  readonly cacheWriteCostMicros: number | null;
}

export interface UsageRecordsPage {
  readonly rows: readonly UsageRecordRow[];
  readonly total: number;
}

/** 筛选下拉的一个选项；value 为 null 只出现在项目维度 = 无项目。 */
export interface UsageFilterOption {
  readonly value: string | null;
  /** 分面口径：其余维度已按当前筛选限定后，该值的记录数。 */
  readonly count: number;
}

export interface UsageFilterOptionsPayload {
  readonly tools: readonly UsageFilterOption[];
  readonly models: readonly UsageFilterOption[];
  readonly projects: readonly UsageFilterOption[];
}

export function fetchUsageRecords(
  query: UsageRecordsQuery,
): Promise<UsageRecordsPage> {
  return invoke("usage_records_page", { query });
}

export function fetchUsageFilterOptions(
  filters: UsageFilters,
  disabled: string[],
): Promise<UsageFilterOptionsPayload> {
  return invoke("usage_filter_options", { filters, disabled });
}

// ── 总览仪表盘 ──────────────────────────────────────────────
// 类型镜像 storage 的 DashboardPayload 系列（serde camelCase）。
// 口径：tokens = 五桶合计（缓存读是子集）；成本 = 已知成本（未定价行不计）；
// 趋势桶起点 / 日历日期都是本地时区；label 由页面按 trendGrain 生成本地化文案。

/** 趋势序列的一桶；startMs 是本地时区对齐的桶起点（epoch ms）。 */
export interface DashboardTrendBucket {
  readonly startMs: number;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
  readonly cacheReadTokens: number;
  readonly cacheReadCostMicros: number;
}

export type TrendGrain = "hour" | "day" | "month";

/** 按模型拆分的趋势序列：三个数组与 trend 的桶一一对齐（无记录的桶填 0）。 */
export interface DashboardModelSeries {
  readonly model: string;
  readonly costMicros: readonly number[];
  readonly tokens: readonly number[];
  readonly calls: readonly number[];
}

/** 构成类卡片的一行（模型 / 项目共用）；label 里的 "" 是"无项目"哨兵。 */
export interface DashboardBreakdownRow {
  readonly label: string;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
  readonly unknownCost: boolean;
}

/** 桑基图的一条链路：工具 → 模型、模型 → 项目两段；目标为 "" 即无归属。 */
export interface DashboardFlowLink {
  readonly source: string;
  readonly target: string;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
}

/** 活跃热力图的一格：day 0=周一 … 6=周日 × 小时。 */
export interface DashboardActivityCell {
  readonly day: number;
  readonly hour: number;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
}

/** 日历热力图的一天；date 为本地日期 YYYY-MM-DD，只含有数据的日子（升序）。 */
export interface DashboardDailyPoint {
  readonly date: string;
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
}

/** 箱线图的一组五数概括：min / Q1 / 中位数 / Q3 / max；只统计有价行。 */
export interface DashboardSpread {
  readonly label: string;
  readonly costMicros: readonly number[];
  readonly tokens: readonly number[];
}

/** 会话消耗气泡：一会话（× 模型）一点；全部行未定价的会话不在切片里。 */
export interface DashboardSessionPoint {
  /** 会话所属工具；与 externalId 一起构成会话自然键。 */
  readonly tool: string;
  /** 会话在工具数据库里的原始 id（与会话页同一身份）。 */
  readonly externalId: string;
  /** 会话的项目目录；无项目为 ""。 */
  readonly projectDir: string;
  /** 模型标准名；一会话跨多模型时按模型拆点。 */
  readonly model: string;
  readonly calls: number;
  readonly tokens: number;
  /** 该会话（该模型）的已知成本合计（微美元）。 */
  readonly costMicros: number;
  /** 该会话首条记录的时间（epoch ms）。 */
  readonly startMs: number;
  /** 该会话末条记录的时间（epoch ms）。 */
  readonly endMs: number;
}

/** 费用构成瀑布的分项；分项之和 = 已知总成本。 */
export interface DashboardCostComposition {
  readonly inputCostMicros: number;
  readonly outputCostMicros: number;
  /** 推理量按输出价计费，从输出桶里拆出来单列。 */
  readonly reasoningCostMicros: number;
  readonly cacheReadCostMicros: number;
  readonly cacheWriteCostMicros: number;
}

export interface DashboardTotals {
  readonly costMicros: number;
  readonly tokens: number;
  readonly calls: number;
  readonly sessions: number;
  readonly projects: number;
  readonly cacheReadTokens: number;
  /** 活跃时长合计：各会话（首条 − 末条记录时间）之和；无会话的孤儿行不计。 */
  readonly activeDurationMs: number;
  readonly activeTools: number;
}

export interface DashboardPayload {
  readonly totals: DashboardTotals;
  /** 时间趋势，按桶起点升序；空数据为空数组。 */
  readonly trend: readonly DashboardTrendBucket[];
  /** 趋势粒度，前端据此生成本地化 label。 */
  readonly trendGrain: TrendGrain;
  readonly modelTrend: readonly DashboardModelSeries[];
  /** 按模型构成，已知成本降序。 */
  readonly composition: readonly DashboardBreakdownRow[];
  /** 按项目构成，已知成本降序；"" = 无项目。 */
  readonly projects: readonly DashboardBreakdownRow[];
  readonly flow: readonly DashboardFlowLink[];
  /** 恒 7×24 格（无数据的格补 0）。 */
  readonly activity: readonly DashboardActivityCell[];
  /** 有数据的日子，日期升序；不随时间筛选收窄，缺的日子由前端补 0。 */
  readonly daily: readonly DashboardDailyPoint[];
  /** 有价模型的五数概括，顺序与 composition 一致。 */
  readonly spread: readonly DashboardSpread[];
  /** 会话消耗气泡切片：一会话（× 模型）一点，按已知成本降序。 */
  readonly sessions: readonly DashboardSessionPoint[];
  readonly costComposition: DashboardCostComposition;
  /** 有未定价行的模型个数。 */
  readonly unknownModelCount: number;
}

/** 总览仪表盘：filters 与明细页同语义（"" = 无项目哨兵），disabled 的工具不参与聚合。 */
export function fetchOverview(
  filters: UsageFilters,
  disabled: string[],
): Promise<DashboardPayload> {
  return invoke("overview", { filters, disabled });
}

// ── 会话页 ──────────────────────────────────────────────────
// 类型镜像 storage 的 SessionFilters / SessionsPageQuery / SessionRow（serde camelCase）。

/** 会话页筛选；null / 空数组 = 该维度不筛选。 */
export interface SessionFilters {
  readonly timeStart: number | null;
  readonly timeEnd: number | null;
  readonly tools: readonly string[];
  readonly search: string | null;
}

export interface SessionsPageQuery {
  readonly filters: SessionFilters;
  readonly disabled: readonly string[];
  readonly sortKey: string | null;
  readonly sortDesc: boolean;
  readonly offset: number;
  readonly limit: number;
}

/** 会话页的一行：会话事实 + 该会话全部用量记录的聚合（成本不在会话维度展示）。 */
export interface SessionRow {
  readonly id: number;
  readonly tool: string;
  readonly externalId: string;
  readonly title: string | null;
  readonly projectDir: string | null;
  readonly lastActivityAt: number;
  readonly requestCount: number;
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly cacheReadTokens: number;
  readonly cacheWriteTokens: number;
}

export interface SessionsPagePayload {
  readonly rows: readonly SessionRow[];
  readonly total: number;
}

export function fetchSessionsPage(
  query: SessionsPageQuery,
): Promise<SessionsPagePayload> {
  return invoke("sessions_page", { query });
}

/** 批量删除的结果：入桶路径 + 未能删除的会话 id（镜像 sessions::BatchTrashReport）。 */
export interface BatchTrashReport {
  readonly trashed: readonly string[];
  readonly failed: readonly number[];
}

/** 单次删除的结果：移入回收站的路径（镜像 sessions::TrashReport）。 */
export interface TrashReport {
  readonly trashed: readonly string[];
}

export function deleteSessions(
  sessionIds: number[],
): Promise<BatchTrashReport> {
  return invoke("delete_sessions", { sessionIds });
}

/** 单会话删除：语义同批量（文件进回收站、统计保留）；不可删会话直接报错。 */
export function deleteSession(sessionId: number): Promise<TrashReport> {
  return invoke("delete_session", { sessionId });
}

// ── 会话转录 ────────────────────────────────────────────────
// 类型镜像 adapter/transcript 的 TranscriptEntry / TranscriptBlock
//（serde camelCase 与 tag = "kind"）。

export type TranscriptBlock =
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "thinking"; readonly text: string }
  | {
      /**
       * harness 注入上下文所在消息的完整原文（<system-reminder> 段、
       * <user_query> 标签与提问内容一字不差）。injectedChars 是其中注入段
       * 的字符数（占比条分子）。
       */
      readonly kind: "injected";
      readonly text: string;
      readonly injectedChars: number;
    }
  | {
      readonly kind: "toolCall";
      readonly id: string | null;
      readonly name: string | null;
      readonly arguments: string | null;
    }
  | {
      readonly kind: "toolResult";
      readonly callId: string | null;
      readonly content: string;
      readonly isError: boolean;
    }
  | {
      /** 图片附件引用：本地路径型只带元数据；opencode 的 dataUrl 内嵌原图。 */
      readonly kind: "image";
      readonly path: string;
      readonly filename: string;
      readonly size: number | null;
      readonly exists: boolean;
      readonly dataUrl: string | null;
    }
  | { readonly kind: "raw"; readonly json: string }
  | {
      /**
       * 后台任务完成通知（<task-notification> 包裹，user 角色、系统生成）。
       * text 是整条原文（含尾部固定模板指令），字段已从原文解析并反转义。
       */
      readonly kind: "taskNotification";
      readonly taskId: string;
      readonly status: string;
      readonly summary: string;
      readonly text: string;
    }
  | {
      /** 上下文压缩摘要（<conversation_history_summary> 标签内原文）。 */
      readonly kind: "historySummary";
      readonly text: string;
      /** 紧随的"继续对话"固定指令是否已并入本条（核心层合并）。 */
      readonly hasContinue: boolean;
    }
  | { readonly kind: "continueNotice" }
  | {
      /** 用户中断标记（codex 的 <turn_aborted>）：事件而非内容，渲染为红色分隔线徽章。 */
      readonly kind: "turnAborted";
    }
  | {
      /**
       * 工具注入的会话上下文（codex 的 <environment_context> /
       * <skills_instructions> / <turn_aborted> 等包裹消息）：给模型的指令，
       * 不是用户说的话。text 是整条原文（含包裹标签），默认折叠成一行。
       */
      readonly kind: "harnessContext";
      readonly tag: string;
      readonly text: string;
    };

export interface TranscriptEntry {
  readonly role: "user" | "assistant" | "system" | "tool";
  readonly tsMs: number | null;
  readonly model: string | null;
  readonly blocks: readonly TranscriptBlock[];
}

/** 一页里的条目：seq 是索引顺序号，翻页与跳轮的锚点。 */
export interface TranscriptPageEntry {
  readonly seq: number;
  readonly entry: TranscriptEntry;
}

/** 一页转录。`built = false` 表示索引未建（或失效），先触发建站。 */
export interface TranscriptPagePayload {
  readonly tool: string;
  readonly externalId: string;
  readonly built: boolean;
  readonly total: number;
  readonly entries: readonly TranscriptPageEntry[];
}

/** 轮次摘要的一段：正文文本或图片占位（tooltip 里图片段渲染成胶囊）。 */
export type TranscriptTurnPart =
  | { readonly kind: "text"; readonly text: string }
  | { readonly kind: "image"; readonly filename: string };

/** 目录（轮次列表）的一项。 */
export interface TranscriptTurn {
  readonly seq: number;
  readonly tsMs: number | null;
  readonly snippet: string;
  /** 结构化分段：文本/图片按原位交错；纯文本版即 snippet。 */
  readonly parts: readonly TranscriptTurnPart[];
  readonly isSummary: boolean;
}

export interface TranscriptTurnsPayload {
  readonly tool: string;
  readonly externalId: string;
  readonly built: boolean;
  readonly turns: readonly TranscriptTurn[];
}

/**
 * 分页取转录：`afterSeq`（向后翻）或 `beforeSeq`（向前翻）二选一，都缺省
 * 则从头取。索引未建时返回 built=false 的空页。
 */
export function fetchTranscriptPage(
  tool: string,
  externalId: string,
  afterSeq: number | null,
  beforeSeq: number | null,
  limit: number,
): Promise<TranscriptPagePayload> {
  return invoke("session_transcript_page", {
    tool,
    externalId,
    afterSeq,
    beforeSeq,
    limit,
  });
}

/** 目录（轮次列表）。索引未建时 built=false，前端先触发建站。 */
export function fetchTranscriptTurns(
  tool: string,
  externalId: string,
): Promise<TranscriptTurnsPayload> {
  return invoke("session_transcript_turns", { tool, externalId });
}

/** 构建转录索引：低优先级线程，进度走 `transcript://progress` 事件。 */
export function buildTranscriptIndex(
  tool: string,
  externalId: string,
): Promise<boolean> {
  return invoke("session_transcript_build", { tool, externalId });
}

/** 取消正在进行的建站（已建部分落库，续建从断点继续）。 */
export function cancelTranscriptBuild(): Promise<void> {
  return invoke("session_transcript_cancel");
}

// ── 网关 ────────────────────────────────────────────────────
// 类型镜像 toktol-gateway/src/proxy 的 GatewayStatus / ConfigView（serde camelCase）。

export interface UpstreamView {
  readonly name: string;
  readonly protocol: "openai" | "responses" | "anthropic" | "gemini";
  readonly baseUrl: string;
  readonly enabled: boolean;
}

export interface MappingView {
  readonly from: string;
  readonly to: string;
}

/** UI 提交的上游字段（serde camelCase 对齐 Rust 的 UpstreamInput）。 */
export interface UpstreamInput {
  readonly name: string;
  readonly protocol: "openai" | "responses" | "anthropic" | "gemini";
  readonly baseUrl: string;
  readonly keyRef: string;
  readonly enabled: boolean;
}

export interface RouteView {
  readonly pattern: string;
  readonly upstream: string;
}

/** 配置视图：valid 快照或 invalid 错误；`kind` 是 serde 的 tag。 */
export type ConfigView =
  | {
      readonly kind: "valid";
      readonly listen: string;
      readonly upstreams: readonly UpstreamView[];
      readonly routes: readonly RouteView[];
      readonly mappings: readonly MappingView[];
      readonly models: readonly string[];
      readonly tokenCount: number;
    }
  | { readonly kind: "invalid"; readonly message: string };

export interface GatewayStatus {
  readonly running: boolean;
  readonly listen: string | null;
  readonly config: ConfigView;
  readonly configPath: string | null;
}

export interface GatewayTotals {
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly cacheReadTokens: number;
  readonly cacheWriteTokens: number;
  readonly reasoningTokens: number;
  readonly requestCount: number;
  readonly errorCount: number;
  readonly knownCostMicros: number;
  readonly unknownCostRows: number;
}

export interface GatewayOverviewPayload {
  readonly totals: GatewayTotals;
  readonly byModel: readonly ModelUsageRow[];
}

/** 流量页的一行（镜像 storage 的 GatewayRequestRow）。 */
export interface GatewayRequestRow {
  readonly id: number;
  readonly ts: number;
  readonly modelRaw: string;
  readonly model: string;
  readonly inputTokens: number;
  readonly outputTokens: number;
  readonly statusCode: number | null;
  readonly latencyMs: number | null;
  readonly upstream: string | null;
  /** 视图口径：任一有量桶未定价则为 null。 */
  readonly costMicros: number | null;
}

export interface GatewayRequestsPage {
  readonly rows: readonly GatewayRequestRow[];
  readonly total: number;
}

export function fetchGatewayStatus(): Promise<GatewayStatus> {
  return invoke("gateway_status");
}

export function startGateway(): Promise<GatewayStatus> {
  return invoke("gateway_start");
}

export function stopGateway(): Promise<GatewayStatus> {
  return invoke("gateway_stop");
}

/** 明文令牌只在本次返回值里出现；哈希由 Rust 侧写入 gateway.json。 */
export function issueGatewayToken(): Promise<string> {
  return invoke("gateway_issue_token");
}

export function fetchGatewayOverview(): Promise<GatewayOverviewPayload> {
  return invoke("gateway_overview");
}

export function fetchGatewayRequests(
  page: number,
  pageSize: number,
): Promise<GatewayRequestsPage> {
  return invoke("gateway_requests", { page, pageSize });
}

export function addUpstream(upstream: UpstreamInput): Promise<void> {
  return invoke("gateway_upstream_add", { upstream });
}

export function updateUpstream(name: string, upstream: UpstreamInput): Promise<void> {
  return invoke("gateway_upstream_update", { name, upstream });
}

export function deleteUpstream(name: string): Promise<void> {
  return invoke("gateway_upstream_delete", { name });
}

export function addMapping(from: string, to: string): Promise<void> {
  return invoke("gateway_mapping_add", { from, to });
}

export function deleteMapping(from: string): Promise<void> {
  return invoke("gateway_mapping_delete", { from });
}

// ── 定价页 ──────────────────────────────────────────────────
// 类型镜像 storage 的 PricingOverviewPayload / CatalogSyncPayload（serde camelCase）。
// 单价一律微美元 / 每百万 token；页面按 USD 显示。

/** 目录建议价（收敛到一条后）。 */
export interface PricingCatalogHint {
  readonly provider: string;
  readonly modelId: string;
  readonly displayName: string | null;
  readonly status: string | null;
  readonly inputPerMtokMicros: number | null;
  readonly outputPerMtokMicros: number | null;
  readonly cacheReadPerMtokMicros: number | null;
  readonly cacheWritePerMtokMicros: number | null;
}

/** 指向某标准模型的一个原始变种名。 */
export interface PricingVariant {
  readonly rawModel: string;
  /** 映射来源：auto（确定性归一）/ suggested（启发合并，需过目）/ user。 */
  readonly source: string;
}

/** 定价页一行的标准模型。 */
export interface PricingModelRow {
  readonly id: string;
  readonly displayName: string;
  readonly priceSource: string | null;
  readonly inputPerMtokMicros: number | null;
  readonly outputPerMtokMicros: number | null;
  readonly cacheReadPerMtokMicros: number | null;
  readonly cacheWritePerMtokMicros: number | null;
  readonly catalog: PricingCatalogHint | null;
  readonly usageCount: number;
  readonly variants: readonly PricingVariant[];
  /** 变种映射来源的"最弱一环"：suggested > auto > user；无变种为 null。
   *  非 user 即待确认（匹配成功的默认归并，待用户过目）。 */
  readonly mappingSource: string | null;
}

/** models.dev 目录候选的一行（每模型收敛一条）：标准名改名下拉的数据源。 */
export interface CatalogModelBrief {
  readonly modelId: string;
  readonly provider: string;
  readonly displayName: string | null;
  readonly status: string | null;
  readonly inputPerMtokMicros: number | null;
  readonly outputPerMtokMicros: number | null;
  readonly cacheReadPerMtokMicros: number | null;
  readonly cacheWritePerMtokMicros: number | null;
}

export interface PricingOverviewPayload {
  readonly syncedAt: number | null;
  readonly models: readonly PricingModelRow[];
  readonly catalogModels: readonly CatalogModelBrief[];
}

export interface CatalogSyncPayload {
  readonly entries: number;
  readonly matched: number;
  readonly priced: number;
  /** 反向补映射改指的变种个数。 */
  readonly remapped: number;
  readonly syncedAt: number;
}

/** 四桶价格入参；null = 该桶无价（不计费）。 */
export interface ModelPriceInput {
  readonly input: number | null;
  readonly output: number | null;
  readonly cacheRead: number | null;
  readonly cacheWrite: number | null;
}

export function fetchPricingOverview(): Promise<PricingOverviewPayload> {
  return invoke("pricing_overview");
}

export function syncModelCatalog(): Promise<CatalogSyncPayload> {
  return invoke("sync_model_catalog");
}

export function setModelPrice(modelId: string, price: ModelPriceInput): Promise<void> {
  return invoke("set_model_price", { modelId, ...price });
}

export function setModelMapping(rawModel: string, modelId: string): Promise<number> {
  return invoke("set_model_mapping", { rawModel, modelId });
}

/** 批量收编变体：raw 列表改指到一个标准模型，同事务清孤儿壳并重算。 */
export function bindVariants(raws: string[], modelId: string): Promise<number> {
  return invoke("bind_variants", { raws, modelId });
}

/** 用户确认一个标准模型的自动映射：该模型下所有 auto/suggested 变种翻成 user。 */
export function confirmModel(modelId: string): Promise<number> {
  return invoke("confirm_model", { modelId });
}

export function mergeModels(from: string, into: string): Promise<number> {
  return invoke("merge_models", { from, into });
}

/** 标准模型改成 models.dev 规范名：合并收编 + 本地目录快照补缺价（无网络）。 */
export function renameModel(from: string, into: string): Promise<number> {
  return invoke("rename_model", { from, into });
}

// ── 配置页 ──────────────────────────────────────────────────
// 类型镜像 toktol-core/src/toolconfig 的 ToolConfigReport / FileContent（serde camelCase）。

/** 一台 MCP 服务器的连接元数据；env 只回键名，URL/命令行的疑似密钥值已打码为 ***。 */
export interface McpServerView {
  readonly name: string;
  readonly transport: string;
  readonly scope: string;
  readonly command: string | null;
  readonly url: string | null;
  readonly envKeys: readonly string[];
  readonly project: string | null;
}

/** 一个 Skill 条目；root/rel 用于抽屉详情回读 SKILL.md（经脱敏闸门）。 */
export interface SkillView {
  readonly name: string;
  readonly scope: string;
  readonly description: string | null;
  readonly path: string;
  readonly root: string;
  readonly rel: string;
}

/** 文件树的一个条目；rel 相对根，回读内容时原样传回。 */
export interface FileEntry {
  readonly rel: string;
  readonly name: string;
  readonly dir: boolean;
  readonly size: number | null;
}

/** 文件树的一个根：目录根带子树条目；单文件根条目为空。 */
export interface ConfigRoot {
  readonly key: string;
  readonly label: string;
  readonly entries: readonly FileEntry[];
}

/** 一个工具的配置视图。supported=false 表示主目录不存在（工具未安装）。 */
export interface ToolConfigReport {
  readonly tool: string;
  readonly homeLabel: string;
  readonly supported: boolean;
  readonly mcp: readonly McpServerView[];
  readonly skills: readonly SkillView[];
  readonly roots: readonly ConfigRoot[];
}

/** 文件内容回读结果；文本已脱敏，truncated 标记超限截断。 */
export interface FileContent {
  readonly text: string;
  readonly truncated: boolean;
}

/** 工具配置视图：现读磁盘，不落库。 */
export function fetchToolConfig(tool: string): Promise<ToolConfigReport> {
  return invoke("tool_config", { tool });
}

/** 配置页内容回读：凭据类文件拒读、统一脱敏、超限截断。 */
export function fetchToolConfigEntry(
  tool: string,
  root: string,
  rel: string,
): Promise<FileContent> {
  return invoke("tool_config_entry", { tool, root, rel });
}

// ---- 后台与托盘 ----

/** 是否运行在 Tauri 窗口内：纯浏览器打开 dev 页面时壳能力（托盘/自启动）不可用。 */
export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** 壳配置（~/.toktol/config.json）的当前态：托盘开关初值在这里拉一次。 */
export interface ShellConfig {
  readonly tray: boolean;
  readonly lowPower: boolean;
  readonly disabledTools: readonly string[];
}

export function shellGetConfig(): Promise<ShellConfig> {
  return invoke("shell_get_config");
}

/** 被禁用工具清单镜像进壳：扫描循环每轮从壳配置读取（窗口销毁后前端传不了参）。 */
export function shellSetDisabledTools(disabled: string[]): Promise<void> {
  return invoke("shell_set_disabled_tools", { disabled });
}

export function traySetEnabled(tray: boolean): Promise<void> {
  return invoke("tray_set_enabled", { tray });
}

/** 托盘菜单文案跟随界面语言；前端在启动与语言切换时同步。 */
export function traySetTexts(
  show: string,
  scan: string,
  lowPower: string,
  quit: string,
): Promise<void> {
  return invoke("tray_set_texts", { show, scan, lowPower, quit });
}

// ---- 开机自启动（tauri-plugin-autostart） ----

export function autostartIsEnabled(): Promise<boolean> {
  return isEnabled();
}

export function autostartSet(enabled: boolean): Promise<void> {
  return enabled ? enable() : disable();
}
