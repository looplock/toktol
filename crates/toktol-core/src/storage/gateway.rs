//! 网关侧只读查询：请求总览（与 [`super::Storage::overview`] 同构口径）与请求级分页。

use rusqlite::params;

use super::*;

impl Storage {
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
}
