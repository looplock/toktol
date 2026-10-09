//! 用量明细页查询：筛选子句构建、排序白名单、行级分页与筛选选项。
//! [`usage_where`] 同时被仪表盘复用——筛选口径只有这一份。

use rusqlite::{params_from_iter, types::Value};

use super::*;

impl Storage {
    /// 明细页请求级分页：筛选/排序/分页都在 SQL 里做（行数无上界，不能整表拉回前端）。
    /// 成本走 `v_usage_cost` 视图，与聚合口径严格一致；会话归属 LEFT JOIN——
    /// 删除会话后 `session_id` 为 NULL，用量行保留。
    pub fn usage_records_page(&self, query: &UsageRecordsQuery) -> Result<UsageRecordsPage> {
        let (where_sql, args) = usage_where(&query.filters, &query.disabled);
        let from = "FROM usage_records AS r
             JOIN v_usage_cost AS v ON v.id = r.id
             LEFT JOIN sessions AS s ON s.id = r.session_id";

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) {from}{where_sql}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        // NULL 排序：SQLite 默认 ASC 时 NULL 在最前、DESC 在最后——"未知成本 /
        // 无耗时不与数值混排"（未知不冒充 0）。r.id DESC 做稳定 tiebreaker。
        let order = usage_sort_expr(query.sort_key.as_deref());
        let dir = if query.sort_desc { "DESC" } else { "ASC" };

        let mut stmt = self.conn.prepare(&format!(
            "SELECT
                r.id, r.ts, r.tool, r.model,
                r.input_tokens, r.output_tokens, r.cache_read_tokens, r.cache_write_tokens,
                r.reasoning_tokens, r.duration_ms,
                v.cost_micros, v.input_cost_micros, v.output_cost_micros,
                v.cache_read_cost_micros, v.cache_write_cost_micros,
                s.external_id, s.project_dir
             {from}{where_sql}
             ORDER BY {order} {dir}, r.id DESC
             LIMIT ? OFFSET ?"
        ))?;
        let mut bindings = args.clone();
        bindings.push(Value::Integer(query.limit.max(0)));
        bindings.push(Value::Integer(query.offset.max(0)));
        let rows = stmt
            .query_map(params_from_iter(bindings.iter()), |row| {
                Ok(UsageRecordRow {
                    id: row.get(0)?,
                    ts: row.get(1)?,
                    tool: row.get(2)?,
                    model: row.get(3)?,
                    session_external_id: row.get(15)?,
                    project_dir: row.get(16)?,
                    input_tokens: row.get(4)?,
                    output_tokens: row.get(5)?,
                    cache_read_tokens: row.get(6)?,
                    cache_write_tokens: row.get(7)?,
                    reasoning_tokens: row.get(8)?,
                    duration_ms: row.get(9)?,
                    cost_micros: row.get(10)?,
                    input_cost_micros: row.get(11)?,
                    output_cost_micros: row.get(12)?,
                    cache_read_cost_micros: row.get(13)?,
                    cache_write_cost_micros: row.get(14)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(UsageRecordsPage { rows, total })
    }

    /// 明细页筛选下拉的选项与计数：分面口径——每个维度的计数用其余维度的
    /// 筛选条件（不含本维度，否则选中即把他项归零）加时间与搜索；按条数降序；
    /// 禁用工具的记录不进选项（与其它读路径一致）。
    pub fn usage_filter_options(
        &self,
        filters: &UsageFilters,
        disabled: &[String],
    ) -> Result<UsageFilterOptionsPayload> {
        // 各维度去掉自己的白名单：分面计数（faceted search）的标准口径。
        let mut tool_filters = filters.clone();
        tool_filters.tools.clear();
        let mut model_filters = filters.clone();
        model_filters.models.clear();
        let mut project_filters = filters.clone();
        project_filters.projects.clear();

        let (tool_where, tool_args) = usage_where(&tool_filters, disabled);
        let (model_where, model_args) = usage_where(&model_filters, disabled);
        let (project_where, project_args) = usage_where(&project_filters, disabled);

        let query_options = |sql: String, args: &[Value]| -> Result<Vec<UsageFilterOption>> {
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt
                .query_map(params_from_iter(args.iter()), |row| {
                    Ok(UsageFilterOption {
                        value: row.get(0)?,
                        count: row.get(1)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        };

        // 项目与搜索子句引用 s. 别名，三组查询统一带上 sessions JOIN。
        let from = "FROM usage_records AS r
             LEFT JOIN sessions AS s ON s.id = r.session_id";
        let tools = query_options(
            format!(
                "SELECT r.tool, COUNT(*) {from}{tool_where}
                 GROUP BY r.tool ORDER BY 2 DESC, 1 ASC"
            ),
            &tool_args,
        )?;
        let models = query_options(
            format!(
                "SELECT r.model, COUNT(*) {from}{model_where}
                 GROUP BY r.model ORDER BY 2 DESC, 1 ASC"
            ),
            &model_args,
        )?;
        let projects = query_options(
            format!(
                "SELECT s.project_dir, COUNT(*) {from}{project_where}
                 GROUP BY s.project_dir ORDER BY 2 DESC, 1 ASC"
            ),
            &project_args,
        )?;

        Ok(UsageFilterOptionsPayload {
            tools,
            models,
            projects,
        })
    }
}

/// 排序键 → SQL 表达式的白名单。白名单外的键（含 `None`）一律退回时间排序——
/// 排序只是视图偏好，不值得为它开错误通道。
fn usage_sort_expr(key: Option<&str>) -> &'static str {
    match key {
        Some("tool") => "r.tool",
        Some("model") => "r.model",
        Some("project") => "s.project_dir",
        Some("session") => "s.external_id",
        // 四桶合计（量级），与前端旧口径一致。
        Some("tokens") => {
            "(r.input_tokens + r.output_tokens + r.cache_read_tokens + r.cache_write_tokens)"
        }
        Some("duration") => "r.duration_ms",
        Some("cost") => "v.cost_micros",
        _ => "r.ts",
    }
}

/// 明细查询的 WHERE 子句与绑定参数。占位符全用匿名 `?`（每处绑定只用一次，
/// 不像 overview 那样需要跨子查询复用编号）。
pub(crate) fn usage_where(filters: &UsageFilters, disabled: &[String]) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();

    if !disabled.is_empty() {
        clauses.push(format!(
            "r.tool NOT IN ({})",
            question_marks(disabled.len())
        ));
        args.extend(disabled.iter().map(|t| Value::Text(t.clone())));
    }
    if let Some(start) = filters.time_start {
        clauses.push("r.ts >= ?".into());
        args.push(Value::Integer(start));
    }
    if let Some(end) = filters.time_end {
        clauses.push("r.ts <= ?".into());
        args.push(Value::Integer(end));
    }
    if !filters.tools.is_empty() {
        clauses.push(format!(
            "r.tool IN ({})",
            question_marks(filters.tools.len())
        ));
        args.extend(filters.tools.iter().map(|t| Value::Text(t.clone())));
    }
    if !filters.models.is_empty() {
        clauses.push(format!(
            "r.model IN ({})",
            question_marks(filters.models.len())
        ));
        args.extend(filters.models.iter().map(|m| Value::Text(m.clone())));
    }
    // 项目筛选："" 是"无项目"哨兵 → project_dir IS NULL，其余按目录精确匹配。
    let wants_none = filters.projects.iter().any(String::is_empty);
    let named: Vec<&String> = filters.projects.iter().filter(|p| !p.is_empty()).collect();
    if wants_none || !named.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if wants_none {
            parts.push("s.project_dir IS NULL".into());
        }
        if !named.is_empty() {
            parts.push(format!(
                "s.project_dir IN ({})",
                question_marks(named.len())
            ));
            args.extend(named.iter().map(|p| Value::Text((*p).clone())));
        }
        clauses.push(format!("({})", parts.join(" OR ")));
    }
    if let Some(term) = filters
        .search
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let pattern = like_pattern(term);
        clauses
            .push("(s.external_id LIKE ? ESCAPE '\\' OR s.project_dir LIKE ? ESCAPE '\\')".into());
        args.push(Value::Text(pattern.clone()));
        args.push(Value::Text(pattern));
    }

    let sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (sql, args)
}
