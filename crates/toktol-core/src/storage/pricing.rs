//! 模型目录与定价：映射维护、目录同步、定价页聚合与成本重算管线。
//! 事实表改写（repoint/reprice）只在本模块发生——计价口径集中在
//! [`reprice_scoped`] 一处，新增事实表时把表名加进 [`FACT_TABLES`] 即可。

use std::collections::HashMap;

use rusqlite::{
    Connection, OptionalExtension, Transaction, params, params_from_iter, types::Value,
};

use super::*;

use crate::pricing::catalog::CatalogEntry;

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

impl Storage {
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
    pub fn replace_catalog_entries(&self, entries: &[CatalogEntry], now: i64) -> Result<()> {
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
    /// 拉取与解析在 [`crate::pricing::catalog::sync_catalog`]，这里只管写入与聚合结果。
    pub fn apply_catalog(&self, entries: &[CatalogEntry]) -> Result<CatalogSyncPayload> {
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
        let best = self.catalog_best()?;
        let now = now_ms();
        let tx = self.conn.unchecked_transaction()?;
        // 受影响模型清单：改指的新目标 + 挂价/刷价的模型；末尾只重算这些。
        let mut repriced_models: Vec<String> = Vec::new();

        let remapped = self.backfill_catalog_mappings(&tx, &best, now, &mut repriced_models)?;
        let (matched, priced) = self.apply_catalog_prices(&tx, &best, &mut repriced_models)?;
        // 挂价/刷价/改指可能改变成本：只重算受影响模型，页面无需再触发。
        reprice_scoped(&tx, Some(&repriced_models))?;
        tx.commit()?;
        Ok((matched, priced, remapped))
    }

    /// 目录收敛：每个 model_id 只留一条（在售优先、供应商字典序，见
    /// [`Self::sync_catalog_prices`] 的选取规则）。
    fn catalog_best(&self) -> Result<HashMap<String, CatalogEntry>> {
        let mut best: HashMap<String, CatalogEntry> = HashMap::new();
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
            Ok(CatalogEntry {
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
        Ok(best)
    }

    /// 反向补映射：auto/suggested 映射指到的标准模型在目录里没有、但变种名重新
    /// 解析（归一 → 显示名 → 启发候选）能在目录命中时，改指过去并改写事实表；
    /// user 映射永不参与。改指的新目标追加进 `repriced`。
    /// 返回补映射改指数。
    fn backfill_catalog_mappings(
        &self,
        tx: &Transaction,
        best: &HashMap<String, CatalogEntry>,
        now: i64,
        repriced: &mut Vec<String>,
    ) -> Result<usize> {
        let mut remapped = 0usize;
        // 显示名索引用目录收敛结果建（display_name 归一 → 目录 model_id），
        // 与挂价同一条选取规则。
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
            } else if (model_id == raw && canonical != model_id) || best.contains_key(&canonical) {
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
                repoint_mapping_tx(tx, &raw, &new_id, source, now)?;
                repriced.push(new_id);
                remapped += 1;
            }
        }
        delete_orphan_models(tx)?;
        Ok(remapped)
    }

    /// 挂价/刷价：`catalog < user`，只补缺和刷新 `catalog` 来源的行；
    /// user 是用户改过的价，目录永不覆盖。deprecated 照常挂价（历史用量同样要算钱）。
    /// 返回 (匹配到的标准模型数, 实际写价/刷价次数)；写价模型追加进 `repriced`。
    fn apply_catalog_prices(
        &self,
        tx: &Transaction,
        best: &HashMap<String, CatalogEntry>,
        repriced: &mut Vec<String>,
    ) -> Result<(usize, usize)> {
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
                    repriced.push(model_id.clone());
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
                    repriced.push(model_id.clone());
                }
                // user：目录不覆盖。
                Some(_) => {}
            }
        }
        Ok((matched, priced))
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
    pub(crate) fn resolve_suggestion(&self, canonical: &str) -> (String, &'static str) {
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
pub(crate) fn reprice_scoped(conn: &Connection, models: Option<&[String]>) -> Result<usize> {
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
