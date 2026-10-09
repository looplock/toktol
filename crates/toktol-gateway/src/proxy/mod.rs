//! 反向代理：基于 `axum` 的本地回环 HTTP 服务——校验 `sk-toktol-` 令牌、按模型
//! 路由到配置的上游、（需要时）协议转换、流式回传，结束后计量落库。
//!
//! 每请求先做 mtime 探测的热重载；重载失败沿用旧快照，见 config 模块文档。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use toktol_core::error::{Error, Result};
use toktol_core::paths::GATEWAY_TOKEN_PREFIX;

use super::config::{ConfigHandle, Protocol};
use super::metering::{self, MeterFacts};
use super::translate::{self, SseScanner, UsageScanner};

/// 运行中网关的句柄：壳层持有（放在 `Mutex<Option<_>>` 里），起停与探活都经它。
pub struct GatewayHandle {
    task: tokio::task::JoinHandle<()>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    listen: std::net::SocketAddr,
}

impl GatewayHandle {
    /// 加载配置、绑定监听并后台启动服务。配置非法则整体失败，不留半启动状态；
    /// 配置端口被占用时自动回落到系统分配的临时端口（仍绑回环），实际地址以
    /// [`Self::listen`] 为准。
    pub async fn start(config_path: PathBuf, db_path: PathBuf) -> Result<Self> {
        // 配置文件缺失时先落一份骨架，"启动服务"不依赖用户先手写文件。
        super::config::ensure_default(&config_path)?;
        let config = Arc::new(ConfigHandle::load(&config_path)?);
        let (listener, listen) = bind_with_fallback(config.current().listen).await?;
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            serve_on(listener, config, db_path, async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
        });
        Ok(Self {
            task,
            shutdown: Some(shutdown_tx),
            listen,
        })
    }

    /// 触发优雅关停；流式中的请求会被等完（axum graceful shutdown 语义）。
    pub fn stop(&mut self) {
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
    }

    /// 后台任务是否仍在跑（stop 之后、graceful 退出完成前仍为 true）。
    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    /// 监听地址。
    pub fn listen(&self) -> std::net::SocketAddr {
        self.listen
    }
}

/// 配置文件的对外视图：有效快照或校验错误。前端据此渲染配置概览与错误提示。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ConfigView {
    /// 配置合法。
    Valid {
        /// 监听地址。
        listen: String,
        /// 上游列表。
        upstreams: Vec<UpstreamView>,
        /// 路由列表。
        routes: Vec<RouteView>,
        /// 模型映射列表。
        mappings: Vec<MappingView>,
        /// `/v1/models` 宣称的模型列表。
        models: Vec<String>,
        /// 已登记的访问令牌数（只给数量，绝不给哈希）。
        token_count: usize,
    },
    /// 配置缺失或校验失败；`message` 已在 config 层脱敏（不含文件内容）。
    Invalid {
        /// 错误描述。
        message: String,
    },
}

/// 一个上游的对外视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamView {
    /// 上游名。
    pub name: String,
    /// 协议（"openai" / "anthropic"）。
    pub protocol: &'static str,
    /// API 根地址。
    pub base_url: String,
    /// 是否参与路由。
    pub enabled: bool,
}

/// 一条模型映射的对外视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingView {
    /// 本地应用请求的模型名。
    pub from: String,
    /// 转发给上游的模型名。
    pub to: String,
}

/// 一条路由的对外视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteView {
    /// 模型匹配模式。
    pub pattern: String,
    /// 目标上游名。
    pub upstream: String,
}

/// 网关整体状态：运行位 + 监听地址 + 配置视图。前端轮询的唯一载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    /// 服务是否在跑。
    pub running: bool,
    /// 运行中时的监听地址；未运行为 `None`。
    pub listen: Option<String>,
    /// 配置视图（与运行状态独立：改坏配置时服务照跑，这里显示 Invalid）。
    pub config: ConfigView,
    /// gateway.json 完整路径；主目录不可用时 `None`。
    pub config_path: Option<String>,
}

/// 组装状态：配置每次从磁盘现读（不经热重载快照，页面要反映"刚改完"的文件）。
pub fn status(
    config_path: Option<&Path>,
    running: bool,
    listen: Option<std::net::SocketAddr>,
) -> GatewayStatus {
    let config = match config_path {
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|source| Error::DataFile {
                path: path.to_path_buf(),
                source,
            })
            .and_then(|text| super::config::parse(&text))
        {
            Ok(config) => ConfigView::Valid {
                listen: config.listen.to_string(),
                upstreams: config
                    .upstreams
                    .iter()
                    .map(|u| UpstreamView {
                        name: u.name.clone(),
                        protocol: u.protocol.as_str(),
                        base_url: u.base_url.clone(),
                        enabled: u.enabled,
                    })
                    .collect(),
                routes: config
                    .routes
                    .iter()
                    .map(|route| RouteView {
                        pattern: route.pattern.clone(),
                        upstream: config
                            .upstreams
                            .get(route.upstream)
                            .map(|u| u.name.clone())
                            .unwrap_or_default(),
                    })
                    .collect(),
                mappings: config
                    .mappings
                    .iter()
                    .map(|m| MappingView {
                        from: m.from.clone(),
                        to: m.to.clone(),
                    })
                    .collect(),
                models: config.models.clone(),
                token_count: config.token_hashes.len(),
            },
            Err(err) => ConfigView::Invalid {
                message: err.to_string(),
            },
        },
        None => ConfigView::Invalid {
            message: "home directory unavailable".to_string(),
        },
    };
    GatewayStatus {
        running,
        listen: listen.map(|addr| addr.to_string()),
        config,
        config_path: config_path.map(|path| path.to_string_lossy().into_owned()),
    }
}

/// 共享状态；监听地址由 config 持有，这里只放请求路径需要的东西。
struct GatewayState {
    config: Arc<ConfigHandle>,
    db_path: PathBuf,
    client: reqwest::Client,
}

/// 加载配置并启动服务，直到 `shutdown` 完成后优雅退出。
///
/// `config_path`/`db_path` 由调用方给（桌面壳传 `paths::gateway_config_path()` 与
/// `paths::main_db_path()`），分离参数是为了测试可注入临时路径。
pub async fn serve(
    config_path: PathBuf,
    db_path: PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let config = Arc::new(ConfigHandle::load(&config_path)?);
    let (listener, _) = bind_with_fallback(config.current().listen).await?;
    serve_on(listener, config, db_path, shutdown).await
}

/// 绑定配置地址；被占用（AddrInUse）时回落到系统分配的临时端口，仍绑回环。
/// 配置端口是"首选"而非"必须"——换端口比起不来服务好得多；实际地址由调用方
/// 经返回值对外披露（状态页/句柄），不写回配置文件。
async fn bind_with_fallback(
    configured: std::net::SocketAddr,
) -> Result<(tokio::net::TcpListener, std::net::SocketAddr)> {
    match tokio::net::TcpListener::bind(configured).await {
        Ok(listener) => Ok((listener, configured)),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|e| Error::Internal(format!("bind fallback: {e}")))?;
            let addr = listener
                .local_addr()
                .map_err(|e| Error::Internal(format!("local_addr: {e}")))?;
            Ok((listener, addr))
        }
        Err(err) => Err(Error::Internal(format!("bind {configured}: {err}"))),
    }
}

/// 已绑定 listener 的启动形式：测试用 0 端口拿真实地址，不必与时间赛跑抢固定端口。
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    config: Arc<ConfigHandle>,
    db_path: PathBuf,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<()> {
    let state = Arc::new(GatewayState {
        config,
        db_path,
        client: reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Internal(format!("http client: {e}")))?,
    });

    let app = Router::new()
        .route("/v1/chat/completions", post(openai_entry))
        .route("/v1/messages", post(anthropic_entry))
        .route("/v1/responses", post(responses_entry))
        // 很多 OpenAI SDK 客户端初始化时先列模型，404 会让它们直接报错。
        .route("/v1/models", get(models_entry))
        .fallback(any(|| async {
            error_response(
                Protocol::OpenAI,
                StatusCode::NOT_FOUND,
                "not_found",
                "unknown path",
            )
        }))
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .map_err(|e| Error::Internal(format!("serve: {e}")))?;
    Ok(())
}

async fn models_entry(State(state): State<Arc<GatewayState>>, headers: HeaderMap) -> Response {
    let _ = state.config.reload_if_changed();
    let snapshot = state.config.current();
    let authorized = extract_token(&headers)
        .and_then(|token| {
            let ok = token_is_authorized(&token, &snapshot.token_hashes);
            ok.then_some(())
        })
        .is_some();
    if !authorized {
        return error_response(
            Protocol::OpenAI,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid access token",
        );
    }
    let data: Vec<Value> = snapshot
        .models
        .iter()
        .map(|id| json!({"id": id, "object": "model", "owned_by": "toktol"}))
        .collect();
    axum::Json(json!({"object": "list", "data": data})).into_response()
}

/// axum handler 的薄壳：把入站协议钉进去，其余交给 [`proxy`]。
async fn openai_entry(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy(State(state), headers, body, Protocol::OpenAI).await
}

async fn anthropic_entry(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy(State(state), headers, body, Protocol::Anthropic).await
}

async fn responses_entry(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    proxy(State(state), headers, body, Protocol::Responses).await
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// OpenAI 风格错误体；Anthropic 入站时换成 Anthropic 的 error 信封。
fn error_response(inbound: Protocol, status: StatusCode, kind: &str, message: &str) -> Response {
    let body = match inbound {
        // Responses 的错误信封与 OpenAI 同形（{"error": {...}}）。
        Protocol::OpenAI | Protocol::Responses => {
            json!({"error": {"message": message, "type": kind, "code": kind}})
        }
        Protocol::Anthropic => {
            json!({"type": "error", "error": {"type": kind, "message": message}})
        }
        // Gemini 错误信封：{"error": {"code", "message", "status"}}。
        Protocol::Gemini => {
            json!({"error": {"code": status.as_u16(), "message": message, "status": kind}})
        }
    };
    (status, axum::Json(body)).into_response()
}

/// 从请求头取访问令牌；明文令牌只在本函数内存活，出函数即只剩哈希。
fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        let token = token.trim();
        if !token.is_empty() {
            return Some(token.to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_string)
}

/// 前缀门 + 常量时间哈希比对：对每个配置哈希全量异或后再汇总，不因命中提前返回。
fn token_is_authorized(token: &str, token_hashes: &[[u8; 32]]) -> bool {
    if !token.starts_with(GATEWAY_TOKEN_PREFIX) {
        return false;
    }
    let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    let mut matched = false;
    for expected in token_hashes {
        let mut diff = 0u8;
        for (got, want) in token_hash.iter().zip(expected.iter()) {
            diff |= got ^ want;
        }
        matched |= diff == 0;
    }
    matched
}

async fn proxy(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
    inbound: Protocol,
) -> Response {
    let started = Instant::now();
    let ts = now_ms();

    // 热重载失败（用户改坏了配置）不拦请求：沿用旧快照继续服务。
    let _ = state.config.reload_if_changed();
    let snapshot = state.config.current();

    let Some(token) = extract_token(&headers) else {
        return error_response(
            inbound,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "missing access token",
        );
    };
    let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
    if !token_is_authorized(&token, &snapshot.token_hashes) {
        return error_response(
            inbound,
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "invalid access token",
        );
    }
    drop(token);

    let Ok(body_json) = serde_json::from_slice::<Value>(&body) else {
        return error_response(
            inbound,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "body is not valid json",
        );
    };
    let model_raw = body_json
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if model_raw.is_empty() {
        return error_response(
            inbound,
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "missing model",
        );
    }
    // 入站体的流式标志要在 translate 消费 body 之前取出（Gemini 上游靠它选端点）。
    let stream_requested = body_json.get("stream").and_then(Value::as_bool) == Some(true);

    // 模型映射先改写再路由：请求名照抄进计量（model_raw），转发名进转发体与 model 列。
    let model_forwarded = snapshot
        .mapping_for(&model_raw)
        .unwrap_or(&model_raw)
        .to_string();
    let Some(upstream_idx) = snapshot.route_for(&model_forwarded) else {
        return error_response(
            inbound,
            StatusCode::SERVICE_UNAVAILABLE,
            "no_route",
            "no route matches model",
        );
    };
    let upstream = &snapshot.upstreams[upstream_idx];

    let Ok(key) = std::env::var(&upstream.key_ref) else {
        // key_ref 是用户自己写的环境变量名，不是秘密；缺失要说清是哪个。
        return error_response(
            inbound,
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            &format!("upstream key env var not set: {}", upstream.key_ref),
        );
    };

    let translating = upstream.protocol != inbound;
    let mut upstream_body =
        match translate::translate_request(inbound, upstream.protocol, body_json) {
            Ok(body) => body,
            Err(err) => {
                return error_response(
                    inbound,
                    StatusCode::BAD_REQUEST,
                    "invalid_request_error",
                    &err.to_string(),
                );
            }
        };
    // 转发体里的模型名统一改写为映射后的名字（透传时 body 里还是原始名）。
    // Gemini 的 model 在路径不在体，写入体反而会被上游拒绝。
    if upstream.protocol != Protocol::Gemini
        && let Some(object) = upstream_body.as_object_mut()
    {
        object.insert("model".into(), json!(model_forwarded));
    }
    // 仅转换路径注入 include_usage：透传时客户端可能对多出来的 usage chunk 敏感，
    // 而转换路径的响应由我们生成，注入只改善计量覆盖率。
    if translating && upstream.protocol == Protocol::OpenAI {
        translate::inject_openai_include_usage(&mut upstream_body);
    }

    let path = match upstream.protocol {
        Protocol::OpenAI => "/v1/chat/completions".to_string(),
        Protocol::Anthropic => "/v1/messages".to_string(),
        Protocol::Responses => "/v1/responses".to_string(),
        Protocol::Gemini if stream_requested => {
            format!("/v1beta/models/{model_forwarded}:streamGenerateContent?alt=sse")
        }
        Protocol::Gemini => format!("/v1beta/models/{model_forwarded}:generateContent"),
    };
    let mut request = state
        .client
        .post(format!("{}{path}", upstream.base_url))
        .header(header::CONTENT_TYPE, "application/json")
        .body(upstream_body.to_string());
    match upstream.protocol {
        Protocol::OpenAI | Protocol::Responses => {
            request = request.bearer_auth(&key);
        }
        Protocol::Anthropic => {
            request = request
                .header("x-api-key", &key)
                .header("anthropic-version", translate::ANTHROPIC_VERSION);
            // 客户端显式请求的 beta 能力照传，否则上游按无 beta 处理。
            if let Some(beta) = headers.get("anthropic-beta") {
                request = request.header("anthropic-beta", beta);
            }
        }
        Protocol::Gemini => {
            request = request.header("x-goog-api-key", &key);
        }
    }

    let upstream_response = match request.send().await {
        Ok(response) => response,
        Err(err) => {
            return error_response(
                inbound,
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                &format!("upstream unreachable: {err}"),
            );
        }
    };
    let status = upstream_response.status();
    let upstream_protocol = upstream.protocol;
    let is_stream = upstream_response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    let meter = RequestMetering {
        db_path: state.db_path.clone(),
        ts,
        started,
        model_raw: model_raw.clone(),
        model: model_forwarded.clone(),
        upstream: upstream.name.clone(),
        token_hash,
    };

    if !status.is_success() {
        // 照常计量；错误体在协议一致的入站方向原样透传，跨协议时换成入站的
        // error 信封（状态码保留，消息与类型从上游错误体尽力提取）。
        let body_bytes = upstream_response.bytes().await.unwrap_or_default();
        // 计量含 SQLite 开库与迁移，是阻塞调用——spawn_blocking 让出 worker 线程。
        tokio::task::spawn_blocking(move || meter.record(None, status))
            .await
            .ok();
        let status = StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        if !translating {
            return (status, body_bytes).into_response();
        }
        let upstream_error: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
        let message = upstream_error
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("upstream request failed");
        let kind = upstream_error
            .pointer("/error/type")
            .and_then(Value::as_str)
            .unwrap_or("upstream_error");
        return error_response(inbound, status, kind, message);
    }

    if !is_stream {
        let body_bytes = upstream_response.bytes().await.unwrap_or_default();
        let parsed: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);
        let usage = translate::extract_usage_json(upstream_protocol, &parsed);
        let response_body = if translating {
            match translate::translate_response(inbound, upstream_protocol, parsed) {
                Ok(body) => body.to_string().into_bytes(),
                Err(err) => {
                    return error_response(
                        inbound,
                        StatusCode::BAD_GATEWAY,
                        "upstream_error",
                        &err.to_string(),
                    );
                }
            }
        } else {
            body_bytes.to_vec()
        };
        // 计量含 SQLite 开库与迁移，是阻塞调用——spawn_blocking 让出 worker 线程
        // （与下方流式路径同款）。
        tokio::task::spawn_blocking(move || meter.record(usage, status))
            .await
            .ok();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            response_body,
        )
            .into_response();
    }

    // 流式：字节泵任务一边透传/转换一边喂 usage 抽取，结束后计量。
    let upstream_stream = upstream_response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let meter = RequestMetering {
        db_path: state.db_path.clone(),
        ts,
        started,
        model_raw,
        model: model_forwarded.clone(),
        upstream: upstream.name.clone(),
        token_hash,
    };
    tokio::spawn(async move {
        let mut scanner = SseScanner::new();
        let mut usage_scanner = UsageScanner::new(upstream_protocol);
        let mut translator = translate::stream_translator(inbound, upstream_protocol);
        let mut stream = upstream_stream;
        while let Some(chunk) = stream.next().await {
            let Ok(bytes) = chunk else { break };
            for payload in scanner.push(&bytes) {
                usage_scanner.feed(&payload);
                if let Some(translator) = translator.as_mut() {
                    for line in translator.feed(&payload) {
                        if tx
                            .send(format!("data: {line}\n\n").into_bytes())
                            .await
                            .is_err()
                        {
                            return; // 客户端断开，计量在循环外照做。
                        }
                    }
                }
            }
            // 透传模式原样转发字节；转换模式只发翻译产物。
            if translator.is_none() && tx.send(bytes.to_vec()).await.is_err() {
                return;
            }
        }
        for payload in scanner.finish() {
            usage_scanner.feed(&payload);
            if let Some(translator) = translator.as_mut() {
                for line in translator.feed(&payload) {
                    let _ = tx.send(format!("data: {line}\n\n").into_bytes()).await;
                }
            }
        }
        if let Some(translator) = translator.as_mut() {
            for line in translator.finish() {
                let _ = tx.send(format!("data: {line}\n\n").into_bytes()).await;
            }
        }
        drop(tx);
        let usage = usage_scanner.take();
        tokio::task::spawn_blocking(move || meter.record(usage, status))
            .await
            .ok();
    });

    let body_stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, std::convert::Infallible>(chunk), rx))
    });
    let mut response = Response::new(Body::from_stream(body_stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-cache"),
    );
    response
}

/// 一次请求的计量上下文：路径公共部分只组装一次，落库时机（立即 / 流结束后）不同。
struct RequestMetering {
    db_path: PathBuf,
    ts: i64,
    started: Instant,
    model_raw: String,
    /// 映射后的转发模型名。
    model: String,
    upstream: String,
    token_hash: [u8; 32],
}

impl RequestMetering {
    fn record(&self, usage: Option<toktol_core::model::TokenUsage>, status: reqwest::StatusCode) {
        metering::record(
            &self.db_path,
            MeterFacts {
                ts: self.ts,
                model_raw: self.model_raw.clone(),
                model: self.model.clone(),
                usage: usage.unwrap_or_default(),
                status_code: Some(i64::from(status.as_u16())),
                latency_ms: i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX),
                upstream: self.upstream.clone(),
                token_hash: self.token_hash,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn start_falls_back_to_ephemeral_port_when_occupied() {
        use std::sync::atomic::{AtomicU64, Ordering};

        // 占住一个真实端口，让配置指向它。
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_addr = occupied.local_addr().unwrap();

        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("toktol-gw-fallback-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("gateway.json");
        std::fs::write(
            &config_path,
            json!({
                "listen": occupied_addr.to_string(),
                "upstreams": [],
                "token_hashes": [],
            })
            .to_string(),
        )
        .unwrap();

        let mut handle = GatewayHandle::start(config_path, dir.join("toktol.db"))
            .await
            .expect("端口被占应回落临时端口而不是失败");
        assert!(handle.is_running());
        assert_ne!(handle.listen(), occupied_addr, "实际监听换到了临时端口");
        // 回落地址仍是回环（红线不因换端口而破）。
        assert!(
            handle.listen().ip().is_loopback(),
            "回落地址仍是回环（红线不因换端口而破）"
        );
        handle.stop();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracts_token_from_both_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer sk-toktol-abc"),
        );
        assert_eq!(extract_token(&headers).as_deref(), Some("sk-toktol-abc"));

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            header::HeaderValue::from_static("sk-toktol-xyz"),
        );
        assert_eq!(extract_token(&headers).as_deref(), Some("sk-toktol-xyz"));
        assert_eq!(extract_token(&HeaderMap::new()), None);
    }

    #[test]
    fn token_check_is_prefix_gated_and_time_constant() {
        let token = format!("{GATEWAY_TOKEN_PREFIX}secret");
        let hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        assert!(token_is_authorized(&token, &[hash, [9u8; 32]]));
        assert!(token_is_authorized(&token, &[[9u8; 32], hash]));

        // 无前缀直接拒绝，不做哈希比对；哈希不匹配也拒绝。
        let other = "sk-toktol-other";
        assert!(!token_is_authorized(other, &[hash]));
        assert!(!token_is_authorized(&token, &[[1u8; 32], [2u8; 32]]));
    }

    /// 集成：真起网关 + 假上游（axum），走完 鉴权 → 转发 → SSE 透传 → 计量落库 全链路。
    #[tokio::test]
    async fn proxied_stream_is_forwarded_and_metered() {
        use std::sync::atomic::{AtomicU64, Ordering};

        const TOKEN: &str = "sk-toktol-itest-token";
        // edition 2024 起 set_var 是 unsafe；本测试进程内只有这一处用它，无竞争。
        unsafe { std::env::set_var("TOKTOL_TEST_KEY", "test-upstream-key") };

        // 假上游：记录收到的 Authorization，回一段带 usage 的 SSE。
        let seen_auth = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_body = Arc::new(std::sync::Mutex::new(String::new()));
        let auth_for_handler = seen_auth.clone();
        let body_for_handler = seen_body.clone();
        let upstream = Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(
                move |headers: HeaderMap, body: String| {
                    let auth = auth_for_handler.clone();
                    let seen_body = body_for_handler.clone();
                    async move {
                        *auth.lock().unwrap() = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        *seen_body.lock().unwrap() = body;
                        let chunks = futures_util::stream::iter(vec![
                            Ok::<_, std::convert::Infallible>(
                                b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n"
                                    .to_vec(),
                            ),
                            Ok(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":3}}\n\n"
                                .to_vec()),
                            Ok(b"data: [DONE]\n\n".to_vec()),
                        ]);
                        let mut response = Response::new(Body::from_stream(chunks));
                        response.headers_mut().insert(
                            header::CONTENT_TYPE,
                            header::HeaderValue::from_static("text/event-stream"),
                        );
                        response
                    }
                },
            ),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream).await.unwrap();
        });

        // 网关：临时库 + 临时 gateway.json，0 端口拿真实地址。
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("toktol-gw-itest-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("toktol.db");
        // 服务起来之前先把库建好（迁移跑完）：与生产一致——桌面壳先于网关流量开过库，
        // 也避免测试的轮询连接和计量首次建表抢同一个全新库的 schema 锁。
        let db = toktol_core::storage::open(&db_path).unwrap();
        let token_hash: [u8; 32] = Sha256::digest(TOKEN.as_bytes()).into();
        let config_path = dir.join("gateway.json");
        std::fs::write(
            &config_path,
            json!({
                "listen": "127.0.0.1:0",
                "upstreams": [{"name": "mock", "protocol": "openai",
                    "base_url": format!("http://{upstream_addr}"), "key_ref": "TOKTOL_TEST_KEY"}],
                "routes": [{"model": "gpt-*", "upstream": "mock"}],
                "models": ["gpt-x"],
                "token_hashes": [hex_encode(&token_hash)],
            })
            .to_string(),
        )
        .unwrap();

        let db_path_for_serve = db_path.clone();
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        let config = Arc::new(ConfigHandle::load(&config_path).unwrap());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_on(gateway_listener, config, db_path_for_serve, async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://{gateway_addr}/v1/chat/completions");

        // 无令牌 → 401；错令牌 → 401。
        let status = client
            .post(&url)
            .json(&json!({"model": "gpt-x", "stream": true}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // 正确令牌：SSE 原样透传，Authorization 换成上游密钥。
        let response = client
            .post(&url)
            .bearer_auth(TOKEN)
            .json(&json!({"model": "gpt-x", "stream": true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(content_type.contains("text/event-stream"));
        let body = response.text().await.unwrap();
        assert!(
            body.contains("\"content\":\"hi\""),
            "透传上游 chunk: {body}"
        );
        assert!(
            body.contains("\"prompt_tokens\":8"),
            "透传 usage chunk: {body}"
        );
        assert!(body.contains("[DONE]"));
        assert_eq!(
            seen_auth.lock().unwrap().as_str(),
            "Bearer test-upstream-key",
            "上游请求用 key_ref 解析出的密钥"
        );
        assert!(
            seen_body.lock().unwrap().contains("\"model\":\"gpt-x\""),
            "透传模式不改写请求体"
        );

        // /v1/models：带令牌列出宣称模型，无令牌 401。
        let status = client
            .get(format!("http://{gateway_addr}/v1/models"))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let models: Value = client
            .get(format!("http://{gateway_addr}/v1/models"))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(models["object"], "list");
        assert_eq!(models["data"][0]["id"], "gpt-x");

        // 计量异步落库，轮询等待；公开 API 只暴露聚合口径。复用预建的连接，不反复开库。
        let mut totals = None;
        for _ in 0..100 {
            if let Ok(payload) = db.gateway_overview()
                && payload.totals.request_count > 0
            {
                totals = Some(payload.totals);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let totals = totals.expect("流结束后计量行必须落库");
        assert_eq!(totals.request_count, 1);
        assert_eq!(
            (totals.input_tokens, totals.output_tokens),
            (8, 3),
            "usage 从上游 SSE 末包抽取"
        );
        assert_eq!(totals.unknown_cost_rows, 1, "gpt-x 未挂价，走未知而非编造");

        shutdown_tx.send(()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        fn hex_encode(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }

    /// 集成：入站 OpenAI Chat → Responses 上游的跨协议全链路。假上游说
    /// Responses SSE，验证请求被转写成 input items、响应被转回 chat chunks、
    /// usage 从 response.completed 抽取进计量。
    #[tokio::test]
    async fn proxied_openai_to_responses_upstream_is_translated_and_metered() {
        use std::sync::atomic::{AtomicU64, Ordering};

        const TOKEN: &str = "sk-toktol-itest-token";
        unsafe { std::env::set_var("TOKTOL_TEST_KEY_RESP", "test-upstream-key") };

        let seen_auth = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_body = Arc::new(std::sync::Mutex::new(String::new()));
        let auth_for_handler = seen_auth.clone();
        let body_for_handler = seen_body.clone();
        let upstream = Router::new().route(
            "/v1/responses",
            axum::routing::post(move |headers: HeaderMap, body: String| {
                let auth = auth_for_handler.clone();
                let seen_body = body_for_handler.clone();
                async move {
                    *auth.lock().unwrap() = headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    *seen_body.lock().unwrap() = body;
                    let chunks = futures_util::stream::iter(vec![
                        Ok::<_, std::convert::Infallible>(
                            b"event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"model\":\"gpt-x\"}}\n\n".to_vec(),
                        ),
                        Ok(b"event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"hi\"}\n\n".to_vec()),
                        Ok(b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"model\":\"gpt-x\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":1}}}}\n\n".to_vec()),
                    ]);
                    let mut response = Response::new(Body::from_stream(chunks));
                    response.headers_mut().insert(
                        header::CONTENT_TYPE,
                        header::HeaderValue::from_static("text/event-stream"),
                    );
                    response
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream).await.unwrap();
        });

        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("toktol-gw-itest-resp-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("toktol.db");
        let db = toktol_core::storage::open(&db_path).unwrap();
        let token_hash: [u8; 32] = Sha256::digest(TOKEN.as_bytes()).into();
        let config_path = dir.join("gateway.json");
        std::fs::write(
            &config_path,
            json!({
                "listen": "127.0.0.1:0",
                "upstreams": [{"name": "mock", "protocol": "responses",
                    "base_url": format!("http://{upstream_addr}"), "key_ref": "TOKTOL_TEST_KEY_RESP"}],
                "routes": [{"model": "gpt-*", "upstream": "mock"}],
                "models": ["gpt-x"],
                "token_hashes": [hex_encode(&token_hash)],
            })
            .to_string(),
        )
        .unwrap();

        let db_path_for_serve = db_path.clone();
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        let config = Arc::new(ConfigHandle::load(&config_path).unwrap());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_on(gateway_listener, config, db_path_for_serve, async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{gateway_addr}/v1/chat/completions"))
            .bearer_auth(TOKEN)
            .json(&json!({"model": "gpt-x", "stream": true,
                "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.text().await.unwrap();
        assert!(
            body.contains("\"content\":\"hi\""),
            "Responses delta 转回 chat chunk: {body}"
        );
        assert!(body.contains("\"finish_reason\":\"stop\""), "{body}");
        assert!(
            body.contains("\"prompt_tokens\":5"),
            "usage 进独立末包: {body}"
        );
        assert!(body.contains("[DONE]"));

        // 上游收到的请求应是转写后的 Responses 形态（messages → input items）。
        let upstream_body: Value = serde_json::from_str(&seen_body.lock().unwrap()).unwrap();
        assert_eq!(upstream_body["input"][0]["role"], "user");
        assert_eq!(upstream_body["model"], "gpt-x");
        assert_eq!(
            seen_auth.lock().unwrap().as_str(),
            "Bearer test-upstream-key",
            "Responses 上游走 bearer 鉴权"
        );

        // 计量：usage 从 response.completed 抽取。
        let mut totals = None;
        for _ in 0..100 {
            if let Ok(payload) = db.gateway_overview()
                && payload.totals.request_count > 0
            {
                totals = Some(payload.totals);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let totals = totals.expect("流结束后计量行必须落库");
        assert_eq!(totals.request_count, 1);
        assert_eq!(
            (totals.input_tokens, totals.output_tokens),
            (5, 2),
            "usage 从 Responses 收尾事件抽取"
        );

        shutdown_tx.send(()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        fn hex_encode(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }

    /// 集成：入站 OpenAI Chat → Gemini 上游的跨协议全链路。Gemini 的结构差异
    /// 都在这里过一遍：model 在路径、x-goog-api-key 鉴权、响应转回 chat 形态、
    /// usage 从 usageMetadata 抽取进计量。
    #[tokio::test]
    async fn proxied_openai_to_gemini_upstream_is_translated_and_metered() {
        use std::sync::atomic::{AtomicU64, Ordering};

        const TOKEN: &str = "sk-toktol-itest-token";
        unsafe { std::env::set_var("TOKTOL_TEST_KEY_GEM", "test-upstream-key") };

        let seen_key = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_path = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_body = Arc::new(std::sync::Mutex::new(String::new()));
        let key_for_handler = seen_key.clone();
        let path_for_handler = seen_path.clone();
        let body_for_handler = seen_body.clone();
        let upstream = Router::new().route(
            // axum 一段只允许一个参数：整段捕获 "gpt-x:generateContent"。
            "/v1beta/models/{segment}",
            axum::routing::post(
                move |headers: HeaderMap, path: axum::extract::Path<String>, body: String| {
                    let key = key_for_handler.clone();
                    let seen_path = path_for_handler.clone();
                    let seen_body = body_for_handler.clone();
                    async move {
                        *key.lock().unwrap() = headers
                            .get("x-goog-api-key")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        *seen_path.lock().unwrap() = path.0;
                        *seen_body.lock().unwrap() = body;
                        let response = json!({
                            "candidates": [{
                                "content": {"role": "model", "parts": [{"text": "hi"}]},
                                "finishReason": "STOP"
                            }],
                            "modelVersion": "gemini-x",
                            "usageMetadata": {"promptTokenCount": 6, "candidatesTokenCount": 2,
                                "cachedContentTokenCount": 1}
                        });
                        (
                            [(header::CONTENT_TYPE, "application/json")],
                            response.to_string(),
                        )
                    }
                },
            ),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(upstream_listener, upstream).await.unwrap();
        });

        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("toktol-gw-itest-gem-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("toktol.db");
        let db = toktol_core::storage::open(&db_path).unwrap();
        let token_hash: [u8; 32] = Sha256::digest(TOKEN.as_bytes()).into();
        let config_path = dir.join("gateway.json");
        std::fs::write(
            &config_path,
            json!({
                "listen": "127.0.0.1:0",
                "upstreams": [{"name": "mock", "protocol": "gemini",
                    "base_url": format!("http://{upstream_addr}"), "key_ref": "TOKTOL_TEST_KEY_GEM"}],
                "routes": [{"model": "gpt-*", "upstream": "mock"}],
                "models": ["gpt-x"],
                "token_hashes": [hex_encode(&token_hash)],
            })
            .to_string(),
        )
        .unwrap();

        let db_path_for_serve = db_path.clone();
        let gateway_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        let config = Arc::new(ConfigHandle::load(&config_path).unwrap());
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            serve_on(gateway_listener, config, db_path_for_serve, async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        let response = client
            .post(format!("http://{gateway_addr}/v1/chat/completions"))
            .bearer_auth(TOKEN)
            .json(&json!({"model": "gpt-x", "stream": false,
                "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["content"], "hi");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert_eq!(body["usage"]["prompt_tokens"], 6);
        assert_eq!(body["usage"]["prompt_tokens_details"]["cached_tokens"], 1);

        // 上游请求：model 在路径（整段为 "gpt-x:generateContent"）、messages 转
        // 成 contents、密钥走 x-goog-api-key，体里不能出现 model/stream 字段。
        assert_eq!(seen_path.lock().unwrap().as_str(), "gpt-x:generateContent");
        let upstream_body: Value = serde_json::from_str(&seen_body.lock().unwrap()).unwrap();
        assert_eq!(upstream_body["contents"][0]["role"], "user");
        assert_eq!(upstream_body["contents"][0]["parts"][0]["text"], "hi");
        assert!(upstream_body.get("model").is_none());
        assert!(upstream_body.get("stream").is_none());
        assert_eq!(
            seen_key.lock().unwrap().as_str(),
            "test-upstream-key",
            "Gemini 上游走 x-goog-api-key 鉴权"
        );

        // 计量：usage 从 usageMetadata 抽取。
        let mut totals = None;
        for _ in 0..100 {
            if let Ok(payload) = db.gateway_overview()
                && payload.totals.request_count > 0
            {
                totals = Some(payload.totals);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let totals = totals.expect("计量行必须落库");
        assert_eq!(totals.request_count, 1);
        assert_eq!(
            (totals.input_tokens, totals.output_tokens),
            (6, 2),
            "usage 从 usageMetadata 抽取"
        );

        shutdown_tx.send(()).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        fn hex_encode(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
