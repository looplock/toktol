//! 前端可调用的 Tauri 命令。
//!
//! 纪律：命令函数只做"参数转发 + 结果序列化"，
//! 真正的实现在 `toktol-core` / `toktol-gateway`。后续阶段每加一个命令，都在这里加一行转调。
//!
//! 错误通道：领域错误不跨 IPC 传结构体，只传稳定错误码字符串（[`ErrorCode::as_str`]），
//! 前端按码选文案。

use serde::Serialize;
use tauri::{Emitter, Manager, State};
use toktol_core::error::ErrorCode;
use toktol_core::{VERSION, paths, pricing, scan, sessions, storage};
use toktol_gateway::proxy::{self, GatewayHandle, GatewayStatus};

/// 网关运行句柄的壳内槽位。async Mutex：start 是 async 且持锁跨 await，
/// 顺带串行化起停，避免并发双开。
type GatewaySlot = tauri::async_runtime::Mutex<Option<GatewayHandle>>;

/// 领域层错误压成错误码字符串。
fn code_of(err: toktol_core::error::Error) -> String {
    err.code().as_str().to_string()
}

/// 组装状态载荷：运行位来自槽位，配置视图每次现读磁盘（页面要反映"刚保存"的文件）。
fn current_status(
    config_path: Option<std::path::PathBuf>,
    handle: Option<&GatewayHandle>,
) -> GatewayStatus {
    proxy::status(
        config_path.as_deref(),
        handle.is_some_and(GatewayHandle::is_running),
        handle.map(GatewayHandle::listen),
    )
}

/// 每个命令独立开库：壳进程里没有长连接的必要，迁移已跑过时打开是毫秒级。
fn open_storage() -> Result<storage::Storage, String> {
    let db =
        paths::main_db_path().ok_or_else(|| ErrorCode::HomeDirUnavailable.as_str().to_string())?;
    storage::open(&db).map_err(code_of)
}

/// 返回应用版本（取自 `toktol-core` 的单一事实源）；骨架期兼作 IPC 链路冒烟。
#[tauri::command]
pub fn app_version() -> &'static str {
    VERSION
}

/// 手动触发一轮扫描：常驻循环在壳里（与窗口生死无关），这里只是通知。
/// 在飞时壳侧记为"下一轮立即跑"，绝不并发扫描。
#[tauri::command]
pub fn scan_trigger(app: tauri::AppHandle) -> Result<(), String> {
    crate::trigger_scan(&app)
}

/// 首扫量级：流式适配器尚未扫入的字节总量。禁用清单读壳配置——与扫描循环
/// 同一事实源，禁用的工具不扫，也就不提示积压；多 GB 首扫期间前端用它
/// 解释"为什么这轮久"。
#[tauri::command]
pub async fn scan_backlog(app: tauri::AppHandle) -> Result<scan::ScanBacklog, String> {
    let disabled = app
        .state::<crate::shell_config::ShellConfigState>()
        .get()
        .disabled_tools;
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        toktol_core::scan::scan_backlog(&storage, &disabled).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 总览仪表盘聚合：一次给出仪表盘全部卡片的切片。`filters` 与明细页同语义
/// （projects 里的 "" 是"无项目"哨兵）；`disabled` 是被禁用工具的 id 列表
/// （前端偏好），只统计启用的工具；禁用不删数据。
#[tauri::command]
pub async fn overview(
    filters: Option<storage::UsageFilters>,
    disabled: Option<Vec<String>>,
) -> Result<storage::DashboardPayload, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage
            .dashboard(&filters.unwrap_or_default(), &disabled.unwrap_or_default())
            .map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 工具配置视图（配置页）：MCP 连接元数据（已脱敏）、Skills 清单与文件树，
/// 现读磁盘。未知工具返回 core.unsupported。
#[tauri::command]
pub async fn tool_config(
    tool: String,
) -> Result<toktol_core::toolconfig::ToolConfigReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        toktol_core::toolconfig::inspect(&tool).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 配置页内容回读：凭据类文件拒读、文本统一脱敏、超限截断。
#[tauri::command]
pub async fn tool_config_entry(
    tool: String,
    root: String,
    rel: String,
) -> Result<toktol_core::toolconfig::FileContent, String> {
    tauri::async_runtime::spawn_blocking(move || {
        toktol_core::toolconfig::read_entry(&tool, &root, &rel).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 删除会话：数据进系统回收站，统计保留（usage 脱离归属）。文件类来源删
/// 日志文件；数据库类来源（opencode、zcode 被轮转会话）先导出会话行入桶再
/// 删行；未实现删除的来源（codebuddy）返回 core.unsupported。
/// 参数是会话列表返回的行 id。
#[tauri::command]
pub async fn delete_session(session_id: i64) -> Result<sessions::TrashReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        sessions::trash_session(&storage, session_id).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 批量删除会话：逐个复用单删语义，单个失败（不存在、数据库来源未支持删除）
/// 不阻断整批，聚进 failed 供前端提示。
#[tauri::command]
pub async fn delete_sessions(session_ids: Vec<i64>) -> Result<sessions::BatchTrashReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        sessions::trash_sessions(&storage, &session_ids).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 会话转录：按 (tool, externalId) 定位会话，现读源日志的消息内容（用户/
/// 助手消息、工具调用等）。内容只在本次返回值里出现，不落库；
/// zcode/grok 等日志不带可重建对话的工具返回 core.unsupported。
#[tauri::command]
pub async fn session_transcript_page(
    tool: String,
    external_id: String,
    after_seq: Option<i64>,
    before_seq: Option<i64>,
    limit: i64,
) -> Result<sessions::TranscriptPagePayload, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        sessions::session_transcript_page(
            &storage,
            &tool,
            &external_id,
            after_seq,
            before_seq,
            limit,
        )
        .map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 目录（轮次列表）。索引未建时 `built = false`，前端先触发建站。
#[tauri::command]
pub async fn session_transcript_turns(
    tool: String,
    external_id: String,
) -> Result<sessions::TranscriptTurnsPayload, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        sessions::session_transcript_turns(&storage, &tool, &external_id).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// `transcript://progress` 事件的载荷：建站进度（done/total 为索引条目数）。
/// 字段名与 api.ts 的镜像由 verify:api 盯着。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptProgressPayload {
    pub tool: String,
    pub external_id: String,
    pub done: u64,
    pub total: u64,
}

/// 为会话源文件构建转录索引：专用低优先级线程（Windows 下 THREAD_MODE_
/// BACKGROUND，机器忙时自动让路），进度经 `transcript://progress` 事件
/// 广播；返回是否建完整（false = 被取消，断点已落库）。并发建站各持一份
/// 取消旗标（注册表见 [`crate::TranscriptBuildSlot`]），互不覆盖。
#[tauri::command]
pub async fn session_transcript_build(
    app: tauri::AppHandle,
    tool: String,
    external_id: String,
) -> Result<bool, String> {
    use std::sync::Arc;

    let cancel = app.state::<crate::TranscriptBuildSlot>().register();
    // 闭包 move 进的是这份克隆，注册表句柄留在本地做 unregister。
    let build_cancel = Arc::clone(&cancel);
    let build_tool = tool.clone();
    let build_external = external_id.clone();
    let progress_app = app.clone();

    let joined = tauri::async_runtime::spawn_blocking(move || {
        let handle = std::thread::Builder::new()
            .name("transcript-index".into())
            .spawn(move || {
                toktol_core::set_thread_background_priority();
                let progress = move |done: u64, total: u64| {
                    let _ = progress_app.emit(
                        "transcript://progress",
                        TranscriptProgressPayload {
                            tool: build_tool.clone(),
                            external_id: build_external.clone(),
                            done,
                            total,
                        },
                    );
                };
                let cancel = Arc::clone(&build_cancel);
                let storage = open_storage()?;
                sessions::build_transcript_index(
                    &storage,
                    &tool,
                    &external_id,
                    sessions::TRANSCRIPT_CHECKPOINT_BYTES,
                    &cancel,
                    &progress,
                )
                .map_err(code_of)
            })
            .expect("转录索引线程启动失败");
        handle
            .join()
            .map_err(|_| "transcript index thread panicked".to_string())?
    })
    .await;

    // 摘旗标在结果传播之前：spawn_blocking 自身失败也不能把旗标留在注册表里。
    app.state::<crate::TranscriptBuildSlot>()
        .unregister(&cancel);
    joined.map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 取消正在进行的建站（可能不止一个在飞）：全体置位，已建部分已落库，
/// 下次从断点继续。
#[tauri::command]
pub fn session_transcript_cancel(app: tauri::AppHandle) -> Result<(), String> {
    app.state::<crate::TranscriptBuildSlot>().cancel_all();
    Ok(())
}

/// 网关状态：运行位 + 监听地址 + 配置视图（含校验错误），前端轮询用。
#[tauri::command]
pub async fn gateway_status(state: State<'_, GatewaySlot>) -> Result<GatewayStatus, String> {
    let slot = state.lock().await;
    Ok(current_status(paths::gateway_config_path(), slot.as_ref()))
}

/// 启动网关。已在跑则原样返回状态；配置非法或端口占用则报错不落地。
#[tauri::command]
pub async fn gateway_start(state: State<'_, GatewaySlot>) -> Result<GatewayStatus, String> {
    let mut slot = state.lock().await;
    if let Some(handle) = slot.as_ref()
        && handle.is_running()
    {
        return Ok(current_status(paths::gateway_config_path(), Some(handle)));
    }
    let config_path = paths::gateway_config_path()
        .ok_or_else(|| ErrorCode::HomeDirUnavailable.as_str().to_string())?;
    let db_path =
        paths::main_db_path().ok_or_else(|| ErrorCode::HomeDirUnavailable.as_str().to_string())?;
    let handle = GatewayHandle::start(config_path, db_path)
        .await
        // 启动失败把详情带给前端（端口占用/配置错误的具体原因只有这里知道）；
        // 稳定码仍在前缀，便于将来按码选文案。
        .map_err(|err| format!("{}: {err}", err.code().as_str()))?;
    let status = current_status(paths::gateway_config_path(), Some(&handle));
    *slot = Some(handle);
    Ok(status)
}

/// 停止网关（graceful：流式中的请求等完）。未运行则原样返回状态。
#[tauri::command]
pub async fn gateway_stop(state: State<'_, GatewaySlot>) -> Result<GatewayStatus, String> {
    let mut slot = state.lock().await;
    if let Some(mut handle) = slot.take() {
        handle.stop();
    }
    Ok(current_status(paths::gateway_config_path(), None))
}

/// 签发新访问令牌。明文只在本次返回值里出现一次；落盘的是 sha256 哈希。
#[tauri::command]
pub async fn gateway_issue_token() -> Result<String, String> {
    let config_path = paths::gateway_config_path()
        .ok_or_else(|| ErrorCode::HomeDirUnavailable.as_str().to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let token = toktol_gateway::config::generate_token();
        toktol_gateway::config::append_token_hash(&config_path, &token).map_err(code_of)?;
        Ok(token)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 网关侧总览聚合（与 overview 同构但只看 gateway_requests，双计未合并）。
#[tauri::command]
pub async fn gateway_overview() -> Result<storage::GatewayOverviewPayload, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let storage = open_storage()?;
        storage.gateway_overview().map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// IPC 分页命令的页大小统一上限：明细 / 会话 / 网关流量同口径。前端可选的
/// 页大小都不会超过它，这里钳的是防误传把整表拉回。
const PAGE_SIZE_MAX: i64 = 200;

/// 明细页请求级分页：筛选/排序/分页都在 SQL 里做（行数无上界，不能整表拉回前端）。
#[tauri::command]
pub async fn usage_records_page(
    query: storage::UsageRecordsQuery,
) -> Result<storage::UsageRecordsPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        let mut query = query;
        // 页大小统一钳到 PAGE_SIZE_MAX；页码换算成偏移也在这层做。
        query.limit = query.limit.clamp(1, PAGE_SIZE_MAX);
        query.offset = query.offset.max(0);
        storage.usage_records_page(&query).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 明细页筛选下拉的选项与计数（分面口径：各维度用其余筛选条件计数，排除禁用工具）。
#[tauri::command]
pub async fn usage_filter_options(
    filters: Option<storage::UsageFilters>,
    disabled: Option<Vec<String>>,
) -> Result<storage::UsageFilterOptionsPayload, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage
            .usage_filter_options(&filters.unwrap_or_default(), &disabled.unwrap_or_default())
            .map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 会话页分页：按会话聚合的用量（服务端筛选/排序/分页），页大小上限与明细页同口径。
#[tauri::command]
pub async fn sessions_page(
    query: storage::SessionsPageQuery,
) -> Result<storage::SessionsPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        let mut query = query;
        query.limit = query.limit.clamp(1, PAGE_SIZE_MAX);
        query.offset = query.offset.max(0);
        storage.sessions_page(&query).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 流量明细分页。页码从 1 起；页大小统一钳到 PAGE_SIZE_MAX 防误传。
#[tauri::command]
pub async fn gateway_requests(
    page: i64,
    page_size: i64,
) -> Result<storage::GatewayRequestsPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        let page_size = page_size.clamp(1, PAGE_SIZE_MAX);
        let page = page.max(1);
        storage
            .gateway_requests_page((page - 1) * page_size, page_size)
            .map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 配置文件写锁：上游/映射的增删改串行化，避免并发写坏 gateway.json。
type ConfigWriteLock = tauri::async_runtime::Mutex<()>;

fn gateway_config_path_or_err() -> Result<std::path::PathBuf, String> {
    paths::gateway_config_path().ok_or_else(|| ErrorCode::HomeDirUnavailable.as_str().to_string())
}

/// 新增上游（写 gateway.json，写前整份校验）。
#[tauri::command]
pub async fn gateway_upstream_add(
    upstream: toktol_gateway::config::UpstreamInput,
    lock: State<'_, ConfigWriteLock>,
) -> Result<(), String> {
    let path = gateway_config_path_or_err()?;
    let _guard = lock.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        toktol_gateway::config::upstream_add(&path, &upstream).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 更新上游（按名字定位，允许改名；引用同步改写）。
#[tauri::command]
pub async fn gateway_upstream_update(
    name: String,
    upstream: toktol_gateway::config::UpstreamInput,
    lock: State<'_, ConfigWriteLock>,
) -> Result<(), String> {
    let path = gateway_config_path_or_err()?;
    let _guard = lock.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        toktol_gateway::config::upstream_update(&path, &name, &upstream).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 删除上游（引用它的路由一并删除）。
#[tauri::command]
pub async fn gateway_upstream_delete(
    name: String,
    lock: State<'_, ConfigWriteLock>,
) -> Result<(), String> {
    let path = gateway_config_path_or_err()?;
    let _guard = lock.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        toktol_gateway::config::upstream_delete(&path, &name).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 新增/覆盖一条模型映射。
#[tauri::command]
pub async fn gateway_mapping_add(
    from: String,
    to: String,
    lock: State<'_, ConfigWriteLock>,
) -> Result<(), String> {
    let path = gateway_config_path_or_err()?;
    let _guard = lock.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        toktol_gateway::config::mapping_add(&path, &from, &to).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 删除一条模型映射。
#[tauri::command]
pub async fn gateway_mapping_delete(
    from: String,
    lock: State<'_, ConfigWriteLock>,
) -> Result<(), String> {
    let path = gateway_config_path_or_err()?;
    let _guard = lock.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        toktol_gateway::config::mapping_delete(&path, &from).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

// ── 定价页：模型目录同步、价格确认与映射修正 ─────────────────────

/// 拉取 models.dev 公共模型目录并整体落库。拉取、解析与挂价重算全在
/// [`toktol_core::pricing::catalog::sync_catalog`]（blocking HTTP，故整条放进
/// spawn_blocking）；断网/非 2xx 返回 core.catalog_fetch。
#[tauri::command]
pub async fn sync_model_catalog() -> Result<storage::CatalogSyncPayload, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        pricing::catalog::sync_catalog(&storage).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 定价页读路径：标准模型 × 现价 × 目录建议价 × 变种映射 × 用量。
#[tauri::command]
pub async fn pricing_overview() -> Result<storage::PricingOverviewPayload, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage.pricing_overview().map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 确认/修改一个标准模型的四桶价（source='user'），同一事务内自动重算。
/// 单价为微美元 / 每百万 token；`None` = 该桶无价（不计费）。
#[tauri::command]
pub async fn set_model_price(
    model_id: String,
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage
            .set_model_price(&model_id, input, output, cache_read, cache_write)
            .map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 修正一条变种映射（全局：raw_model → 标准模型），同一事务内改写事实表并重算。
#[tauri::command]
pub async fn set_model_mapping(raw_model: String, model_id: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage
            .set_model_mapping(&raw_model, &model_id, now_ms())
            .map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 批量收编变种映射（raw_model 列表 → 一个标准模型），同一事务内改写事实表、
/// 清孤儿壳并重算。定价页"指到这里的变体"的提交入口。
#[tauri::command]
pub async fn bind_variants(raws: Vec<String>, model_id: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage.bind_variants(&raws, &model_id).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 把标准模型 from 并入 into（映射改指、事实改写、价格搬家、壳删除）并重算。
#[tauri::command]
pub async fn merge_models(from: String, into: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage.merge_models(&from, &into).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 标准模型改名（定价页把本地名改成 models.dev 规范名）：合并语义收编 +
/// 本地目录快照补缺价，改名即关联挂价（无网络）。
#[tauri::command]
pub async fn rename_model(from: String, into: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage.rename_model(&from, &into).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 用户确认一个标准模型的自动映射：该模型下所有 auto/suggested 变种翻成 user。
#[tauri::command]
pub async fn confirm_model(model_id: String) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let storage = open_storage()?;
        storage.confirm_model(&model_id, now_ms()).map_err(code_of)
    })
    .await
    .map_err(|_| ErrorCode::Internal.as_str().to_string())?
}

/// 当前时间（epoch ms）：IPC 层只给 updated_at 打戳用。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

// ---- 托盘（"后台与托盘"设置区）。实现都在 lib.rs 的 tray_ops，这里只转发。 ----

/// 壳配置当前态：前端启动时拉一次（托盘开关初值），低功耗等由事件/重拉同步。
#[tauri::command]
pub fn shell_get_config(
    state: State<'_, crate::shell_config::ShellConfigState>,
) -> crate::shell_config::ShellConfig {
    state.get()
}

/// 被禁用工具清单镜像进壳配置：扫描循环每轮从壳配置读取，窗口销毁后前端
/// 传不了参，清单必须由壳自己持有。
#[tauri::command]
pub fn shell_set_disabled_tools(
    state: State<'_, crate::shell_config::ShellConfigState>,
    disabled: Vec<String>,
) {
    state.update(|c| c.disabled_tools = disabled);
}

/// 托盘显隐切换（设置页开关）。失败传错误串，前端据此回滚开关。
#[tauri::command]
pub fn tray_set_enabled(app: tauri::AppHandle, tray: bool) -> Result<(), String> {
    crate::tray_ops::set_enabled(&app, tray)?;
    // 只有切成功才落盘，偏好与实际状态保持一致。
    app.state::<crate::shell_config::ShellConfigState>()
        .update(|c| c.tray = tray);
    Ok(())
}

/// 菜单文案跟随界面语言；前端在启动与语言切换时调用。
#[tauri::command]
pub fn tray_set_texts(
    app: tauri::AppHandle,
    show: String,
    scan: String,
    low_power: String,
    quit: String,
) {
    crate::tray_ops::set_texts(&app, &show, &scan, &low_power, &quit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_version_returns_core_version() {
        assert_eq!(app_version(), toktol_core::VERSION);
        assert_eq!(app_version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn errors_compress_to_their_stable_code() {
        assert_eq!(
            code_of(toktol_core::error::Error::HomeDirUnavailable),
            "core.home_dir_unavailable"
        );
    }
}
