//! 数据库类来源会话删除的共用实现。契约见 [`Adapter::delete_db_session`]
//! （super）：导出白名单表 → bundle 移入系统回收站 → 按主键删行，级联清理
//! 由工具库自己的外键承担。红线：只按会话主键 DELETE、不 UPDATE、不建表；
//! 导出只认调用方给的白名单；凭据类表绝不触碰。

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, params};

use crate::error::{Error, Result};

/// 删除一个数据库类来源的会话。会话行（或 session 表本身）不存在时返回
/// `Ok(None)`：来源已无可清之物，删除意图视为达成，调用方照常清 Toktol
/// 侧归属——混合来源工具（zcode）的老 rollout 文件可能早于现存库。
pub(crate) fn delete_session_rows(
    db_path: &Path,
    external_id: &str,
    bundle_dir: &Path,
    trash: &dyn Fn(&Path) -> Result<()>,
    tool: &str,
    export_tables: &[(&str, &str)],
) -> Result<Option<PathBuf>> {
    // 不带 CREATE：库已消失时直接报错，绝不凭空造一个空库。
    let mut db = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|err| Error::Internal(format!("{tool} 会话库打开失败 {db_path:?}: {err}")))?;
    // 工具可能在运行：写锁竞争靠 busy_timeout 等待；级联删行要求外键生效，
    // 而该开关是每连接各自记忆、默认关闭。
    db.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(Error::from)?;
    db.pragma_update(None, "foreign_keys", "ON")
        .map_err(Error::from)?;
    // IMMEDIATE：导出与删行之间不容他人插写，入桶快照 == 删行状态。
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(Error::from)?;

    let session_table: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session'",
            [],
            |row| row.get(0),
        )
        .map_err(Error::from)?;
    let exists: i64 = if session_table == 0 {
        0
    } else {
        tx.query_row(
            "SELECT COUNT(*) FROM session WHERE id = ?1",
            params![external_id],
            |row| row.get(0),
        )
        .map_err(Error::from)?
    };
    if exists == 0 {
        return Ok(None);
    }

    let mut tables = serde_json::Map::new();
    tables.insert(
        "session".into(),
        serde_json::Value::Array(rows_by_key(&tx, "session", "id", external_id)?),
    );
    for (table, key_col) in export_tables {
        let present: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .map_err(Error::from)?;
        // 表不存在 = 老版本库，跳过：可恢复以主表与内容表为准。
        if present == 0 {
            continue;
        }
        let rows = rows_by_key(&tx, table, key_col, external_id)?;
        tables.insert((*table).to_string(), serde_json::Value::Array(rows));
    }

    let exported_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(0))
        .unwrap_or(0);
    let bundle = serde_json::json!({
        "tool": tool,
        "db": db_path.to_string_lossy(),
        "sessionId": external_id,
        "exportedAtMs": exported_at_ms,
        "tables": serde_json::Value::Object(tables),
    });
    let text = serde_json::to_string_pretty(&bundle)?;

    // 先入桶、后动库（与文件类来源同一顺序不变式）：bundle 落地暂存目录、
    // 移入系统回收站，全部成功才删行。入桶失败 → 事务回滚，库纹丝不动；
    // 暂存文件留在原地无害。回收站在事务提交前发生：若提交本身失败，bundle
    // 已入桶而行还在——可恢复状态，重试即自愈。
    let safe_id: String = external_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let bundle_path = bundle_dir.join(format!("{tool}-session-{safe_id}-{exported_at_ms}.json"));
    std::fs::create_dir_all(bundle_dir).map_err(|source| Error::DataFile {
        path: bundle_dir.to_path_buf(),
        source,
    })?;
    std::fs::write(&bundle_path, text).map_err(|source| Error::DataFile {
        path: bundle_path.clone(),
        source,
    })?;
    trash(&bundle_path)?;

    let deleted = tx
        .execute("DELETE FROM session WHERE id = ?1", params![external_id])
        .map_err(Error::from)?;
    if deleted == 0 {
        return Err(Error::Internal(format!(
            "{tool} session row vanished during delete: {external_id}"
        )));
    }
    tx.commit().map_err(Error::from)?;
    Ok(Some(bundle_path))
}

/// 导出一张白名单表里属于某个会话的全部行为 JSON 对象数组（列名 → 值）。
fn rows_by_key(
    tx: &rusqlite::Transaction<'_>,
    table: &str,
    key_col: &str,
    key: &str,
) -> Result<Vec<serde_json::Value>> {
    let sql = format!("SELECT * FROM {table} WHERE {key_col} = ?1");
    let mut stmt = tx.prepare(&sql).map_err(Error::from)?;
    let names: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
    let rows = stmt
        .query_map(params![key], |row| {
            let mut obj = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                obj.insert(name.clone(), value_ref_to_json(row.get_ref(i)?));
            }
            Ok(serde_json::Value::Object(obj))
        })
        .map_err(Error::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(Error::from)?);
    }
    Ok(out)
}

/// 单个单元格 → JSON。这些表全是 TEXT/INTEGER/REAL，BLOB 是兜底分支：
/// 转十六进制串，绝不丢数据。
fn value_ref_to_json(value: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => serde_json::Value::Null,
        ValueRef::Integer(i) => serde_json::Value::from(i),
        ValueRef::Real(f) => serde_json::Value::from(f),
        ValueRef::Text(t) => serde_json::Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => {
            serde_json::Value::String(b.iter().map(|byte| format!("{byte:02x}")).collect())
        }
    }
}
