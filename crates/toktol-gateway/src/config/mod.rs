//! 网关配置：`~/.toktol/gateway.json`（上游、路由、模型映射、令牌哈希），持
//! 快照支持热重载。契约变化（阶段 4 后期）：文件从"纯用户手写"转为**应用管理**——
//! 上游/映射/令牌经本模块的写入器增删改，写入前整份校验、失败不落盘；手改文件
//! 仍兼容（mtime 热重载）。
//! 隐私红线：上游密钥只存环境变量名（`key_ref`），访问令牌只存 sha256 哈希——
//! 本模块的任何类型、错误信息、测试都不得引入明文形态。

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use arc_swap::{ArcSwap, Guard};
use serde::Deserialize;

use toktol_core::error::{Error, Result};
use toktol_core::paths::GATEWAY_TOKEN_PREFIX;

/// 默认监听地址：只绑回环。端口无外部依据，选一个不与常见本地服务冲突的高位端口。
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8412";

/// 上游 API 协议。值进 gateway.json 的 `protocol` 字段，取值即契约。
/// 四种协议既可作入站也可作上游；协议不一致时经 translate 层互转。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// OpenAI 兼容（/v1/chat/completions）。
    OpenAI,
    /// OpenAI Responses（/v1/responses）。
    Responses,
    /// Anthropic Messages（/v1/messages）。
    Anthropic,
    /// Google Gemini（/v1beta/models/{model}:generateContent）。
    Gemini,
}

impl Protocol {
    fn parse_upstream(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(Protocol::OpenAI),
            "anthropic" => Some(Protocol::Anthropic),
            "responses" => Some(Protocol::Responses),
            "gemini" => Some(Protocol::Gemini),
            _ => None,
        }
    }

    /// serde/前端显示用的字符串形态。
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::OpenAI => "openai",
            Protocol::Responses => "responses",
            Protocol::Anthropic => "anthropic",
            Protocol::Gemini => "gemini",
        }
    }
}

/// 一个上游 API 供应商。
#[derive(Debug, Clone)]
pub struct Upstream {
    /// 配置内引用的名字（路由指向它）。
    pub name: String,
    /// 上游说的协议。
    pub protocol: Protocol,
    /// API 根地址（如 `https://api.anthropic.com`），不含路径。
    pub base_url: String,
    /// 上游密钥所在的**环境变量名**；请求时才读取，绝不记录值。
    pub key_ref: String,
    /// 关闭后不参与路由（等同不存在），配置保留。
    pub enabled: bool,
}

/// 一条模型映射：请求里的模型名 `from`（精确匹配）改写为 `to` 后再路由与转发。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelMapping {
    /// 本地应用请求的模型名。
    pub from: String,
    /// 转发给上游的模型名。
    pub to: String,
}

/// 一条模型路由：模型名匹配 [`Route::pattern`] 的请求发给对应上游。
#[derive(Debug, Clone)]
pub struct Route {
    /// 模型名匹配模式，`*` 通配一个片段（如 `claude-*`）；首个匹配生效。
    pub pattern: String,
    /// 目标上游在 [`GatewayConfig::upstreams`] 里的下标。
    pub upstream: usize,
}

/// 校验通过的网关配置快照。字段都是解析期定型的，热重载整份替换。
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    /// 监听地址；校验保证只绑回环。
    pub listen: SocketAddr,
    pub upstreams: Vec<Upstream>,
    pub routes: Vec<Route>,
    /// 无路由命中时的上游下标；未配置则为 `None`（请求得 503）。
    pub default_upstream: Option<usize>,
    /// 访问令牌的 sha256（原始 32 字节）；明文令牌永不进入本结构。
    pub token_hashes: Vec<[u8; 32]>,
    /// 对外宣称的模型列表（`/v1/models` 用）。未配置时回落为路由里的**精确**
    /// 模式（含 `*` 的通配不是合法模型 id，不进列表）；两者皆空则列表为空。
    pub models: Vec<String>,
    /// 模型映射表（精确匹配改名，改写后再路由）。
    pub mappings: Vec<ModelMapping>,
}

impl GatewayConfig {
    /// 按配置顺序找第一条命中且**启用中**的路由；无命中回落 `default_upstream`。
    pub fn route_for(&self, model: &str) -> Option<usize> {
        self.routes
            .iter()
            .find(|route| {
                model_matches(&route.pattern, model)
                    && self
                        .upstreams
                        .get(route.upstream)
                        .is_some_and(|u| u.enabled)
            })
            .map(|route| route.upstream)
            .or(self.default_upstream)
            .filter(|idx| self.upstreams.get(*idx).is_some_and(|u| u.enabled))
    }

    /// 模型映射（精确匹配）；命中则请求模型名改写为返回值。
    pub fn mapping_for(&self, model: &str) -> Option<&str> {
        self.mappings
            .iter()
            .find(|mapping| mapping.from == model)
            .map(|mapping| mapping.to.as_str())
    }
}

/// `*` 只通配一个片段：`claude-*` 匹配 `claude-sonnet-5` 但不匹配 `a/claude-x`。
fn model_matches(pattern: &str, model: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == model,
        Some((prefix, suffix)) => {
            model.len() >= prefix.len() + suffix.len()
                && model.starts_with(prefix)
                && model.ends_with(suffix)
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawUpstream {
    name: String,
    protocol: String,
    base_url: String,
    key_ref: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct RawRoute {
    model: String,
    upstream: String,
}

#[derive(Debug, Deserialize)]
struct RawMapping {
    from: String,
    to: String,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    listen: Option<String>,
    #[serde(default)]
    upstreams: Vec<RawUpstream>,
    #[serde(default)]
    routes: Vec<RawRoute>,
    default_upstream: Option<String>,
    #[serde(default)]
    token_hashes: Vec<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    model_mappings: Vec<RawMapping>,
}

fn invalid(what: impl Into<String>) -> Error {
    Error::GatewayConfig(what.into())
}

/// 解析并校验配置文本。校验失败一律 [`Error::GatewayConfig`]，消息只描述问题，
/// 不携带文件内容（token 哈希与 key_ref 都在文件里）。
pub fn parse(text: &str) -> Result<GatewayConfig> {
    let raw: RawConfig = serde_json::from_str(text)?;

    let listen = match &raw.listen {
        Some(text) => text
            .parse::<SocketAddr>()
            .map_err(|_| invalid(format!("listen 不是合法地址: {text}")))?,
        None => DEFAULT_LISTEN.parse().expect("默认地址常量必须可解析"),
    };
    if !listen.ip().is_loopback() {
        return Err(invalid(format!("listen 必须是回环地址: {listen}")));
    }

    let mut upstreams = Vec::with_capacity(raw.upstreams.len());
    for raw_upstream in raw.upstreams {
        if raw_upstream.name.is_empty() {
            return Err(invalid("upstream.name 不能为空"));
        }
        if upstreams
            .iter()
            .any(|u: &Upstream| u.name == raw_upstream.name)
        {
            return Err(invalid(format!("upstream 名重复: {}", raw_upstream.name)));
        }
        if !raw_upstream.base_url.starts_with("https://")
            && !raw_upstream.base_url.starts_with("http://127.0.0.1")
            && !raw_upstream.base_url.starts_with("http://localhost")
        {
            return Err(invalid(format!(
                "upstream.base_url 必须是 https（或本地回环 http）: {}",
                raw_upstream.name
            )));
        }
        if raw_upstream.key_ref.is_empty() {
            return Err(invalid(format!(
                "upstream.key_ref 不能为空: {}",
                raw_upstream.name
            )));
        }
        let protocol = Protocol::parse_upstream(&raw_upstream.protocol).ok_or_else(|| {
            invalid(format!(
                "upstream.protocol 只支持 openai/anthropic/responses/gemini: {}",
                raw_upstream.name
            ))
        })?;
        upstreams.push(Upstream {
            name: raw_upstream.name,
            protocol,
            base_url: raw_upstream.base_url.trim_end_matches('/').to_string(),
            key_ref: raw_upstream.key_ref,
            enabled: raw_upstream.enabled,
        });
    }

    let upstream_index = |name: &str| {
        upstreams
            .iter()
            .position(|u| u.name == name)
            .ok_or_else(|| invalid(format!("路由引用了不存在的 upstream: {name}")))
    };
    let mut routes = Vec::with_capacity(raw.routes.len());
    for raw_route in raw.routes {
        if raw_route.model.is_empty() {
            return Err(invalid("route.model 不能为空"));
        }
        routes.push(Route {
            pattern: raw_route.model,
            upstream: upstream_index(&raw_route.upstream)?,
        });
    }
    let default_upstream = raw
        .default_upstream
        .as_deref()
        .map(upstream_index)
        .transpose()?;

    let mut token_hashes = Vec::with_capacity(raw.token_hashes.len());
    for hex in raw.token_hashes {
        let bytes = decode_sha256_hex(&hex)
            .ok_or_else(|| invalid("token_hashes 里存在非 64 位十六进制的项"))?;
        token_hashes.push(bytes);
    }

    // 宣称列表：显式 models 优先；否则取路由里的精确模式（通配模式不是合法 id）。
    let models = if raw.models.is_empty() {
        routes
            .iter()
            .map(|route| route.pattern.clone())
            .filter(|pattern| !pattern.contains('*'))
            .collect()
    } else {
        raw.models
    };

    let mut mappings = Vec::with_capacity(raw.model_mappings.len());
    for raw_mapping in raw.model_mappings {
        if raw_mapping.from.is_empty() || raw_mapping.to.is_empty() {
            return Err(invalid("model_mappings 的 from/to 都不能为空"));
        }
        if mappings
            .iter()
            .any(|m: &ModelMapping| m.from == raw_mapping.from)
        {
            return Err(invalid(format!(
                "model_mappings.from 重复: {}",
                raw_mapping.from
            )));
        }
        mappings.push(ModelMapping {
            from: raw_mapping.from,
            to: raw_mapping.to,
        });
    }

    Ok(GatewayConfig {
        listen,
        upstreams,
        routes,
        default_upstream,
        token_hashes,
        models,
        mappings,
    })
}

/// 64 位十六进制 → 32 字节；不引入 hex 依赖。
fn decode_sha256_hex(hex: &str) -> Option<[u8; 32]> {
    let bytes = hex.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// 配置快照句柄：进程内共享 + mtime 探测的热重载。
///
/// 不用文件监视依赖（notify）：请求前 stat 一次 mtime 足够廉价，用户手写配置的
/// 修改频率远低于请求频率。重载失败（文件被半写、改坏了）时**保留上一份好快照**，
/// 不让正在服务的网关陪葬——下次修改成功后再切换。
pub struct ConfigHandle {
    path: PathBuf,
    snapshot: ArcSwap<GatewayConfig>,
    last_mtime: Mutex<Option<SystemTime>>,
}

impl ConfigHandle {
    /// 读取并校验配置，失败则整体失败（启动期宁可拒绝服务也不带病运行）。
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::DataFile {
            path: path.to_path_buf(),
            source,
        })?;
        let snapshot = Arc::new(parse(&text)?);
        let mtime = file_mtime(path);
        Ok(Self {
            path: path.to_path_buf(),
            snapshot: ArcSwap::from(snapshot),
            last_mtime: Mutex::new(mtime),
        })
    }

    /// 当前快照。
    pub fn current(&self) -> Guard<Arc<GatewayConfig>> {
        self.snapshot.load()
    }

    /// mtime 变了才重读重校验。返回是否发生了切换；校验失败保留旧快照并返回错误
    /// （调用方决定是否上报，服务照常）。
    pub fn reload_if_changed(&self) -> Result<bool> {
        let mtime = file_mtime(&self.path);
        {
            let last = self.last_mtime.lock().expect("mtime 锁不应中毒");
            if mtime == *last {
                return Ok(false);
            }
        }
        let text = std::fs::read_to_string(&self.path).map_err(|source| Error::DataFile {
            path: self.path.clone(),
            source,
        })?;
        let next = Arc::new(parse(&text)?);
        *self.last_mtime.lock().expect("mtime 锁不应中毒") = mtime;
        self.snapshot.store(next);
        Ok(true)
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// 签发一个新访问令牌：`sk-toktol-` 前缀 + 32 位随机字母数字。
/// 明文只应经 IPC 返回给用户一次；落盘的永远是 [`append_token_hash`] 写的哈希。
pub fn generate_token() -> String {
    use rand::distr::{Alphanumeric, SampleString};
    format!(
        "{GATEWAY_TOKEN_PREFIX}{}",
        Alphanumeric.sample_string(&mut rand::rng(), 32)
    )
}

/// 配置文件写入的公共收口：读（不存在则给最小骨架）→ 修改 → 整份校验 → pretty 写回。
/// 校验失败时**不落盘**——用户手改了一半的文件不会因为一次失败的 UI 操作被毁掉。
fn edit_config_file(
    path: &Path,
    mutate: impl FnOnce(&mut serde_json::Value) -> Result<()>,
) -> Result<()> {
    let document = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)?,
        // 首次经 UI 配置时文件可能还不存在：从最小骨架开始。
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(source) => {
            return Err(Error::DataFile {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut document = document;
    mutate(&mut document)?;
    let rendered = serde_json::to_string_pretty(&document)?;
    parse(&rendered)?;
    toktol_core::fsutil::write_file_atomic(path, &format!("{rendered}\n")).map_err(|source| {
        Error::DataFile {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(())
}

/// `gateway.json` 不存在时写一份最小可用骨架（默认回环地址 + 空上游），
/// 让"启动服务"不依赖用户先手写文件——启动后 UI 的上游/映射写入器接管配置。
/// 返回是否真的创建了文件；已存在时不动（哪怕内容非法——那是校验层的事）。
pub fn ensure_default(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::DataFile {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let rendered = serde_json::to_string_pretty(&serde_json::json!({
        "listen": DEFAULT_LISTEN,
        "upstreams": [],
        "routes": [],
        "token_hashes": [],
    }))?;
    toktol_core::fsutil::write_file_atomic(path, &format!("{rendered}\n")).map_err(|source| {
        Error::DataFile {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(true)
}

/// UI 侧提交的上游字段（serde camelCase 对齐前端；缺了 rename_all 前端的
/// `baseUrl`/`keyRef` 反序列化会直接失败——校验见前端 verify:api 脚本）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamInput {
    /// 上游名（唯一标识；更新时允许改名，引用它的路由与默认上游同步改写）。
    pub name: String,
    /// "openai" | "anthropic"。
    pub protocol: String,
    /// API 根地址。
    pub base_url: String,
    /// 上游密钥所在的环境变量名。
    pub key_ref: String,
    /// 是否参与路由。
    pub enabled: bool,
}

fn upstream_array_mut(document: &mut serde_json::Value) -> Result<&mut Vec<serde_json::Value>> {
    document
        .as_object_mut()
        .ok_or_else(|| invalid("gateway.json 顶层必须是对象"))?
        .entry("upstreams")
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| invalid("upstreams 必须是数组"))
}

fn upstream_input_to_value(input: &UpstreamInput) -> serde_json::Value {
    serde_json::json!({
        "name": input.name,
        "protocol": input.protocol,
        "base_url": input.base_url,
        "key_ref": input.key_ref,
        "enabled": input.enabled,
    })
}

/// 新增上游；名字与既有项重复会被校验拦下。
pub fn upstream_add(path: &Path, input: &UpstreamInput) -> Result<()> {
    edit_config_file(path, |document| {
        upstream_array_mut(document)?.push(upstream_input_to_value(input));
        Ok(())
    })
}

/// 更新上游（按 `name` 定位；`input.name` 允许改名，路由与 default_upstream 里的
/// 旧名同步改写）。找不到该名字时报错。
pub fn upstream_update(path: &Path, name: &str, input: &UpstreamInput) -> Result<()> {
    edit_config_file(path, |document| {
        let list = upstream_array_mut(document)?;
        let entry = list
            .iter_mut()
            .find(|upstream| upstream.get("name").and_then(serde_json::Value::as_str) == Some(name))
            .ok_or_else(|| invalid(format!("upstream 不存在: {name}")))?;
        *entry = upstream_input_to_value(input);

        if input.name != name {
            let rename = |value: &mut serde_json::Value| {
                if value.as_str() == Some(name) {
                    *value = serde_json::Value::String(input.name.clone());
                }
            };
            if let Some(routes) = document.get_mut("routes").and_then(|r| r.as_array_mut()) {
                for route in routes {
                    if let Some(upstream) = route.get_mut("upstream") {
                        rename(upstream);
                    }
                }
            }
            if let Some(default) = document.get_mut("default_upstream") {
                rename(default);
            }
        }
        Ok(())
    })
}

/// 删除上游；引用它的路由一并删除，default_upstream 指向它时清空。
pub fn upstream_delete(path: &Path, name: &str) -> Result<()> {
    edit_config_file(path, |document| {
        let list = upstream_array_mut(document)?;
        let before = list.len();
        list.retain(|upstream| {
            upstream.get("name").and_then(serde_json::Value::as_str) != Some(name)
        });
        if list.len() == before {
            return Err(invalid(format!("upstream 不存在: {name}")));
        }
        if let Some(routes) = document.get_mut("routes").and_then(|r| r.as_array_mut()) {
            routes.retain(|route| {
                route.get("upstream").and_then(serde_json::Value::as_str) != Some(name)
            });
        }
        if document
            .get("default_upstream")
            .and_then(serde_json::Value::as_str)
            == Some(name)
            && let Some(object) = document.as_object_mut()
        {
            object.remove("default_upstream");
        }
        Ok(())
    })
}

/// 新增或覆盖一条模型映射（同 from 覆盖）。
pub fn mapping_add(path: &Path, from: &str, to: &str) -> Result<()> {
    edit_config_file(path, |document| {
        let mappings = document
            .as_object_mut()
            .ok_or_else(|| invalid("gateway.json 顶层必须是对象"))?
            .entry("model_mappings")
            .or_insert_with(|| serde_json::json!([]));
        let Some(list) = mappings.as_array_mut() else {
            return Err(invalid("model_mappings 必须是数组"));
        };
        if let Some(existing) = list
            .iter_mut()
            .find(|m| m.get("from").and_then(serde_json::Value::as_str) == Some(from))
        {
            *existing = serde_json::json!({"from": from, "to": to});
        } else {
            list.push(serde_json::json!({"from": from, "to": to}));
        }
        Ok(())
    })
}

/// 删除一条模型映射；不存在时报错。
pub fn mapping_delete(path: &Path, from: &str) -> Result<()> {
    edit_config_file(path, |document| {
        let mappings = document
            .as_object_mut()
            .ok_or_else(|| invalid("gateway.json 顶层必须是对象"))?
            .get_mut("model_mappings")
            .ok_or_else(|| invalid(format!("模型映射不存在: {from}")))?;
        let Some(list) = mappings.as_array_mut() else {
            return Err(invalid("model_mappings 必须是数组"));
        };
        let before = list.len();
        list.retain(|m| m.get("from").and_then(serde_json::Value::as_str) != Some(from));
        if list.len() == before {
            return Err(invalid(format!("模型映射不存在: {from}")));
        }
        Ok(())
    })
}

/// 把令牌的 sha256 追加进 gateway.json 的 `token_hashes` 并写回（幂等）。
pub fn append_token_hash(path: &Path, token: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let hash: String = Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    edit_config_file(path, |document| {
        let list = document
            .as_object_mut()
            .ok_or_else(|| invalid("gateway.json 顶层必须是对象"))?
            .entry("token_hashes")
            .or_insert_with(|| serde_json::json!([]));
        let Some(list) = list.as_array_mut() else {
            return Err(invalid("token_hashes 必须是数组"));
        };
        if list.iter().any(|entry| entry == hash.as_str()) {
            return Ok(());
        }
        list.push(serde_json::Value::String(hash));
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config_text() -> String {
        format!(
            r#"{{
  "upstreams": [{{"name": "anthropic", "protocol": "anthropic", "base_url": "https://api.anthropic.com", "key_ref": "ANTHROPIC_API_KEY"}}],
  "routes": [{{"model": "claude-*", "upstream": "anthropic"}}],
  "default_upstream": "anthropic",
  "token_hashes": ["{}"]
}}"#,
            "ab".repeat(32)
        )
    }

    /// 在 upstreams 数组里追加一个 openai 上游，供 reload 测试制造内容变化。
    fn config_with_second_upstream() -> String {
        valid_config_text().replace(
            r#""ANTHROPIC_API_KEY"}]"#,
            r#""ANTHROPIC_API_KEY"}, {"name": "openai", "protocol": "openai", "base_url": "https://api.openai.com", "key_ref": "OPENAI_API_KEY"}]"#,
        )
    }

    fn upstream_input(name: &str, protocol: &str) -> UpstreamInput {
        UpstreamInput {
            name: name.into(),
            protocol: protocol.into(),
            base_url: "https://api.example.com".into(),
            key_ref: "EXAMPLE_API_KEY".into(),
            enabled: true,
        }
    }

    #[test]
    fn ensure_default_bootstraps_missing_file_and_keeps_existing() {
        let dir = std::env::temp_dir().join(format!("toktol-gw-def-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("nested").join("gateway.json");

        assert!(ensure_default(&path).unwrap(), "缺失时创建");
        let config = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.listen.to_string(), DEFAULT_LISTEN);
        assert!(config.upstreams.is_empty());

        // 已存在（哪怕改过）不动。
        std::fs::write(&path, valid_config_text()).unwrap();
        assert!(!ensure_default(&path).unwrap());
        let config = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.upstreams.len(), 1, "既有配置未被覆盖");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn upstream_crud_writes_and_renames_references() {
        let dir = std::env::temp_dir().join(format!("toktol-gw-crud-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gateway.json");
        std::fs::write(&path, valid_config_text()).unwrap();

        // 新增 + 指向它的路由。
        upstream_add(&path, &upstream_input("openai", "openai")).unwrap();
        let config = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.upstreams.len(), 2);

        // 改名：路由与 default_upstream 的引用同步改写。
        upstream_update(&path, "anthropic", &upstream_input("claude", "anthropic")).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"claude\""), "新名已写入");
        let config = parse(&text).unwrap();
        assert!(
            config
                .routes
                .iter()
                .all(|r| config.upstreams[r.upstream].name == "claude")
        );
        assert_eq!(config.default_upstream, Some(0));

        // 删除未被引用的 openai：路由不变。
        upstream_delete(&path, "openai").unwrap();
        let config = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.upstreams.len(), 1);
        assert_eq!(config.routes.len(), 1);

        // 删除被引用的 claude：引用它的路由与 default_upstream 一并清理（设计如此，
        // 见 upstream_delete 文档），不留悬空引用。
        upstream_delete(&path, "claude").unwrap();
        let config = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.upstreams.len(), 0);
        assert_eq!(config.routes.len(), 0, "claude 的路由已随之删除");
        assert_eq!(config.default_upstream, None, "default 指向它时被清空");
        assert!(matches!(
            upstream_delete(&path, "claude"),
            Err(Error::GatewayConfig(_))
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mapping_add_is_upsert_and_delete_errors_when_missing() {
        let dir = std::env::temp_dir().join(format!("toktol-gw-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gateway.json");
        std::fs::write(&path, valid_config_text()).unwrap();

        mapping_add(&path, "gpt-x", "claude-sonnet-5").unwrap();
        mapping_add(&path, "gpt-x", "claude-opus-5").unwrap();
        let config = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.mappings.len(), 1, "同 from 覆盖");
        assert_eq!(config.mappings[0].to, "claude-opus-5");
        assert_eq!(config.mapping_for("gpt-x"), Some("claude-opus-5"));
        assert_eq!(config.mapping_for("other"), None);

        mapping_delete(&path, "gpt-x").unwrap();
        assert!(matches!(
            mapping_delete(&path, "gpt-x"),
            Err(Error::GatewayConfig(_))
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generated_token_matches_prefix_shape() {
        let token = generate_token();
        assert!(token.starts_with(GATEWAY_TOKEN_PREFIX));
        let random = &token[GATEWAY_TOKEN_PREFIX.len()..];
        assert_eq!(random.len(), 32);
        assert!(random.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(generate_token(), generate_token(), "连续签发不得相同");
    }

    #[test]
    fn append_token_hash_is_idempotent_and_keeps_unknown_fields() {
        let dir = std::env::temp_dir().join(format!("toktol-gw-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gateway.json");
        std::fs::write(&path, valid_config_text()).unwrap();

        let token = generate_token();
        append_token_hash(&path, &token).unwrap();
        append_token_hash(&path, &token).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let config = parse(&text).unwrap();
        assert_eq!(
            config.token_hashes.len(),
            2,
            "同一令牌重复签发只落一个哈希（数组里原有 1 个）"
        );
        let expected: String = {
            use sha2::{Digest, Sha256};
            Sha256::digest(token.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        };
        assert!(text.contains(&expected));
        // 未知字段与既有配置保留。
        assert!(text.contains("\"default_upstream\""));
        assert_eq!(config.upstreams.len(), 1);

        // 改坏后追加：校验拦下，文件保持原样。
        std::fs::write(&path, "{\"broken\": true").unwrap();
        assert!(append_token_hash(&path, &token).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"broken\": true");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_defaults_and_routes() {
        let config = parse(&valid_config_text()).unwrap();
        assert_eq!(config.listen.to_string(), DEFAULT_LISTEN);
        assert_eq!(config.upstreams.len(), 1);
        assert_eq!(config.upstreams[0].protocol, Protocol::Anthropic);
        assert_eq!(config.upstreams[0].base_url, "https://api.anthropic.com");
        assert_eq!(config.token_hashes.len(), 1);

        assert_eq!(config.route_for("claude-sonnet-5"), Some(0));
        // glob 语义：`claude-*` 匹配带空通配段的 `claude-`，但不匹配缺前缀的名字。
        assert_eq!(config.route_for("claude-"), Some(0));
        let mut without_default = config.clone();
        without_default.default_upstream = None;
        assert_eq!(
            without_default.route_for("gpt-x"),
            None,
            "无命中且未配 default"
        );
        assert_eq!(
            config.route_for("gpt-x"),
            Some(0),
            "无命中回落 default_upstream"
        );
    }

    #[test]
    fn non_loopback_listen_is_rejected() {
        let text = valid_config_text().replacen('{', "{\"listen\": \"0.0.0.0:8412\",", 1);
        assert!(matches!(parse(&text), Err(Error::GatewayConfig(_))));
    }

    #[test]
    fn token_hash_must_be_64_hex() {
        let text = valid_config_text().replace(&"ab".repeat(32), "zz");
        assert!(matches!(parse(&text), Err(Error::GatewayConfig(_))));
    }

    #[test]
    fn models_list_prefers_explicit_and_falls_back_to_exact_patterns() {
        let config = parse(&valid_config_text()).unwrap();
        assert!(
            config.models.is_empty(),
            "通配路由模式不是合法模型 id，不进宣称列表"
        );

        let exact = valid_config_text()
            .replace("\"model\": \"claude-*\"", "\"model\": \"claude-sonnet-5\"");
        let config = parse(&exact).unwrap();
        assert_eq!(
            config.models,
            vec!["claude-sonnet-5"],
            "精确模式回落进宣称列表"
        );

        let explicit = valid_config_text().replacen('{', "{\"models\": [\"gpt-x\"],", 1);
        let config = parse(&explicit).unwrap();
        assert_eq!(config.models, vec!["gpt-x"], "显式 models 优先于路由推导");
    }

    #[test]
    fn route_to_unknown_upstream_is_rejected() {
        let text =
            valid_config_text().replace("\"upstream\": \"anthropic\"", "\"upstream\": \"nope\"");
        assert!(matches!(parse(&text), Err(Error::GatewayConfig(_))));
    }

    #[test]
    fn reload_picks_up_change_and_survives_bad_file() {
        let dir = std::env::temp_dir().join(format!("toktol-gw-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gateway.json");
        std::fs::write(&path, valid_config_text()).unwrap();

        let handle = ConfigHandle::load(&path).unwrap();
        assert_eq!(handle.current().upstreams.len(), 1);

        // 加一个上游；mtime 需要可分辨，Windows 时间戳精度粗，写入前稍等。
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, config_with_second_upstream()).unwrap();
        assert!(handle.reload_if_changed().unwrap());
        assert_eq!(handle.current().upstreams.len(), 2);
        // 未变化时不动。
        assert!(!handle.reload_if_changed().unwrap());

        // 改坏：保留上一份好快照，报错不切换。
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, "{\"listen\": \"0.0.0.0:1\"}").unwrap();
        assert!(handle.reload_if_changed().is_err());
        assert_eq!(handle.current().upstreams.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
