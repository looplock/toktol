//! models.dev 公共目录（`https://models.dev/api.json`）的解析：把开源社区维护的
//! 模型单价表换算成本库口径（微美元 / 每百万 token）。目录只提供**建议价**——
//! 挂价、覆盖与重算的入口都在存储层（`model_prices.source` 三级来源：
//! `seed` 内置 < `catalog` 目录 < `user` 用户，高层永远不覆盖低层）。
//! 红线不变：目录里没有价格的模型只进快照，绝不估价。

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{Error, Result};

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
}
