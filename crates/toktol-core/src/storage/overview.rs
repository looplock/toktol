//! 总览与仪表盘聚合：分项总览、仪表盘九卡的 SQL 聚合与本地时间分桶。
//! 全部查询共用 [`dashboard_cte`] 骨架，口径与行级明细严格一致。

use std::collections::HashMap;

use chrono::{Datelike, Local, LocalResult, NaiveDate, TimeZone, Timelike};
use rusqlite::params_from_iter;
use serde::Serialize;

use super::usage::usage_where;

use super::*;

impl Storage {
    /// 总览聚合：分项与总分都走视图 `v_usage_cost`，与行级口径严格一致。已知成本合计不含未知行；未知行数单独给出，
    /// 由前端标注"部分未知"，绝不混进总数冒充精确值。
    /// `disabled` 是被禁用工具的 id 列表（前端偏好）：聚合只统计启用的工具；
    /// 禁用不删数据，重新启用后数字原样回来。
    pub fn overview(&self, disabled: &[String]) -> Result<OverviewPayload> {
        // 每个子查询共用同一组编号占位符（?1..?n），整条语句只需绑定一遍清单。
        let clause = tool_not_in_clause(disabled);
        let totals = self.conn.query_row(
            &format!(
                "SELECT
                (SELECT COALESCE(SUM(input_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(output_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(cache_read_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(cache_write_tokens), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(reasoning_tokens), 0) FROM usage_records{clause}),
                (SELECT COUNT(*) FROM sessions{clause}),
                (SELECT COALESCE(SUM(request_count), 0) FROM usage_records{clause}),
                (SELECT COALESCE(SUM(cost_micros), 0) FROM v_usage_cost{clause}),
                (SELECT COUNT(*) FROM (SELECT tool, cost_micros FROM v_usage_cost{clause}) WHERE cost_micros IS NULL)"
            ),
            params_from_iter(disabled),
            |row| {
                Ok(UsageTotals {
                    input_tokens: row.get(0)?,
                    output_tokens: row.get(1)?,
                    cache_read_tokens: row.get(2)?,
                    cache_write_tokens: row.get(3)?,
                    reasoning_tokens: row.get(4)?,
                    session_count: row.get(5)?,
                    record_count: row.get(6)?,
                    known_cost_micros: row.get(7)?,
                    unknown_cost_rows: row.get(8)?,
                })
            },
        )?;

        let mut stmt = self.conn.prepare(&format!(
            "SELECT model,
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    COALESCE(SUM(reasoning_tokens), 0),
                    COALESCE(SUM(cost_micros), 0),
                    COUNT(*) FILTER (WHERE cost_micros IS NULL)
             FROM v_usage_cost{clause}
             GROUP BY model
             ORDER BY 7 DESC, 1 ASC"
        ))?;
        let by_model = stmt
            .query_map(params_from_iter(disabled), |row| {
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

        Ok(OverviewPayload { totals, by_model })
    }

    /// 仪表盘聚合：一次给出总览页全部卡片的切片。筛选与明细页同语义
    /// （[`UsageFilters`]，projects 的 `""` = 无项目），`disabled` 是被禁用
    /// 工具的 id 列表（前端偏好），禁用的工具不参与聚合。
    ///
    /// 口径不变式：瀑布分项之和 = 已知总成本。input/cache_read/cache_write
    /// 直接合计物化分桶；推理费 = Σ 推理量 × 输出价（与重算的"输出桶计价量
    /// 含推理"一致），从输出桶毛费里拆出，净输出 = 毛费 − 推理费——分项之和
    /// 仍落回四个物化分桶，即视图的已知总分。
    pub fn dashboard(
        &self,
        filters: &UsageFilters,
        disabled: &[String],
    ) -> Result<DashboardPayload> {
        let (where_sql, args) = usage_where(filters, disabled);
        let cte = dashboard_cte(&where_sql);
        let bind = || params_from_iter(args.iter());

        // 库里没有数据时全部切片归零：从 1970 起铺桶序列没有意义。
        let (min_ts, max_ts): (i64, i64) = self.conn.query_row(
            &format!("{cte} SELECT COALESCE(MIN(ts), 0), COALESCE(MAX(ts), 0) FROM filtered"),
            bind(),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if max_ts <= 0 {
            return Ok(empty_dashboard(TrendGrain::Day));
        }

        // 窗口端点：显式筛选优先；"不限时间"回落数据的 MIN/MAX（上界再与现在取大，
        // 兜住时钟误差导致的时间戳落到未来）。
        let start = filters.time_start.unwrap_or(min_ts);
        let end = filters.time_end.unwrap_or(max_ts.max(now_ms()));
        let grain = trend_grain(end.saturating_sub(start));
        // 脏时间戳（如落在 1970 的记录）会把"不限时间"撑成上千桶：趋势放弃，
        // 其余切片照常（它们按维度聚合，规模有界）。
        let starts = bucket_starts(start, end, grain);
        let starts = if starts.len() > 400 {
            Vec::new()
        } else {
            starts
        };
        let bucket_index: HashMap<i64, usize> = starts
            .iter()
            .copied()
            .enumerate()
            .map(|(i, ms)| (ms, i))
            .collect();

        // ── 使用时间（活跃段并集）────────────────────────────
        // 相邻间隔 ≤ 30 分钟的记录归为同一活跃段；段裁剪到筛选窗口后取
        // 并集。旧口径是"窗口内出现的会话按首末跨度求和"：长寿会话窗口
        // 外的时长、挂机空闲、并行会话的重叠全都重复计入，7 天窗口能算
        // 出 18 天；新口径恒不超过窗口本身。
        const ACTIVE_GAP_MS: i64 = 30 * 60 * 1000;
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT session_id, ts FROM filtered
                     WHERE session_id IS NOT NULL ORDER BY session_id, ts"
        ))?;
        let mut intervals: Vec<(i64, i64)> = Vec::new();
        let mut rows = stmt.query(bind())?;
        let mut cur_sid: Option<i64> = None;
        let mut seg = (0_i64, 0_i64);
        while let Some(row) = rows.next()? {
            let sid: i64 = row.get(0)?;
            let ts: i64 = row.get(1)?;
            if cur_sid == Some(sid) && ts - seg.1 <= ACTIVE_GAP_MS {
                seg.1 = ts;
            } else {
                if cur_sid.is_some() {
                    intervals.push(seg);
                }
                cur_sid = Some(sid);
                seg = (ts, ts);
            }
        }
        if cur_sid.is_some() {
            intervals.push(seg);
        }
        let active_duration_ms = union_duration_ms(intervals, start, end);

        // ── 总量 ─────────────────────────────────────────────
        let totals = self.conn.query_row(
            &format!(
                "{cte} SELECT
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*),
                    COUNT(DISTINCT session_id),
                    COUNT(DISTINCT CASE WHEN project_dir <> '' THEN project_dir END),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COUNT(DISTINCT tool)
                 FROM filtered"
            ),
            bind(),
            |row| {
                Ok(DashboardTotals {
                    cost_micros: row.get(0)?,
                    tokens: row.get(1)?,
                    calls: row.get(2)?,
                    sessions: row.get(3)?,
                    projects: row.get(4)?,
                    cache_read_tokens: row.get(5)?,
                    active_duration_ms,
                    active_tools: row.get(6)?,
                })
            },
        )?;

        // ── 趋势（按粒度分桶，缺失的桶补 0）───────────────────
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT {bucket} AS bucket_ms,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_read_cost_micros), 0)
                 FROM filtered
                 GROUP BY bucket_ms",
            bucket = grain.bucket_expr(),
        ))?;
        let trend_raw: HashMap<i64, [i64; 5]> = stmt
            .query_map(bind(), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    [
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ],
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        let trend: Vec<DashboardTrendBucket> = starts
            .iter()
            .copied()
            .map(|ms| {
                let cell = trend_raw.get(&ms).copied().unwrap_or_default();
                DashboardTrendBucket {
                    start_ms: ms,
                    cost_micros: cell[0],
                    tokens: cell[1],
                    calls: cell[2],
                    cache_read_tokens: cell[3],
                    cache_read_cost_micros: cell[4],
                }
            })
            .collect();

        // ── 按模型趋势（数组与 trend 的桶一一对齐）────────────
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT {bucket} AS bucket_ms, model,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY bucket_ms, model",
            bucket = grain.bucket_expr(),
        ))?;
        let model_trend_raw: HashMap<(i64, String), (i64, i64, i64)> = stmt
            .query_map(bind(), |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, String>(1)?),
                    (row.get(2)?, row.get(3)?, row.get(4)?),
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;

        // ── 构成（模型维度）；模型序是 modelTrend / spread 的权威顺序 ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT model,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*),
                    COUNT(*) FILTER (WHERE cost_micros IS NULL)
                 FROM filtered
                 GROUP BY model
                 ORDER BY 2 DESC, model ASC"
        ))?;
        let composition: Vec<DashboardBreakdownRow> = stmt
            .query_map(bind(), |row| {
                Ok(DashboardBreakdownRow {
                    label: row.get(0)?,
                    cost_micros: row.get(1)?,
                    tokens: row.get(2)?,
                    calls: row.get(3)?,
                    unknown_cost: row.get::<_, i64>(4)? > 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let model_index: HashMap<&str, usize> = composition
            .iter()
            .enumerate()
            .map(|(i, row)| (row.label.as_str(), i))
            .collect();
        let mut model_trend: Vec<DashboardModelSeries> = composition
            .iter()
            .map(|row| DashboardModelSeries {
                model: row.label.clone(),
                cost_micros: vec![0; starts.len()],
                tokens: vec![0; starts.len()],
                calls: vec![0; starts.len()],
            })
            .collect();
        for ((ms, model), (cost, tokens, calls)) in model_trend_raw {
            if let (Some(&bucket), Some(&series)) =
                (bucket_index.get(&ms), model_index.get(model.as_str()))
            {
                let series = &mut model_trend[series];
                series.cost_micros[bucket] = cost;
                series.tokens[bucket] = tokens;
                series.calls[bucket] = calls;
            }
        }

        // ── 构成（项目维度）；unknown 是模型维度的概念，恒 false ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT project_dir,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY project_dir
                 ORDER BY 2 DESC, project_dir ASC"
        ))?;
        let projects: Vec<DashboardBreakdownRow> = stmt
            .query_map(bind(), |row| {
                Ok(DashboardBreakdownRow {
                    label: row.get(0)?,
                    cost_micros: row.get(1)?,
                    tokens: row.get(2)?,
                    calls: row.get(3)?,
                    unknown_cost: false,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // ── 流向：工具 → 模型、模型 → 项目两段 ────────────────
        let mut flow: Vec<DashboardFlowLink> = Vec::new();
        for sql in [
            format!(
                "{cte} SELECT tool, model, COALESCE(SUM(cost_micros), 0), COALESCE(SUM({TOKENS_SUM_EXPR}), 0), COUNT(*) FROM filtered GROUP BY tool, model"
            ),
            format!(
                "{cte} SELECT model, project_dir, COALESCE(SUM(cost_micros), 0), COALESCE(SUM({TOKENS_SUM_EXPR}), 0), COUNT(*) FROM filtered GROUP BY model, project_dir"
            ),
        ] {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(bind(), |row| {
                    Ok(DashboardFlowLink {
                        source: row.get(0)?,
                        target: row.get(1)?,
                        cost_micros: row.get(2)?,
                        tokens: row.get(3)?,
                        calls: row.get(4)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            flow.extend(rows);
        }
        flow.sort_by(|a, b| {
            b.cost_micros
                .cmp(&a.cost_micros)
                .then_with(|| a.source.cmp(&b.source))
                .then_with(|| a.target.cmp(&b.target))
        });

        // ── 活跃热力图：本地星期（0=周一）× 小时，恒 7×24 格 ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT (CAST(strftime('%w', ts/1000, 'unixepoch', 'localtime') AS INTEGER) + 6) % 7 AS day,
                    CAST(strftime('%H', ts/1000, 'unixepoch', 'localtime') AS INTEGER) AS hour,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY day, hour"
        ))?;
        let activity_raw: HashMap<(i64, i64), (i64, i64, i64)> = stmt
            .query_map(bind(), |row| {
                Ok((
                    (row.get::<_, i64>(0)?, row.get::<_, i64>(1)?),
                    (row.get(2)?, row.get(3)?, row.get(4)?),
                ))
            })?
            .collect::<std::result::Result<HashMap<_, _>, _>>()?;
        let mut activity: Vec<DashboardActivityCell> = Vec::with_capacity(7 * 24);
        for day in 0..7 {
            for hour in 0..24 {
                let cell = activity_raw.get(&(day, hour)).copied().unwrap_or((0, 0, 0));
                activity.push(DashboardActivityCell {
                    day,
                    hour,
                    cost_micros: cell.0,
                    tokens: cell.1,
                    calls: cell.2,
                });
            }
        }

        // ── 日历：不吃时间筛选（窗口是卡片自己的设置），其余筛选照常 ──
        let mut daily_filters = filters.clone();
        daily_filters.time_start = None;
        daily_filters.time_end = None;
        let (daily_where, daily_args) = usage_where(&daily_filters, disabled);
        let daily_cte = dashboard_cte(&daily_where);
        let mut stmt = self.conn.prepare(&format!(
            "{daily_cte} SELECT strftime('%Y-%m-%d', ts/1000, 'unixepoch', 'localtime') AS d,
                    COALESCE(SUM(cost_micros), 0),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0),
                    COUNT(*)
                 FROM filtered
                 GROUP BY d
                 ORDER BY d ASC"
        ))?;
        let daily: Vec<DashboardDailyPoint> = stmt
            .query_map(params_from_iter(daily_args.iter()), |row| {
                Ok(DashboardDailyPoint {
                    date: row.get(0)?,
                    cost_micros: row.get(1)?,
                    tokens: row.get(2)?,
                    calls: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // ── 箱线图：只看有价行，每行一次调用的开销与 token，Rust 侧排序取分位 ──
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT model, cost_micros, {TOKENS_SUM_EXPR}
                 FROM filtered
                 WHERE cost_micros IS NOT NULL"
        ))?;
        let spread_rows = stmt
            .query_map(bind(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut spread_raw: HashMap<String, (Vec<i64>, Vec<i64>)> = HashMap::new();
        for (model, cost, tokens) in spread_rows {
            let entry = spread_raw.entry(model).or_default();
            entry.0.push(cost);
            entry.1.push(tokens);
        }
        let spread: Vec<DashboardSpread> = composition
            .iter()
            .filter_map(|row| {
                let (mut costs, mut tokens) = spread_raw.remove(&row.label)?;
                costs.sort_unstable();
                tokens.sort_unstable();
                let five = |sorted: &[i64]| -> Vec<i64> {
                    [0.0, 0.25, 0.5, 0.75, 1.0]
                        .iter()
                        .map(|&q| quantile(sorted, q))
                        .collect()
                };
                Some(DashboardSpread {
                    label: row.label.clone(),
                    cost_micros: five(&costs),
                    tokens: five(&tokens),
                })
            })
            .collect();

        // ── 会话消耗气泡：一会话（× 模型）一点。全部行未定价的会话没有费用
        // 可画（SUM 全 NULL），HAVING 排除；部分未定价按已知部分计入。
        // 分组必须带 tool：external_id 只在工具内唯一，(tool, external_id) 才是
        // 会话自然键。首末时间给前端 tooltip 定位"这是哪次工作"。
        let mut stmt = self.conn.prepare(&format!(
            "{cte} SELECT tool, external_id, project_dir, model, COUNT(*),
                    COALESCE(SUM({TOKENS_SUM_EXPR}), 0), SUM(cost_micros), MIN(ts), MAX(ts)
             FROM filtered
             WHERE external_id IS NOT NULL
             GROUP BY tool, external_id, project_dir, model
             HAVING SUM(cost_micros) IS NOT NULL
             ORDER BY 7 DESC"
        ))?;
        let sessions: Vec<DashboardSessionPoint> = stmt
            .query_map(bind(), |row| {
                Ok(DashboardSessionPoint {
                    tool: row.get(0)?,
                    external_id: row.get(1)?,
                    project_dir: row.get(2)?,
                    model: row.get(3)?,
                    calls: row.get(4)?,
                    tokens: row.get(5)?,
                    cost_micros: row.get(6)?,
                    start_ms: row.get(7)?,
                    end_ms: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // ── 瀑布：四个物化分桶 + 推理费（输出价 × 推理量）拆分 ──
        let (input_cost, output_gross, reasoning_cost, cache_read_cost, cache_write_cost): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = self.conn.query_row(
            &format!(
                "{cte} SELECT
                    COALESCE(SUM(input_cost_micros), 0),
                    COALESCE(SUM(output_cost_micros), 0),
                    COALESCE(SUM(COALESCE(reasoning_tokens, 0) * COALESCE(output_per_mtok_micros, 0) / {MICROS_PER_MTK}), 0),
                    COALESCE(SUM(cache_read_cost_micros), 0),
                    COALESCE(SUM(cache_write_cost_micros), 0)
                 FROM filtered"
            ),
            bind(),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        // 整除截断都向下，毛费 ≥ 推理费逐行成立；max(0) 只是形式兜底。
        let cost_composition = DashboardCostComposition {
            input_cost_micros: input_cost,
            output_cost_micros: (output_gross - reasoning_cost).max(0),
            reasoning_cost_micros: reasoning_cost,
            cache_read_cost_micros: cache_read_cost,
            cache_write_cost_micros: cache_write_cost,
        };

        let unknown_model_count = composition.iter().filter(|row| row.unknown_cost).count() as i64;

        Ok(DashboardPayload {
            totals,
            trend,
            trend_grain: grain,
            model_trend,
            composition,
            projects,
            flow,
            activity,
            daily,
            spread,
            cost_composition,
            unknown_model_count,
            sessions,
        })
    }
}

// ── 仪表盘聚合的 SQL 骨架与桶序列 ─────────────────────────────

/// 五桶 token 合计表达式（缓存读是子集，不做减法）。
const TOKENS_SUM_EXPR: &str = "(input_tokens + output_tokens + cache_read_tokens + cache_write_tokens + COALESCE(reasoning_tokens, 0))";

/// 仪表盘全部查询共用的基础 CTE：事实 + 成本视图（已知总分口径）+ 会话项目归属。
/// 项目空值就地归一为 `""`（无项目哨兵）；external_id 供会话消耗切片分组；
/// LEFT JOIN model_prices 只为推理费取输出单价，主键关联不放大行数。
/// `usage_where` 的子句引用 `r.` / `s.` 别名。
fn dashboard_cte(where_sql: &str) -> String {
    format!(
        "WITH filtered AS (
             SELECT r.*, v.cost_micros, p.output_per_mtok_micros, s.external_id,
                    COALESCE(s.project_dir, '') AS project_dir
             FROM usage_records AS r
             JOIN v_usage_cost AS v ON v.id = r.id
             LEFT JOIN sessions AS s ON s.id = r.session_id
             LEFT JOIN model_prices AS p ON p.model_id = r.model{where_sql}
         )"
    )
}

/// 趋势分桶粒度：跨度 ≤2 天按小时，≤62 天按天，否则按月。与载荷的
/// `trend_grain` 一同交给前端，label 的粒度跟它走。序列化值（`hour` /
/// `day` / `month`）即线上契约，前端镜像见 api.ts 的 `TrendGrain`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TrendGrain {
    Hour,
    Day,
    Month,
}

impl TrendGrain {
    /// 把 ts（ms）对齐到本地时区桶起点的 SQL 表达式。'localtime' 先转到本地
    /// 墙钟、取粒度起点，再以 'utc' 转回 epoch——漏掉最后一步会把本地墙钟
    /// 当 UTC 解释，起点恰好偏移一个时区。注意 SQLite 没有 'start of hour'
    /// 修饰符（"start of" 只到 day），小时粒度改为格式化到整点字符串再转回。
    fn bucket_expr(self) -> &'static str {
        match self {
            TrendGrain::Hour => {
                "CAST(strftime('%s', strftime('%Y-%m-%d %H:00:00', ts/1000, 'unixepoch', 'localtime'), 'utc') AS INTEGER) * 1000"
            }
            TrendGrain::Day => {
                "CAST(strftime('%s', ts/1000, 'unixepoch', 'localtime', 'start of day', 'utc') AS INTEGER) * 1000"
            }
            TrendGrain::Month => {
                "CAST(strftime('%s', ts/1000, 'unixepoch', 'localtime', 'start of month', 'utc') AS INTEGER) * 1000"
            }
        }
    }
}

/// 跨度 → 粒度。
fn trend_grain(span_ms: i64) -> TrendGrain {
    const DAY_MS: i64 = 86_400_000;
    if span_ms <= 2 * DAY_MS {
        TrendGrain::Hour
    } else if span_ms <= 62 * DAY_MS {
        TrendGrain::Day
    } else {
        TrendGrain::Month
    }
}

/// 本地墙钟分量 → epoch ms。歧义（DST 回拨的一小时出现两次）取较早者；
/// 不存在的墙钟（DST 春跳把某个小时跳没了，floor 可能落进去）按 UTC 近似——
/// 桶起点仍唯一且单调，只是那一格的墙钟语义略偏。
fn local_ms(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
    match Local.with_ymd_and_hms(y, m, d, h, min, 0) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.timestamp_millis(),
        LocalResult::None => NaiveDate::from_ymd_opt(y, m, d)
            .and_then(|date| date.and_hms_opt(h, min, 0))
            .map_or(0, |naive| naive.and_utc().timestamp_millis()),
    }
}

/// epoch ms → 本地时区的粒度起点。
fn floor_local_ms(ms: i64, grain: TrendGrain) -> i64 {
    let Some(dt) = Local.timestamp_millis_opt(ms).single() else {
        return ms;
    };
    match grain {
        TrendGrain::Hour => local_ms(dt.year(), dt.month(), dt.day(), dt.hour(), 0),
        TrendGrain::Day => local_ms(dt.year(), dt.month(), dt.day(), 0, 0),
        TrendGrain::Month => local_ms(dt.year(), dt.month(), 1, 0, 0),
    }
}

/// 覆盖 [start, end] 的桶起点序列（本地对齐，严格递增）。窗口倒挂返回空。
/// DST 回拨可能把相邻两步 floor 到同一墙钟：去重而非重复。
fn bucket_starts(start_ms: i64, end_ms: i64, grain: TrendGrain) -> Vec<i64> {
    let last = floor_local_ms(end_ms, grain);
    let mut out: Vec<i64> = Vec::new();

    if grain == TrendGrain::Month {
        let Some(dt) = Local.timestamp_millis_opt(start_ms).single() else {
            return out;
        };
        let (mut y, mut m) = (dt.year(), dt.month());
        loop {
            let start = local_ms(y, m, 1, 0, 0);
            if start > last {
                break;
            }
            if out.last().is_none_or(|prev| *prev < start) {
                out.push(start);
            }
            if m == 12 {
                y += 1;
                m = 1;
            } else {
                m += 1;
            }
        }

        return out;
    }

    // Hour/Day 用 epoch 步进再 floor：floor 单调 + 去重，天然防回拨回环。
    let step: i64 = if grain == TrendGrain::Hour {
        3_600_000
    } else {
        86_400_000
    };
    let mut cursor = floor_local_ms(start_ms, grain);
    while cursor <= last {
        if out.last().is_none_or(|prev| *prev < cursor) {
            out.push(cursor);
        }
        cursor = floor_local_ms(cursor.saturating_add(step), grain);
    }

    out
}

/// 线性插值分位（R-7，Excel/NumPy 同法）；`sorted` 必须升序。
fn quantile(sorted: &[i64], q: f64) -> i64 {
    match sorted.len() {
        0 => 0,
        1 => sorted[0],
        n => {
            let pos = q * (n - 1) as f64;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(n - 1);
            let frac = pos - lo as f64;
            (sorted[lo] as f64 + frac * (sorted[hi] - sorted[lo]) as f64).round() as i64
        }
    }
}

/// 区间并集的覆盖总时长：先裁剪到 [lo, hi]，排序合并重叠后累加长度。
/// 并行会话的活跃段借此互不重复计时；倒挂窗口与空输入得 0。
pub(crate) fn union_duration_ms(mut intervals: Vec<(i64, i64)>, lo: i64, hi: i64) -> i64 {
    intervals.sort_unstable();
    let mut total = 0_i64;
    let mut merged: Option<(i64, i64)> = None;
    for (s, e) in intervals {
        let (s, e) = (s.max(lo), e.min(hi));
        if s >= e {
            continue;
        }
        match merged {
            Some((ms, me)) if s <= me => merged = Some((ms, me.max(e))),
            _ => {
                if let Some((ms, me)) = merged.replace((s, e)) {
                    total += me - ms;
                }
            }
        }
    }
    if let Some((ms, me)) = merged {
        total += me - ms;
    }
    total
}

/// 空库 / 脏窗口时的零值载荷：activity 仍给全 168 格（热力图按网格画）。
fn empty_dashboard(grain: TrendGrain) -> DashboardPayload {
    let mut activity = Vec::with_capacity(7 * 24);
    for day in 0..7 {
        for hour in 0..24 {
            activity.push(DashboardActivityCell {
                day,
                hour,
                cost_micros: 0,
                tokens: 0,
                calls: 0,
            });
        }
    }

    DashboardPayload {
        totals: DashboardTotals {
            cost_micros: 0,
            tokens: 0,
            calls: 0,
            sessions: 0,
            projects: 0,
            cache_read_tokens: 0,
            active_duration_ms: 0,
            active_tools: 0,
        },
        trend: Vec::new(),
        trend_grain: grain,
        model_trend: Vec::new(),
        composition: Vec::new(),
        projects: Vec::new(),
        flow: Vec::new(),
        activity,
        daily: Vec::new(),
        spread: Vec::new(),
        sessions: Vec::new(),
        cost_composition: DashboardCostComposition {
            input_cost_micros: 0,
            output_cost_micros: 0,
            reasoning_cost_micros: 0,
            cache_read_cost_micros: 0,
            cache_write_cost_micros: 0,
        },
        unknown_model_count: 0,
    }
}
