//! 会话簿记与列表：会话登记/删除、活动时间戳与会话页聚合查询。

use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};

use super::*;

impl Storage {
    /// 会话删除编排所需的会话信息；不存在则 `None`。
    pub fn session_info(&self, session_id: i64) -> Result<Option<SessionInfo>> {
        self.conn
            .query_row(
                "SELECT id, tool, external_id, source_file_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(SessionInfo {
                        id: row.get(0)?,
                        tool: row.get(1)?,
                        external_id: row.get(2)?,
                        source_file_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 按 (tool, external_id) 定位会话（会话详情视图只拿到这对自然键）；
    /// 不存在则 `None`。
    pub fn session_info_by_external(
        &self,
        tool: &str,
        external_id: &str,
    ) -> Result<Option<SessionInfo>> {
        self.conn
            .query_row(
                "SELECT id, tool, external_id, source_file_id FROM sessions
                 WHERE tool = ?1 AND external_id = ?2",
                params![tool, external_id],
                |row| {
                    Ok(SessionInfo {
                        id: row.get(0)?,
                        tool: row.get(1)?,
                        external_id: row.get(2)?,
                        source_file_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 簿记里登记的文件路径；行不存在则 `None`。
    pub fn scanned_file_path(&self, file_id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT path FROM scanned_files WHERE id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 引用某源文件的会话数：删除编排靠它判断文件是否可以进回收站。
    pub fn count_sessions_for_file(&self, file_id: i64) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE source_file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// 取回会话；不存在则创建。返回 (会话 id, 是否本次新建)。
    /// 项目目录**缺补不覆写**：老库里的会话可能建于适配器尚无项目事实的时期
    /// （如 workbuddy/zcode），重扫同一行时把 NULL 补上，已有的不漂移——
    /// 同一会话不会换项目，首见与补见只会其一。
    pub fn get_or_create_session(&self, session: &NewSession, now: i64) -> Result<(i64, bool)> {
        let tx = self.conn.unchecked_transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO sessions
                 (tool, external_id, external_id_is_derived, title, project_dir,
                  source_file_id, first_seen_at, last_activity_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                session.tool,
                session.external_id,
                session.external_id_is_derived,
                session.title,
                session.project_dir,
                session.source_file_id,
                now
            ],
        )?;
        let id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE tool = ?1 AND external_id = ?2",
            params![session.tool, session.external_id],
            |row| row.get(0),
        )?;
        if inserted == 0
            && let Some(dir) = &session.project_dir
        {
            tx.execute(
                "UPDATE sessions SET project_dir = ?2 WHERE id = ?1 AND project_dir IS NULL",
                params![id, dir],
            )?;
        }
        tx.commit()?;
        Ok((id, inserted > 0))
    }

    /// 更新会话活跃时间（取更晚者）与标题；会话不存在时静默无事——扫描层总是先建会话。
    /// 标题**首见为准**：适配器给的首条标题是稳定的（如首条用户消息），后续重扫
    /// 不会把标题漂移成更晚的消息。
    pub fn update_session_activity(
        &self,
        session_id: i64,
        ts: i64,
        title: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions
             SET last_activity_at = MAX(last_activity_at, ?2),
                 title = COALESCE(title, ?3)
             WHERE id = ?1",
            params![session_id, ts, title],
        )?;
        Ok(())
    }

    /// 新建会话的初始活动时间：直接落定为事实时间戳。backfill 场景（db 源
    /// 首次入账被工具回收过日志的老会话）里，建行的 `now` 会把老会话在列表
    /// 里顶成"今天"；后续更新仍走 [`Self::update_session_activity`] 的 MAX
    /// 语义，只增不减。
    pub fn init_session_activity(&self, session_id: i64, ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET last_activity_at = ?2 WHERE id = ?1",
            params![session_id, ts],
        )?;
        Ok(())
    }

    /// 删除会话（用户显式触发）。日志文件进回收站由扫描层负责，本函数只管库。
    ///
    /// 产品需求：**用量必须保留**——`usage_records.session_id` 置 NULL、`sessions` 行
    /// 删除；`scanned_files` 行仅在没有其它会话引用时删。墓碑由会话删除编排层
    /// 落（[`crate::sessions`]），本函数不重复记。
    pub fn delete_session(&self, session_id: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;

        let source_file_id: Option<i64> = tx
            .query_row(
                "SELECT source_file_id FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;

        tx.execute(
            "UPDATE usage_records SET session_id = NULL WHERE session_id = ?1",
            params![session_id],
        )?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;

        if let Some(file_id) = source_file_id {
            let refs: i64 = tx.query_row(
                "SELECT COUNT(*) FROM sessions WHERE source_file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )?;
            if refs == 0 {
                tx.execute("DELETE FROM scanned_files WHERE id = ?1", params![file_id])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// 记已删会话的墓碑：扫描层建行前查此表，共享日志轮转重解析不再把已删
    /// 会话重建为 0 用量空壳。重复删除幂等（INSERT OR IGNORE）。
    pub fn record_deleted_session(&self, tool: &str, external_id: &str, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO deleted_sessions (tool, external_id, deleted_at)
             VALUES (?1, ?2, ?3)",
            params![tool, external_id, now],
        )?;
        Ok(())
    }

    /// 会话是否已被用户删除（墓碑在册）。
    pub fn is_session_deleted(&self, tool: &str, external_id: &str) -> Result<bool> {
        let hit: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM deleted_sessions WHERE tool = ?1 AND external_id = ?2",
            params![tool, external_id],
            |row| row.get(0),
        )?;
        Ok(hit > 0)
    }

    /// 会话页分页：筛选/排序/分页都在 SQL 里做；用量按会话聚合。零用量的
    /// 会话壳（如只写会话头的日志）保留，聚合列为 0——"看过但没花钱"也是事实。
    /// 成本不在会话维度展示（产品决策）：会话行的成本既不查询也不返回。
    pub fn sessions_page(&self, query: &SessionsPageQuery) -> Result<SessionsPage> {
        let (where_sql, args) = sessions_where(&query.filters, &query.disabled);
        let from = "FROM sessions AS s
             LEFT JOIN usage_records AS r ON r.session_id = s.id";

        let total: i64 = self.conn.query_row(
            &format!("SELECT COUNT(*) FROM sessions AS s{where_sql}"),
            params_from_iter(args.iter()),
            |row| row.get(0),
        )?;

        let order = sessions_sort_expr(query.sort_key.as_deref());
        let dir = if query.sort_desc { "DESC" } else { "ASC" };

        let mut stmt = self.conn.prepare(&format!(
            "SELECT
                s.id, s.tool, s.external_id, s.title, s.project_dir, s.last_activity_at,
                COALESCE(SUM(r.request_count), 0),
                COALESCE(SUM(r.input_tokens), 0),
                COALESCE(SUM(r.output_tokens), 0),
                COALESCE(SUM(r.cache_read_tokens), 0),
                COALESCE(SUM(r.cache_write_tokens), 0)
             {from}{where_sql}
             GROUP BY s.id
             ORDER BY {order} {dir}, s.id DESC
             LIMIT ? OFFSET ?"
        ))?;
        let mut bindings = args.clone();
        bindings.push(Value::Integer(query.limit.max(0)));
        bindings.push(Value::Integer(query.offset.max(0)));
        let rows = stmt
            .query_map(params_from_iter(bindings.iter()), |row| {
                Ok(SessionRow {
                    id: row.get(0)?,
                    tool: row.get(1)?,
                    external_id: row.get(2)?,
                    title: row.get(3)?,
                    project_dir: row.get(4)?,
                    last_activity_at: row.get(5)?,
                    request_count: row.get(6)?,
                    input_tokens: row.get(7)?,
                    output_tokens: row.get(8)?,
                    cache_read_tokens: row.get(9)?,
                    cache_write_tokens: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(SessionsPage { rows, total })
    }
}

/// 会话页排序键 → SQL 表达式的白名单（聚合列直接展开表达式，不依赖 SELECT
/// 别名）；白名单外退回最近活跃。
fn sessions_sort_expr(key: Option<&str>) -> &'static str {
    match key {
        Some("title") => "s.title",
        Some("requests") => "COALESCE(SUM(r.request_count), 0)",
        Some("tokens") => {
            "(COALESCE(SUM(r.input_tokens), 0) + COALESCE(SUM(r.output_tokens), 0)
              + COALESCE(SUM(r.cache_read_tokens), 0) + COALESCE(SUM(r.cache_write_tokens), 0))"
        }
        _ => "s.last_activity_at",
    }
}

/// 会话页查询的 WHERE 子句与绑定参数。筛选只落在 sessions 自身的列上
/// （时间是最近活跃），聚合不参与筛选——HAVING 会让零用量壳永远不可见。
fn sessions_where(filters: &SessionFilters, disabled: &[String]) -> (String, Vec<Value>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();

    if !disabled.is_empty() {
        clauses.push(format!(
            "s.tool NOT IN ({})",
            question_marks(disabled.len())
        ));
        args.extend(disabled.iter().map(|t| Value::Text(t.clone())));
    }
    if let Some(start) = filters.time_start {
        clauses.push("s.last_activity_at >= ?".into());
        args.push(Value::Integer(start));
    }
    if let Some(end) = filters.time_end {
        clauses.push("s.last_activity_at <= ?".into());
        args.push(Value::Integer(end));
    }
    if !filters.tools.is_empty() {
        clauses.push(format!(
            "s.tool IN ({})",
            question_marks(filters.tools.len())
        ));
        args.extend(filters.tools.iter().map(|t| Value::Text(t.clone())));
    }
    if let Some(term) = filters
        .search
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        let pattern = like_pattern(term);
        clauses.push(
            "(s.title LIKE ? ESCAPE '\\'
              OR s.external_id LIKE ? ESCAPE '\\'
              OR s.project_dir LIKE ? ESCAPE '\\')"
                .into(),
        );
        args.extend(std::iter::repeat_n(Value::Text(pattern), 3));
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    };
    (where_sql, args)
}
