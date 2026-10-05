//! 会话删除编排：会话数据移入系统回收站（产品红线，绝不硬删），统计保留——
//! `usage_records.session_id` 置 NULL、会话行删除；
//! 重扫重建的零用量空壳不会重复计账（防复活）。
//!
//! 文件类来源：日志文件进回收站。数据库类来源（opencode 全部、zcode 被轮转
//! 会话）：适配器把会话行导出成 JSON bundle、bundle 进回收站，随后才对工具
//! 自有库删行（用户主动删除的写例外；级联由工具库自己的外键承担）。双来源
//! 工具（zcode 的文件会话）文件与库行双清，不留可重扫重建的空壳。
//!
//! 顺序刻意安排：先全部入桶、全部成功才动数据库——任何一步失败整体中止，
//! 不会出现"文件没了、归属还挂着"的中间态。已入桶的文件留在桶里无害。

pub mod transcript;

pub use transcript::{
    TRANSCRIPT_CHECKPOINT_BYTES, TranscriptPageEntry, TranscriptPagePayload, TranscriptTurn,
    TranscriptTurnsPayload, build_transcript_index, session_transcript, session_transcript_page,
    session_transcript_turns,
};

use std::path::Path;

use crate::adapter::Adapter;
use crate::error::{Error, Result};
use crate::model::Tool;
use crate::storage::Storage;

/// 一次删除的结果：移入回收站的路径列表（供前端展示）。
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashReport {
    /// 成功移入回收站的路径。
    pub trashed: Vec<String>,
}

/// 批量删除的结果：成功入桶的路径 + 未能删除的会话 id。
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTrashReport {
    /// 成功移入回收站的路径。
    pub trashed: Vec<String>,
    /// 未能删除的会话 id（不存在、数据库来源不可删等）。
    pub failed: Vec<i64>,
}

/// 移入回收站的单个动作，测试注入用。
type TrashFn<'a> = &'a dyn Fn(&Path) -> Result<()>;

/// 默认回收站动作：交还真实实现，命令层与批量入口共用。
fn system_trash(path: &Path) -> Result<()> {
    trash::delete(path)
        .map_err(|err| Error::Internal(format!("moving to recycle bin failed: {err}")))
}

/// 删除一个会话：数据进回收站 + 数据库清归属（统计保留）。
pub fn trash_session(storage: &Storage, session_id: i64) -> Result<TrashReport> {
    let bundle_dir = crate::paths::trash_staging_dir().ok_or(Error::HomeDirUnavailable)?;
    trash_one(storage, session_id, &bundle_dir, &system_trash)
}

/// 批量删除：单个失败（不存在、数据库来源未支持删除）记入 failed，不阻断整批。
pub fn trash_sessions(storage: &Storage, session_ids: &[i64]) -> Result<BatchTrashReport> {
    let bundle_dir = crate::paths::trash_staging_dir().ok_or(Error::HomeDirUnavailable)?;
    trash_sessions_with(storage, session_ids, &bundle_dir, &system_trash)
}

/// [`trash_sessions`] 的可注入形态：测试注入假回收站，不碰系统回收站。
pub fn trash_sessions_with(
    storage: &Storage,
    session_ids: &[i64],
    bundle_dir: &Path,
    trash: TrashFn,
) -> Result<BatchTrashReport> {
    let mut report = BatchTrashReport::default();
    for id in session_ids {
        match trash_one(storage, *id, bundle_dir, trash) {
            Ok(single) => report.trashed.extend(single.trashed),
            Err(_) => report.failed.push(*id),
        }
    }
    Ok(report)
}

fn trash_one(
    storage: &Storage,
    session_id: i64,
    bundle_dir: &Path,
    trash: TrashFn,
) -> Result<TrashReport> {
    let Some(session) = storage.session_info(session_id)? else {
        return Err(Error::Internal(format!("session not found: {session_id}")));
    };
    let adapter = find_adapter(parse_tool(&session.tool)?)?;
    trash_session_with(storage, session_id, adapter.as_ref(), bundle_dir, trash)
}

/// [`trash_session`] 的可注入形态：适配器一并注入，测试才不必碰真实家目录
/// （数据库来源判定要对比"会话登记源 == 工具自有库路径"，默认适配器拿的是
/// 真实路径，注入临时库的用例必须带上自己的适配器）。
pub fn trash_session_with(
    storage: &Storage,
    session_id: i64,
    adapter: &dyn Adapter,
    bundle_dir: &Path,
    trash: TrashFn,
) -> Result<TrashReport> {
    let Some(session) = storage.session_info(session_id)? else {
        return Err(Error::Internal(format!("session not found: {session_id}")));
    };

    let source = storage.scanned_file_path(session.source_file_id)?;
    // 数据库类来源判定：会话登记的源就是工具自有库。命中的是 opencode（全部
    // 会话）与 zcode 被轮转的会话；zcode 的 rollout 文件会话不匹配，走文件流。
    let db_path = adapter.db_source_path();
    let db_source = match (&db_path, source.as_deref()) {
        (Some(db), Some(src)) if src == db.to_string_lossy().as_ref() => Some(db.as_path()),
        _ => None,
    };

    let mut trashed = Vec::new();
    if let Some(db) = db_source {
        // 数据库类来源：没有文件可入桶。删除 = 适配器把会话行导出成 bundle
        // 移入回收站（可恢复），成功后才对工具库删行——用户主动删除的写例外。
        // 行已不存在时 None：来源无可清之物，照常清归属。未实现
        // delete_db_session 的来源（codebuddy）维持拒绝。
        if let Some(bundle) =
            adapter.delete_db_session(db, &session.external_id, bundle_dir, trash)?
        {
            trashed.push(bundle.to_string_lossy().into_owned());
        }
    } else {
        // 文件类来源：引用保护——还有其他会话共用此文件时只清归属、文件不动。
        let refs = storage.count_sessions_for_file(session.source_file_id)?;
        if refs == 1
            && let Some(source) = source
        {
            for artifact in adapter.session_artifacts(Path::new(&source)) {
                if !artifact.exists() {
                    continue; // 目标已消失（如用户手动删过）：无可入桶，不算错误。
                }
                trash(&artifact)?;
                trashed.push(artifact.to_string_lossy().into_owned());
            }
        }
        // 双来源工具（zcode）：文件之外，工具自有库也存着该会话的事实行
        //（用量、标题），只清文件会留下可被重扫重建的空壳。库行清理走同一
        // 个导出→入桶→删行流程；行不存在（库被清过/重建过）视为无事可做。
        if adapter.purge_db_rows_for_file_sessions()
            && let Some(db) = db_path.as_deref()
            && let Some(bundle) =
                adapter.delete_db_session(db, &session.external_id, bundle_dir, trash)?
        {
            trashed.push(bundle.to_string_lossy().into_owned());
        }
    }

    // 统计保留：usage_records.session_id 置 NULL，会话行删除，簿记行按引用清理。
    storage.delete_session(session_id)?;
    // 墓碑：共享日志轮转重解析不再把已删会话重建为空壳（对一切工具生效，
    // codebuddy 的扩展日志尤其依赖——它的用量行删不掉，永远躺在日志里）。
    storage.record_deleted_session(
        &session.tool,
        &session.external_id,
        chrono::Utc::now().timestamp_millis(),
    )?;

    Ok(TrashReport { trashed })
}

fn parse_tool(name: &str) -> Result<Tool> {
    crate::model::Tool::ALL
        .iter()
        .copied()
        .find(|tool| tool.as_str() == name)
        .ok_or_else(|| Error::Internal(format!("unknown tool: {name}")))
}

pub(crate) fn find_adapter(tool: Tool) -> Result<Box<dyn Adapter>> {
    crate::adapter::adapters()
        .into_iter()
        .find(|adapter| adapter.tool() == tool)
        .ok_or_else(|| Error::Internal(format!("no adapter registered: {}", tool.as_str())))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::adapter::LineParse;
    use crate::adapter::codex::CodexAdapter;
    use crate::adapter::opencode::OpenCodeAdapter;
    use crate::adapter::zcode::ZcodeAdapter;
    use crate::error::Error;
    use crate::storage::{NewSession, NewUsageRecord};

    fn test_db(tag: &str) -> Storage {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "toktol-sessions-db-{}-{}-{tag}.db",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_file(&path);
        crate::storage::open(&path).expect("打开测试库")
    }

    /// 导出包暂存目录（测试版）：随用例建删，不碰真实 `~/.toktol/`。
    fn test_staging(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toktol-sessions-stage-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 建一个带用量的会话：临时日志文件 + 簿记 + 会话 + 一条用量记录。
    fn setup_session(
        storage: &Storage,
        dir: &Path,
        name: &str,
        external_id: &str,
        dedup: u8,
    ) -> (PathBuf, i64) {
        let file = dir.join(name);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(&file, b"{}\n").unwrap();
        let file_id = storage
            .upsert_scanned_file(file.to_string_lossy().as_ref(), "codex", &[0xAB], 3, 3, 0)
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "codex".into(),
                    external_id: external_id.into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();
        storage
            .insert_usage_record(&NewUsageRecord {
                session_id: Some(session_id),
                tool: "codex".into(),
                model_raw: "raw-model".into(),
                model: "raw-model".into(),
                ts: 1,
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                duration_ms: None,
                request_count: 1,
                dedup_key: vec![dedup],
            })
            .unwrap();
        (file, session_id)
    }

    /// 注入的回收站：把文件移进 bin 目录并记录进 `moved`（调用方持有列表）。
    fn bin_trash<'a>(
        bin: &'a Path,
        moved: &'a std::cell::RefCell<Vec<String>>,
    ) -> impl Fn(&Path) -> Result<()> + 'a {
        let bin = bin.to_path_buf();
        move |path: &Path| -> Result<()> {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            std::fs::rename(path, bin.join(&name))
                .map_err(|e| Error::Internal(format!("fake trash failed: {e}")))?;
            moved.borrow_mut().push(path.to_string_lossy().into_owned());
            Ok(())
        }
    }

    /// 模拟 opencode 工具库：实测 schema 的相关子集（含级联外键）。两个会话
    /// 各带消息/部件/待办，验证"删一留一"。
    fn setup_opencode_tool_db(path: &Path) {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL,
                 time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 time_created INTEGER NOT NULL, data TEXT NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                 session_id TEXT NOT NULL, data TEXT NOT NULL,
                 FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE);
             CREATE TABLE todo (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE);
             INSERT INTO session VALUES ('ses_a', 'A', 1, 1), ('ses_b', 'B', 2, 2);
             INSERT INTO message VALUES
                 ('msg_a1', 'ses_a', 10, '{}'), ('msg_a2', 'ses_a', 11, '{}'),
                 ('msg_b1', 'ses_b', 20, '{}');
             INSERT INTO part VALUES
                 ('prt_a1', 'msg_a1', 'ses_a', '{}'),
                 ('prt_a2', 'msg_a2', 'ses_a', '{}');
             INSERT INTO todo VALUES ('todo_a', 'ses_a');",
        )
        .unwrap();
    }

    /// 模拟 zcode 应用库：实测 schema 的相关子集（含级联外键）。两个会话
    /// 各带消息/部件/用量/待办，验证"删一留一"。
    fn setup_zcode_tool_db(path: &Path) {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, title TEXT NOT NULL,
                 time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 time_created INTEGER NOT NULL, data TEXT NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL,
                 session_id TEXT NOT NULL, data TEXT NOT NULL,
                 FOREIGN KEY (message_id) REFERENCES message(id) ON DELETE CASCADE);
             CREATE TABLE model_usage (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 input_tokens INTEGER NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE);
             CREATE TABLE todo (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                 FOREIGN KEY (session_id) REFERENCES session(id) ON DELETE CASCADE);
             INSERT INTO session VALUES ('ses_a', 'A', 1, 1), ('ses_b', 'B', 2, 2);
             INSERT INTO message VALUES
                 ('msg_a1', 'ses_a', 10, '{}'), ('msg_a2', 'ses_a', 11, '{}'),
                 ('msg_b1', 'ses_b', 20, '{}');
             INSERT INTO part VALUES
                 ('prt_a1', 'msg_a1', 'ses_a', '{}'),
                 ('prt_a2', 'msg_a2', 'ses_a', '{}');
             INSERT INTO model_usage VALUES
                 ('mu_a1', 'ses_a', 10), ('mu_a2', 'ses_a', 12), ('mu_b1', 'ses_b', 20);
             INSERT INTO todo VALUES ('todo_a', 'ses_a');",
        )
        .unwrap();
    }

    /// 最小数据库类适配器：不实现 delete_db_session。钉住"未实现删除的
    /// 数据库来源维持拒绝"的契约（codebuddy 的现状）。
    struct DbOnlyAdapter(PathBuf);

    impl Adapter for DbOnlyAdapter {
        fn tool(&self) -> crate::model::Tool {
            crate::model::Tool::CodeBuddy
        }
        fn log_dirs(&self) -> Vec<PathBuf> {
            vec![]
        }
        fn is_session_log(&self, _path: &Path) -> bool {
            false
        }
        fn parse_line(&self, _line: &str) -> LineParse {
            LineParse::Skip
        }
        fn db_source_path(&self) -> Option<PathBuf> {
            Some(self.0.clone())
        }
    }

    #[test]
    fn delete_trashes_files_and_preserves_usage() {
        let dir = std::env::temp_dir().join(format!("toktol-sessions-del-{}", std::process::id()));
        let bin = std::env::temp_dir().join(format!("toktol-sessions-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let storage = test_db("del");
        let (file, session_id) = setup_session(&storage, &dir, "a.jsonl", "s1", 1);

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let staging = test_staging("del");
        let report =
            trash_session_with(&storage, session_id, &CodexAdapter, &staging, &trash).unwrap();

        assert_eq!(moved.borrow().len(), 1, "文件应移入回收站");
        assert_eq!(report.trashed, moved.borrow().clone());
        assert!(!file.exists(), "原文件必须已离开原位");

        let (orphan, sessions, files): (i64, i64, i64) = storage
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL),
                        (SELECT COUNT(*) FROM sessions),
                        (SELECT COUNT(*) FROM scanned_files)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(orphan, 1, "用量必须保留，只脱离归属");
        assert_eq!(sessions, 0, "会话行删除");
        assert_eq!(files, 0, "无引用的簿记行删除");
        assert!(
            storage.is_session_deleted("codex", "s1").unwrap(),
            "墓碑在册：共享日志轮转重解析不再重建空壳"
        );

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
    }

    #[test]
    fn batch_delete_collects_failures_without_blocking() {
        let dir =
            std::env::temp_dir().join(format!("toktol-sessions-batch-{}", std::process::id()));
        let bin = std::env::temp_dir().join(format!("toktol-sessions-bin3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let storage = test_db("batch");
        let (file, s1) = setup_session(&storage, &dir, "a.jsonl", "s1", 1);

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let staging = test_staging("batch");
        let report = trash_sessions_with(&storage, &[s1, 99_999], &staging, &trash).unwrap();

        assert_eq!(report.failed, vec![99_999], "不存在的会话记入 failed");
        assert_eq!(moved.borrow().len(), 1, "有效会话不受整批中失败项影响");
        assert!(!file.exists());

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
    }

    #[test]
    fn shared_file_is_not_trashed_only_unlinked() {
        let dir =
            std::env::temp_dir().join(format!("toktol-sessions-shared-{}", std::process::id()));
        let bin = std::env::temp_dir().join(format!("toktol-sessions-bin2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let storage = test_db("shared");
        // 两个会话共用同一源文件。
        let (file, s1) = setup_session(&storage, &dir, "a.jsonl", "s1", 1);
        let file_id = storage.session_info(s1).unwrap().unwrap().source_file_id;
        let (s2, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "codex".into(),
                    external_id: "s2".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let staging = test_staging("shared");
        trash_session_with(&storage, s1, &CodexAdapter, &staging, &trash).unwrap();
        assert_eq!(moved.borrow().len(), 0, "共享文件不得进回收站");
        assert!(file.exists(), "文件必须留在原位");
        let files: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM scanned_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(files, 1, "簿记行保留");

        // 最后一个引用者删除后，文件才进回收站。
        trash_session_with(&storage, s2, &CodexAdapter, &staging, &trash).unwrap();
        assert_eq!(moved.borrow().len(), 1);
        assert!(!file.exists());

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
    }

    #[test]
    fn missing_files_delete_cleanly() {
        let dir = std::env::temp_dir().join(format!("toktol-sessions-miss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let storage = test_db("miss");
        let (file, session_id) = setup_session(&storage, &dir, "a.jsonl", "s1", 1);
        std::fs::remove_file(&file).unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&dir, &moved);
        let staging = test_staging("miss");
        trash_session_with(&storage, session_id, &CodexAdapter, &staging, &trash).unwrap();
        assert_eq!(moved.borrow().len(), 0, "不存在的文件不进回收站");

        let sessions: i64 = storage
            .conn()
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opencode_db_session_delete_exports_and_cascades() {
        let dir =
            std::env::temp_dir().join(format!("toktol-sessions-ocdel-{}", std::process::id()));
        let bin =
            std::env::temp_dir().join(format!("toktol-sessions-ocbin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let staging = test_staging("ocdel");
        let tool_db = dir.join("opencode.db");
        std::fs::create_dir_all(&dir).unwrap();
        setup_opencode_tool_db(&tool_db);
        let storage = test_db("ocdel");
        let file_id = storage
            .upsert_scanned_file(
                tool_db.to_string_lossy().as_ref(),
                "opencode",
                &[0x01],
                9,
                9,
                0,
            )
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "opencode".into(),
                    external_id: "ses_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();
        storage
            .insert_usage_record(&NewUsageRecord {
                session_id: Some(session_id),
                tool: "opencode".into(),
                model_raw: "raw-model".into(),
                model: "raw-model".into(),
                ts: 1,
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                duration_ms: None,
                request_count: 1,
                dedup_key: vec![1],
            })
            .unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let adapter = OpenCodeAdapter {
            db_path: Some(tool_db.clone()),
        };
        let report = trash_session_with(&storage, session_id, &adapter, &staging, &trash).unwrap();

        assert_eq!(moved.borrow().len(), 1, "入桶的恰是导出包");
        assert!(report.trashed[0].ends_with(".json"));

        // 工具库：被删会话的行全部级联消失，其余会话原样保留。
        let tool = rusqlite::Connection::open(&tool_db).unwrap();
        let (a_msgs, b_msgs, sessions): (i64, i64, i64) = tool
            .query_row(
                "SELECT (SELECT COUNT(*) FROM message WHERE session_id = 'ses_a'),
                        (SELECT COUNT(*) FROM message WHERE session_id = 'ses_b'),
                        (SELECT COUNT(*) FROM session)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((a_msgs, b_msgs, sessions), (0, 1, 1));
        let (a_parts, a_todos): (i64, i64) = tool
            .query_row(
                "SELECT (SELECT COUNT(*) FROM part WHERE session_id = 'ses_a'),
                        (SELECT COUNT(*) FROM todo WHERE session_id = 'ses_a')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((a_parts, a_todos), (0, 0));

        // bundle 内容可恢复：会话行与消息行都在。
        let bundle_name = Path::new(&report.trashed[0])
            .file_name()
            .unwrap()
            .to_os_string();
        let bundle_text = std::fs::read_to_string(bin.join(bundle_name)).unwrap();
        assert!(bundle_text.contains("\"sessionId\": \"ses_a\""));
        assert!(bundle_text.contains("msg_a1"));

        // toktol 侧：统计保留、会话行删除，与文件类来源同语义。
        let (orphan, sessions_left): (i64, i64) = storage
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL),
                        (SELECT COUNT(*) FROM sessions)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(orphan, 1, "用量必须保留，只脱离归属");
        assert_eq!(sessions_left, 0);

        drop(tool); // Windows：连接未关时删目录会撞文件锁
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
        std::fs::remove_dir_all(&staging).unwrap();
    }

    #[test]
    fn opencode_delete_aborts_when_trash_fails() {
        let dir =
            std::env::temp_dir().join(format!("toktol-sessions-ocfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staging = test_staging("ocfail");
        let tool_db = dir.join("opencode.db");
        setup_opencode_tool_db(&tool_db);
        let storage = test_db("ocfail");
        let file_id = storage
            .upsert_scanned_file(
                tool_db.to_string_lossy().as_ref(),
                "opencode",
                &[0x01],
                9,
                9,
                0,
            )
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "opencode".into(),
                    external_id: "ses_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        // 入桶失败：先全部入桶、才动库的顺序不变式——事务必须回滚。
        let trash =
            |_path: &Path| -> Result<()> { Err(Error::Internal("recycle bin broken".into())) };
        let adapter = OpenCodeAdapter {
            db_path: Some(tool_db.clone()),
        };
        let err = trash_session_with(&storage, session_id, &adapter, &staging, &trash).unwrap_err();
        assert_eq!(err.code().as_str(), "core.internal");

        let tool = rusqlite::Connection::open(&tool_db).unwrap();
        let sessions: i64 = tool
            .query_row("SELECT COUNT(*) FROM session", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sessions, 2, "工具库必须纹丝不动");
        assert!(
            std::fs::read_dir(&staging).unwrap().next().is_some(),
            "落地的导出包留在暂存目录（无害留底）"
        );

        drop(tool); // Windows：连接未关时删目录会撞文件锁
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&staging).unwrap();
    }

    #[test]
    fn zcode_db_session_delete_exports_and_cascades() {
        let dir = std::env::temp_dir().join(format!("toktol-sessions-zdel-{}", std::process::id()));
        let bin = std::env::temp_dir().join(format!("toktol-sessions-zbin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let staging = test_staging("zdel");
        let tool_db = dir.join("db.sqlite");
        std::fs::create_dir_all(&dir).unwrap();
        setup_zcode_tool_db(&tool_db);
        let storage = test_db("zdel");
        let file_id = storage
            .upsert_scanned_file(
                tool_db.to_string_lossy().as_ref(),
                "zcode",
                &[0x02],
                5,
                5,
                0,
            )
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "zcode".into(),
                    external_id: "ses_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();
        storage
            .insert_usage_record(&NewUsageRecord {
                session_id: Some(session_id),
                tool: "zcode".into(),
                model_raw: "raw-model".into(),
                model: "raw-model".into(),
                ts: 1,
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: None,
                duration_ms: None,
                request_count: 1,
                dedup_key: vec![1],
            })
            .unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let adapter = ZcodeAdapter {
            db_path: Some(tool_db.clone()),
            rollout_dir: None,
            session_meta: std::sync::OnceLock::new(),
        };
        let report = trash_session_with(&storage, session_id, &adapter, &staging, &trash).unwrap();

        assert_eq!(moved.borrow().len(), 1, "入桶的恰是导出包");
        assert!(report.trashed[0].ends_with(".json"));

        // 工具库：被删会话的行全部级联消失，其余会话原样保留。
        let tool = rusqlite::Connection::open(&tool_db).unwrap();
        let (a_usage, b_usage, sessions): (i64, i64, i64) = tool
            .query_row(
                "SELECT (SELECT COUNT(*) FROM model_usage WHERE session_id = 'ses_a'),
                        (SELECT COUNT(*) FROM model_usage WHERE session_id = 'ses_b'),
                        (SELECT COUNT(*) FROM session)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((a_usage, b_usage, sessions), (0, 1, 1));
        let (a_msgs, a_parts, a_todos): (i64, i64, i64) = tool
            .query_row(
                "SELECT (SELECT COUNT(*) FROM message WHERE session_id = 'ses_a'),
                        (SELECT COUNT(*) FROM part WHERE session_id = 'ses_a'),
                        (SELECT COUNT(*) FROM todo WHERE session_id = 'ses_a')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((a_msgs, a_parts, a_todos), (0, 0, 0));

        // bundle 内容可恢复：会话行与消息行都在。
        let bundle_name = Path::new(&report.trashed[0])
            .file_name()
            .unwrap()
            .to_os_string();
        let bundle_text = std::fs::read_to_string(bin.join(bundle_name)).unwrap();
        assert!(bundle_text.contains("\"sessionId\": \"ses_a\""));
        assert!(bundle_text.contains("msg_a1"));

        // toktol 侧：统计保留、会话行删除，与文件类来源同语义。
        let (orphan, sessions_left): (i64, i64) = storage
            .conn()
            .query_row(
                "SELECT (SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL),
                        (SELECT COUNT(*) FROM sessions)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(orphan, 1, "用量必须保留，只脱离归属");
        assert_eq!(sessions_left, 0);

        drop(tool); // Windows：连接未关时删目录会撞文件锁
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
        std::fs::remove_dir_all(&staging).unwrap();
    }

    #[test]
    fn zcode_file_session_delete_trashes_file_and_purges_db_rows() {
        let dir = std::env::temp_dir().join(format!("toktol-sessions-zhyb-{}", std::process::id()));
        let bin =
            std::env::temp_dir().join(format!("toktol-sessions-zhbin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let staging = test_staging("zhyb");
        // 双来源并存：rollout 文件（登记源）+ 应用库行。
        let rollout = dir.join("model-io-ses_a.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&rollout, b"{}\n").unwrap();
        let tool_db = dir.join("db.sqlite");
        setup_zcode_tool_db(&tool_db);
        let storage = test_db("zhyb");
        let file_id = storage
            .upsert_scanned_file(
                rollout.to_string_lossy().as_ref(),
                "zcode",
                &[0xAA],
                3,
                3,
                0,
            )
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "zcode".into(),
                    external_id: "ses_a".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let adapter = ZcodeAdapter {
            db_path: Some(tool_db.clone()),
            rollout_dir: None,
            session_meta: std::sync::OnceLock::new(),
        };
        let report = trash_session_with(&storage, session_id, &adapter, &staging, &trash).unwrap();

        assert_eq!(moved.borrow().len(), 2, "rollout 文件与导出包都要入桶");
        assert_eq!(report.trashed.len(), 2);
        assert!(!rollout.exists(), "rollout 文件必须离开原位");
        let tool = rusqlite::Connection::open(&tool_db).unwrap();
        let (sessions, a_usage): (i64, i64) = tool
            .query_row(
                "SELECT (SELECT COUNT(*) FROM session),
                        (SELECT COUNT(*) FROM model_usage WHERE session_id = 'ses_a')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((sessions, a_usage), (1, 0), "库行双清，其余会话保留");

        drop(tool); // Windows：连接未关时删目录会撞文件锁
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
        std::fs::remove_dir_all(&staging).unwrap();
    }

    #[test]
    fn zcode_file_session_delete_survives_missing_db_rows() {
        let dir =
            std::env::temp_dir().join(format!("toktol-sessions-zmiss-{}", std::process::id()));
        let bin =
            std::env::temp_dir().join(format!("toktol-sessions-zmbin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        let staging = test_staging("zmiss");
        // 库被清过/重建过：零字节空库里没有 session 表。
        let rollout = dir.join("model-io-ses_old.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&rollout, b"{}\n").unwrap();
        let tool_db = dir.join("db.sqlite");
        std::fs::write(&tool_db, b"").unwrap();
        let storage = test_db("zmiss");
        let file_id = storage
            .upsert_scanned_file(
                rollout.to_string_lossy().as_ref(),
                "zcode",
                &[0xAB],
                3,
                3,
                0,
            )
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "zcode".into(),
                    external_id: "ses_old".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&bin, &moved);
        let adapter = ZcodeAdapter {
            db_path: Some(tool_db.clone()),
            rollout_dir: None,
            session_meta: std::sync::OnceLock::new(),
        };
        let report = trash_session_with(&storage, session_id, &adapter, &staging, &trash).unwrap();

        assert_eq!(moved.borrow().len(), 1, "只有文件入桶，无行可清不算错误");
        assert!(!report.trashed[0].ends_with(".json"));
        assert!(!rollout.exists());

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&bin).unwrap();
        std::fs::remove_dir_all(&staging).unwrap();
    }

    #[test]
    fn db_source_without_delete_impl_stays_unsupported() {
        let dir =
            std::env::temp_dir().join(format!("toktol-sessions-dbfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let staging = test_staging("dbunsup");
        let storage = test_db("dbunsup");
        // codebuddy 现状：数据库类来源但未实现 delete_db_session，维持拒绝。
        let db_file = dir.join("codebuddy-sessions.vscdb");
        std::fs::write(&db_file, b"sqlite").unwrap();
        let file_id = storage
            .upsert_scanned_file(
                db_file.to_string_lossy().as_ref(),
                "codebuddy",
                &[0x03],
                6,
                6,
                0,
            )
            .unwrap();
        let (session_id, _) = storage
            .get_or_create_session(
                &NewSession {
                    tool: "codebuddy".into(),
                    external_id: "s1".into(),
                    external_id_is_derived: false,
                    title: None,
                    project_dir: None,
                    source_file_id: file_id,
                },
                0,
            )
            .unwrap();

        let moved = std::cell::RefCell::new(Vec::new());
        let trash = bin_trash(&dir, &moved);
        let adapter = DbOnlyAdapter(db_file.clone());
        let err = trash_session_with(&storage, session_id, &adapter, &staging, &trash).unwrap_err();
        assert_eq!(err.code().as_str(), "core.unsupported");
        assert_eq!(moved.borrow().len(), 0, "绝不回收工具的数据库文件");
        assert!(db_file.exists());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
