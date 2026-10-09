//! 事实表写入原语：扫描用量行与网关请求行的单条入库（含去重键拦截与
//! 模型映射登记）。计价不在这里发生——唯一入口是重算管线。

use rusqlite::{OptionalExtension, params};

use super::pricing::reprice_scoped;
use super::*;

impl Storage {
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
}
