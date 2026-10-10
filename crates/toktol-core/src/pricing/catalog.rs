//! models.dev 公共目录（`https://models.dev/api.json`）的解析：把开源社区维护的
//! 模型单价表换算成本库口径（微美元 / 每百万 token）。目录只提供**建议价**——
//! 挂价、覆盖与重算的入口都在存储层（`model_prices.source` 三级来源：
//! `seed` 内置 < `catalog` 目录 < `user` 用户，高层永远不覆盖低层）。
//! 红线不变：目录里没有价格的模型只进快照，绝不估价。

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::storage::{CatalogSyncPayload, Storage};

/// models.dev 公共目录的地址。
pub const MODEL_CATALOG_URL: &str = "https://models.dev/api.json";

/// 目录快照的一行，单价已换算为微美元 / 每百万 token；缺价的桶为 `None`。
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// models.dev 的供应商标识（`deepseek`、`zhipuai`…）。
    pub provider: String,
    /// 目录侧模型 id；与我们的标准模型名同源同风格（小写、`-` 连接）。
    pub model_id: String,
    /// 目录给的展示名（如 "DeepSeek V4 Flash"）。
    pub display_name: Option<String>,
    /// `active` / `deprecated`；目录没给就当在售。
    pub status: Option<String>,
    /// 输入桶单价。
    pub input_per_mtok_micros: Option<i64>,
    /// 输出桶单价（目录口径通常已含推理）。
    pub output_per_mtok_micros: Option<i64>,
    /// 缓存读单价。
    pub cache_read_per_mtok_micros: Option<i64>,
    /// 缓存写单价（多数供应商不单列）。
    pub cache_write_per_mtok_micros: Option<i64>,
    /// 推理桶单价（仅少数供应商单列；本库计价并入输出桶）。
    pub reasoning_per_mtok_micros: Option<i64>,
}

#[derive(Deserialize)]
struct RawDoc {
    #[serde(default)]
    models: HashMap<String, RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    cost: Option<RawCost>,
}

#[derive(Deserialize)]
struct RawCost {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(rename = "cache_read", default)]
    cache_read: Option<f64>,
    #[serde(rename = "cache_write", default)]
    cache_write: Option<f64>,
    #[serde(default)]
    reasoning: Option<f64>,
}

/// USD/百万 token → 微美元/百万 token。负数按缺价处理（目录脏数据不进库）。
fn to_micros(usd: f64) -> Option<i64> {
    (usd >= 0.0 && usd.is_finite()).then(|| (usd * 1_000_000.0).round() as i64)
}

/// 解析目录 JSON。顶层是 `{provider: {models: {model_id: …}}}`；解析失败整体报错，
/// 单个模型的 cost 缺失只影响该模型的挂价能力。
pub fn parse_catalog_json(text: &str) -> Result<Vec<CatalogEntry>> {
    let doc: HashMap<String, RawDoc> = serde_json::from_str(text)
        .map_err(|err| Error::Internal(format!("models.dev 目录解析失败: {err}")))?;

    let mut entries = Vec::new();
    for (provider, doc) in doc {
        for (model_id, model) in doc.models {
            let cost = model.cost.as_ref();
            let pick = |get: fn(&RawCost) -> Option<f64>| cost.and_then(get).and_then(to_micros);
            entries.push(CatalogEntry {
                provider: provider.clone(),
                model_id,
                display_name: model.name,
                status: model.status,
                input_per_mtok_micros: pick(|c| c.input),
                output_per_mtok_micros: pick(|c| c.output),
                cache_read_per_mtok_micros: pick(|c| c.cache_read),
                cache_write_per_mtok_micros: pick(|c| c.cache_write),
                reasoning_per_mtok_micros: pick(|c| c.reasoning),
            });
        }
    }
    Ok(entries)
}

/// 目录同步的整条链路：拉取 → 解析 → 整份换快照挂价重算。HTTP 编排住在 core
/// 而非壳命令——网络编排是领域流程的一部分，进了 core 才可测。
/// blocking HTTP：调用方（壳命令）必须放进 spawn_blocking，别在 async 上下文直调。
pub fn sync_catalog(storage: &Storage) -> Result<CatalogSyncPayload> {
    sync_catalog_from(storage, MODEL_CATALOG_URL)
}

/// [`sync_catalog`] 的可测形态：URL 可注入，测试用本地服务器走全链路。
fn sync_catalog_from(storage: &Storage, url: &str) -> Result<CatalogSyncPayload> {
    let text = fetch_catalog_text(url)?;
    let entries = parse_catalog_json(&text)?;
    storage.apply_catalog(&entries)
}

/// 拉取目录原文。断网、非 2xx、响应不可读统一折进 CatalogFetch——对用户
/// 是同一件事：目录没拿到。总超时 30s：同步是用户手动触发，宁可报错重试，
/// 也不能让调用方的阻塞线程无限挂着。
fn fetch_catalog_text(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|err| Error::CatalogFetch(err.to_string()))?;
    let response = client
        .get(url)
        .send()
        .map_err(|err| Error::CatalogFetch(err.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::CatalogFetch(format!("HTTP {}", status.as_u16())));
    }
    response
        .text()
        .map_err(|err| Error::CatalogFetch(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "deepseek": {"id": "deepseek", "name": "DeepSeek", "models": {
            "deepseek-v4-flash": {
                "name": "DeepSeek V4 Flash", "status": "deprecated",
                "cost": {"input": 0.15, "output": 0.6, "cache_read": 0.003}
            },
            "free-model": {"name": "Free", "cost": {"input": 0.0, "output": 0.0}},
            "no-cost": {"name": "No Cost"}
        }},
        "zhipuai": {"id": "zhipuai", "models": {
            "deepseek-v4-flash": {"name": "同 id 另一供应商", "cost": {"input": 0.2, "output": 0.8}}
        }}
    }"#;

    #[test]
    fn catalog_json_parses_to_micros_per_mtok() {
        let entries = parse_catalog_json(SAMPLE).unwrap();
        assert_eq!(entries.len(), 4);

        let flash = entries
            .iter()
            .find(|e| e.provider == "deepseek" && e.model_id == "deepseek-v4-flash")
            .unwrap();
        assert_eq!(flash.input_per_mtok_micros, Some(150_000));
        assert_eq!(flash.output_per_mtok_micros, Some(600_000));
        assert_eq!(flash.cache_read_per_mtok_micros, Some(3_000));
        assert_eq!(flash.cache_write_per_mtok_micros, None, "目录没给就是缺");
        assert_eq!(flash.status.as_deref(), Some("deprecated"));

        let free = entries.iter().find(|e| e.model_id == "free-model").unwrap();
        assert_eq!(free.input_per_mtok_micros, Some(0), "免费是 0，不是缺");

        let bare = entries.iter().find(|e| e.model_id == "no-cost").unwrap();
        assert_eq!(bare.input_per_mtok_micros, None, "无 cost 绝不估价");
    }

    #[test]
    fn broken_catalog_json_is_an_error_not_a_panic() {
        assert!(parse_catalog_json("{ not json").is_err());
    }

    // ── sync_catalog 全链路：std 自带的单连接测试服务器，不为测试引 dev-dep ──

    /// 每个测试独享一个临时库（同款做法见 storage::tests）。
    fn test_db(tag: &str) -> (Storage, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toktol-catalog-test-{}-{}-{tag}.db",
            std::process::id(),
            seq
        ));
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(path.with_extension(format!("db{suffix}")));
        }
        (crate::storage::open(&path).expect("打开测试库"), path)
    }

    fn cleanup(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(path.with_extension(format!("db{suffix}")));
        }
    }

    /// 监听一次请求、回一段预制响应后关店。测试线程读不满请求无所谓——GET 无体。
    fn serve_once(response: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("绑定测试端口");
        let addr = listener.local_addr().expect("读取测试端口");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let _ = std::io::Write::write_all(&mut stream, &response);
            }
        });
        format!("http://{addr}/")
    }

    #[test]
    fn sync_catalog_applies_a_2xx_body_end_to_end() {
        let (storage, path) = test_db("sync-ok");
        let body = r#"{"deepseek":{"models":{"deepseek-v4-flash":{"name":"DeepSeek V4 Flash","cost":{"input":0.15,"output":0.6}}}}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let url = serve_once(response.into_bytes());
        let payload = sync_catalog_from(&storage, &url).expect("2xx 目录应整体落库");
        assert_eq!(payload.entries, 1);
        assert_eq!(payload.matched, 0, "空库无模型可匹配，快照照常入库");
        assert_eq!(payload.priced, 0);
        assert_eq!(payload.remapped, 0);
        drop(storage);
        cleanup(&path);
    }

    #[test]
    fn sync_catalog_maps_non_2xx_to_catalog_fetch() {
        let (storage, path) = test_db("sync-500");
        let url = serve_once(
            b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                .to_vec(),
        );
        let err = sync_catalog_from(&storage, &url).expect_err("非 2xx 必须报错");
        assert_eq!(err.code(), crate::error::ErrorCode::CatalogFetch);
        drop(storage);
        cleanup(&path);
    }
}
