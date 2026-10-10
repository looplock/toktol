//! `storage` 集成测试：临时库上跑真实 SQL，覆盖迁移、写入去重、聚合口径与重算。

use std::path::{Path, PathBuf};

use super::overview::{TrendGrain, bucket_starts, union_duration_ms};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Datelike, Local, TimeZone};
use rusqlite::{OptionalExtension, params};

use super::*;

/// 每个测试独享一个临时库文件，避免相互干扰；退出前尽力清掉三件套（db/wal/shm）。
fn test_db(tag: &str) -> (Storage, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "toktol-storage-test-{}-{}-{tag}.db",
        std::process::id(),
        seq
    ));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(path.with_extension(format!("db{suffix}")));
    }
    (open(&path).expect("打开测试库"), path)
}

/// 测试库三件套清理（Storage 已 drop，连接已关）。
fn cleanup(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(path.with_extension(format!("db{suffix}")));
    }
}

fn record(session_id: Option<i64>, dedup: u8) -> NewUsageRecord {
    NewUsageRecord {
        session_id,
        tool: "claude-code".into(),
        model_raw: "claude-sonnet-4-5-20250929".into(),
        model: "claude-sonnet-4-5".into(),
        ts: 1_000,
        input_tokens: 1_000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        duration_ms: None,
        request_count: 1,
        dedup_key: vec![dedup],
    }
}

#[test]
fn migrations_are_idempotent_and_wal_is_on() {
    let (storage, path) = test_db("migrate");
    let version: i64 = storage
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version as usize, migrations::MIGRATIONS.len());

    let journal: String = storage
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");

    drop(storage);
    let reopened = open(&path).expect("重开已迁移的库不应再跑迁移");
    let version2: i64 = reopened
        .conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version2 as usize, migrations::MIGRATIONS.len());
    drop(reopened);
    cleanup(&path);
}

/// v13 把 opencode 库源的指纹置空：指纹不失效，"指纹未变→跳过"分支会
/// 挡住 v12 删除行的重建（实测踩坑）。模拟库停在 v12 重开，v13 必须执行。
#[test]
fn v13_invalidates_opencode_fingerprint() {
    let (storage, path) = test_db("v13-fingerprint");
    storage
        .conn
        .execute(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
             VALUES ('C:/opencode.db', 'opencode', x'ABCD', 1, 0, 1)",
            [],
        )
        .unwrap();
    // 库已停在最高版本；回拨到 v12 重开，只重跑 v13。
    storage
        .conn
        .execute("PRAGMA user_version = 12", [])
        .unwrap();
    drop(storage);

    let reopened = open(&path).unwrap();
    let hash: Vec<u8> = reopened
        .conn
        .query_row(
            "SELECT content_hash FROM scanned_files WHERE tool = 'opencode'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(hash.is_empty(), "v13 必须把 opencode 指纹置空");
    // 其他工具不受影响——用另一行验证置空是定向的。
    reopened
        .conn
        .execute(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
             VALUES ('C:/codex.jsonl', 'codex', x'EE', 1, 0, 1)",
            [],
        )
        .unwrap();
    let codex_hash: Vec<u8> = reopened
        .conn
        .query_row(
            "SELECT content_hash FROM scanned_files WHERE tool = 'codex'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(codex_hash, vec![0xEE]);
    drop(reopened);
    cleanup(&path);
}

/// v19 回填请求耗时：删两工具在册会话的用量行、游标归零重扫重建；已删
/// 会话的孤儿行（统计保留的载体，墓碑会拦住重建）必须留下，其他工具不动。
#[test]
fn v19_deletes_linked_usage_and_resets_cursors_for_replay() {
    let (storage, path) = test_db("v19-replay");
    storage
        .conn
        .execute_batch(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                ('C:/cc/s.jsonl',  'claude-code', x'AB', 10, 10, 1),
                ('C:/cb/a.log',    'codebuddy',   x'CD', 10, 10, 1),
                ('C:/z/r.jsonl',   'zcode',       x'EE', 10, 10, 1);
             INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                (1, 'claude-code', 'cc-live',    1, 1),
                (2, 'claude-code', 'cc-deleted', 1, 1);
             INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key) VALUES
                (1,    'claude-code', 'm', 'm', 1, x'01'),
                (NULL, 'claude-code', 'm', 'm', 2, x'02'),
                (1,    'codebuddy',   'm', 'm', 3, x'03'),
                (1,    'zcode',       'm', 'm', 4, x'04');",
        )
        .unwrap();
    // 库已停在最高版本；回拨到 v18 重开，v19 起的迁移依次重跑（v21 会删
    // codex 行，所以"未涉及工具"用 zcode 验证）。
    storage
        .conn
        .execute("PRAGMA user_version = 18", [])
        .unwrap();
    drop(storage);

    let reopened = open(&path).unwrap();
    let rows: Vec<(String, Option<i64>)> = reopened
        .conn
        .prepare("SELECT tool, session_id FROM usage_records ORDER BY ts")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![("claude-code".into(), None), ("zcode".into(), Some(1)),],
        "在册行删除重建，孤儿行与未涉及工具保留"
    );
    let cursors: Vec<(String, i64)> = reopened
        .conn
        .prepare("SELECT tool, parsed_bytes FROM scanned_files ORDER BY path")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        cursors,
        vec![
            ("codebuddy".into(), 0),
            ("claude-code".into(), 0),
            ("zcode".into(), 10),
        ],
        "两工具游标归零，其他工具不动"
    );
    drop(reopened);
    cleanup(&path);
}

/// v21 回填 pi/codex 请求耗时：删两工具在册会话的用量行、游标归零重扫
/// 重建；已删会话的孤儿行（统计保留的载体）必须留下，其他工具不动
/// （v22 会删 grok 在册行，"未涉及工具"用 zcode 验证）。
#[test]
fn v21_deletes_linked_usage_and_resets_cursors_for_replay() {
    let (storage, path) = test_db("v21-replay");
    storage
        .conn
        .execute_batch(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                ('C:/pi/s.jsonl',    'pi',    x'AB', 10, 10, 1),
                ('C:/codex/r.jsonl', 'codex', x'CD', 10, 10, 1),
                ('C:/zcode/u.jsonl', 'zcode', x'EE', 10, 10, 1);
             INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                (1, 'pi', 'pi-live',    1, 1);
             INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key) VALUES
                (1,    'pi',    'm', 'm', 1, x'01'),
                (NULL, 'pi',    'm', 'm', 2, x'02'),
                (1,    'codex', 'm', 'm', 3, x'03'),
                (1,    'zcode', 'm', 'm', 4, x'04');",
        )
        .unwrap();
    // 库已停在最高版本；回拨到 v20 重开，只重跑 v21。
    storage
        .conn
        .execute("PRAGMA user_version = 20", [])
        .unwrap();
    drop(storage);

    let reopened = open(&path).unwrap();
    let rows: Vec<(String, Option<i64>)> = reopened
        .conn
        .prepare("SELECT tool, session_id FROM usage_records ORDER BY ts")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![("pi".into(), None), ("zcode".into(), Some(1)),],
        "在册行删除重建，孤儿行与未涉及工具保留"
    );
    let cursors: Vec<(String, i64)> = reopened
        .conn
        .prepare("SELECT tool, parsed_bytes FROM scanned_files ORDER BY path")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        cursors,
        vec![("codex".into(), 0), ("pi".into(), 0), ("zcode".into(), 10),],
        "两工具游标归零，其他工具不动"
    );
    drop(reopened);
    cleanup(&path);
}

/// v22 落行级请求数：usage_records 重建后 request_count 列生效（存量默认
/// 1）；grok 在册会话的用量行删除、游标归零待重扫拆分重建；孤儿行与
/// 其他工具不动。
#[test]
fn v22_adds_request_count_and_resets_grok_for_replay() {
    let (storage, path) = test_db("v22-request-count");
    storage
        .conn
        .execute_batch(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                ('C:/grok/u.jsonl',  'grok',  x'AB', 10, 10, 1),
                ('C:/codex/r.jsonl', 'codex', x'CD', 10, 10, 1);
             INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                (1, 'grok', 'grok-live', 1, 1);
             INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key) VALUES
                (1,    'grok',  'm', 'm', 1, x'01'),
                (NULL, 'grok',  'm', 'm', 2, x'02'),
                (1,    'codex', 'm', 'm', 3, x'03');",
        )
        .unwrap();
    // 库已停在最高版本；回拨到 v21 重开，只重跑 v22。
    storage
        .conn
        .execute("PRAGMA user_version = 21", [])
        .unwrap();
    drop(storage);

    let reopened = open(&path).unwrap();
    let rows: Vec<(String, Option<i64>, i64)> = reopened
        .conn
        .prepare("SELECT tool, session_id, request_count FROM usage_records ORDER BY ts")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![("grok".into(), None, 1), ("codex".into(), Some(1), 1),],
        "grok 在册行删除待重扫，孤儿行保留，存量请求数默认 1"
    );
    let cursor: i64 = reopened
        .conn
        .query_row(
            "SELECT parsed_bytes FROM scanned_files WHERE tool = 'grok'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor, 0, "grok 游标归零，重扫按请求级拆分重建");
    drop(reopened);
    cleanup(&path);
}

/// v23 回填 grok 请求耗时：与 v22 同款重建（dedup 键不变，旧行锁死），
/// 在册行删除待重扫、孤儿行保留、其他工具不动。
#[test]
fn v23_resets_grok_for_duration_replay() {
    let (storage, path) = test_db("v23-duration");
    storage
        .conn
        .execute_batch(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at) VALUES
                ('C:/grok/u.jsonl',  'grok',  x'AB', 10, 10, 1),
                ('C:/codex/r.jsonl', 'codex', x'CD', 10, 10, 1);
             INSERT INTO sessions (id, tool, external_id, source_file_id, first_seen_at) VALUES
                (1, 'grok', 'grok-live', 1, 1);
             INSERT INTO usage_records (session_id, tool, model_raw, model, ts, dedup_key, request_count) VALUES
                (1,    'grok',  'm', 'm', 1, x'01', 1),
                (NULL, 'grok',  'm', 'm', 2, x'02', 1),
                (1,    'codex', 'm', 'm', 3, x'03', 1);",
        )
        .unwrap();
    // 库已停在最高版本；回拨到 v22 重开，只重跑 v23。
    storage
        .conn
        .execute("PRAGMA user_version = 22", [])
        .unwrap();
    drop(storage);

    let reopened = open(&path).unwrap();
    let rows: Vec<(String, Option<i64>)> = reopened
        .conn
        .prepare("SELECT tool, session_id FROM usage_records ORDER BY ts")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![("grok".into(), None), ("codex".into(), Some(1)),],
        "grok 在册行删除待重扫，孤儿行与其他工具保留"
    );
    let cursor: i64 = reopened
        .conn
        .query_row(
            "SELECT parsed_bytes FROM scanned_files WHERE tool = 'grok'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursor, 0, "grok 游标归零，重扫回填推导耗时");
    drop(reopened);
    cleanup(&path);
}

/// v16 清洗 codebuddy 统计行吃进来的尾部 `)` 模型名：净名同指的脏映射删除、
/// 净名空缺的就地改名、指向分歧的保守保留；事实表只清 raw，model 列不动。
#[test]
fn v16_strips_trailing_paren_from_model_raws() {
    let (storage, path) = test_db("v16-paren");
    for id in ["hy4-preview", "solo-model", "model-a", "model-b", "orphan"] {
        storage
            .conn
            .execute(
                "INSERT INTO models (id, display_name, first_seen_at) VALUES (?1, ?1, 1)",
                params![id],
            )
            .unwrap();
    }
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, source)
             VALUES ('hy4-preview', 845000, 'catalog')",
            [],
        )
        .unwrap();
    for (raw, id, source) in [
        ("hy4-preview-f", "hy4-preview", "user"),
        ("hy4-preview-f)", "hy4-preview", "user"),
        ("divergent", "model-b", "user"),
        ("divergent)", "model-a", "user"),
        ("solo-raw)", "solo-model", "auto"),
    ] {
        storage
            .conn
            .execute(
                "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
                 VALUES (?1, ?2, ?3, 1)",
                params![raw, id, source],
            )
            .unwrap();
    }
    storage
        .conn
        .execute(
            "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
             VALUES ('codebuddy', 'hy4-preview-f)', 'hy4-preview', 1, x'01'),
                    ('codebuddy', 'hy4-preview-f', 'hy4-preview', 2, x'02')",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO gateway_requests (ts, tool, model_raw, model)
             VALUES (1, NULL, 'hy4-preview-f)', 'hy4-preview')",
            [],
        )
        .unwrap();

    // 库已停在最高版本；回拨到 v15 重开，只重跑 v16。
    storage
        .conn
        .execute("PRAGMA user_version = 15", [])
        .unwrap();
    drop(storage);

    let reopened = open(&path).unwrap();
    let usage: Vec<(String, String)> = reopened
        .conn
        .prepare("SELECT model_raw, model FROM usage_records ORDER BY ts")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        usage,
        [
            ("hy4-preview-f".to_string(), "hy4-preview".to_string()),
            ("hy4-preview-f".to_string(), "hy4-preview".to_string()),
        ],
        "脏 raw 清洗，model 列与净行不动"
    );
    let gateway: String = reopened
        .conn
        .query_row("SELECT model_raw FROM gateway_requests", [], |r| r.get(0))
        .unwrap();
    assert_eq!(gateway, "hy4-preview-f");
    let mappings: Vec<(String, String)> = reopened
        .conn
        .prepare("SELECT raw_model, model_id FROM model_mappings ORDER BY raw_model")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        mappings,
        vec![
            ("divergent".to_string(), "model-b".to_string()),
            ("divergent)".to_string(), "model-a".to_string()),
            ("hy4-preview-f".to_string(), "hy4-preview".to_string()),
            ("solo-raw".to_string(), "solo-model".to_string()),
        ],
        "同指脏行删除、无净名脏行改名、指向分歧保留"
    );
    let orphan: i64 = reopened
        .conn
        .query_row("SELECT COUNT(*) FROM models WHERE id = 'orphan'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(orphan, 0, "失去引用的空壳模型删除");
    let kept: i64 = reopened
        .conn
        .query_row(
            "SELECT COUNT(*) FROM models
             WHERE id IN ('hy4-preview','solo-model','model-a','model-b')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 4, "仍被引用或挂价的模型保留");
    drop(reopened);
    cleanup(&path);
}

#[test]
fn foreign_keys_are_enforced() {
    let (storage, path) = test_db("fk");
    let mut rec = record(Some(9_999), 1);
    rec.dedup_key = vec![1, 0, 0];
    assert!(storage.insert_usage_record(&rec).is_err());
    drop(storage);
    cleanup(&path);
}

#[test]
fn usage_insert_is_deduped_and_seeds_auto_mapping() {
    let (storage, path) = test_db("dedup");
    assert!(storage.insert_usage_record(&record(None, 1)).unwrap());
    assert!(!storage.insert_usage_record(&record(None, 1)).unwrap());

    let (model, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings
             WHERE raw_model = 'claude-sonnet-4-5-20250929'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, "claude-sonnet-4-5");
    assert_eq!(source, "auto");

    // 已有 auto 映射时，调用方建议值被映射覆盖——事实表的 model 与映射一致。
    let mut rec = record(None, 2);
    rec.model = "wrong-normalization".into();
    assert!(storage.insert_usage_record(&rec).unwrap());
    let got: String = storage
        .conn
        .query_row(
            "SELECT model FROM usage_records WHERE dedup_key = X'02'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(got, "claude-sonnet-4-5");
    drop(storage);
    cleanup(&path);
}

#[test]
fn delete_session_keeps_usage_and_shared_files() {
    let (storage, path) = test_db("delete");
    let file_a = storage
        .upsert_scanned_file("/logs/a.jsonl", "claude-code", &[0xAA], 10, 10, 0)
        .unwrap();
    let session = |external_id: &str, title: Option<&str>| NewSession {
        tool: "claude-code".into(),
        external_id: external_id.into(),
        external_id_is_derived: false,
        title: title.map(str::to_string),
        project_dir: None,
        source_file_id: file_a,
    };
    let (s1, created1) = storage
        .get_or_create_session(&session("sess-1", Some("t")), 0)
        .unwrap();
    assert!(created1);
    let (s2, created2) = storage
        .get_or_create_session(&session("sess-2", None), 0)
        .unwrap();
    assert!(created2);
    // 再取同一会话：命中已有行，不算新建。
    let (_, re_fetched) = storage
        .get_or_create_session(&session("sess-2", None), 0)
        .unwrap();
    assert!(!re_fetched);
    storage.insert_usage_record(&record(Some(s1), 1)).unwrap();
    storage.insert_usage_record(&record(Some(s2), 2)).unwrap();

    storage.delete_session(s1).unwrap();

    let orphan: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM usage_records WHERE session_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphan, 1, "删除会话后用量必须保留");
    let sessions: i64 = storage
        .conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 1, "会话行删除，不留墓碑");
    let files: i64 = storage
        .conn
        .query_row("SELECT COUNT(*) FROM scanned_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(files, 1, "s2 仍引用该文件，簿记行必须保留");

    storage.delete_session(s2).unwrap();
    let files: i64 = storage
        .conn
        .query_row("SELECT COUNT(*) FROM scanned_files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(files, 0, "最后一个引用者删除后文件簿记行消失");
    drop(storage);
    cleanup(&path);
}

#[test]
fn user_mapping_beats_auto_and_recompute_follows() {
    let (storage, path) = test_db("mapping");
    storage.insert_usage_record(&record(None, 1)).unwrap();

    // 用户把该原始名改归一化到一个未定价模型。
    storage
        .upsert_model_mapping("claude-sonnet-4-5-20250929", "some-unpriced-model", 2_000)
        .unwrap();
    storage
        .apply_model_mapping_changes(&["claude-sonnet-4-5-20250929"])
        .unwrap();

    let (model, cost): (String, Option<i64>) = storage
        .conn
        .query_row(
            "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, "some-unpriced-model", "事实表跟随 user 映射");
    assert_eq!(cost, None, "未定价模型总分必须是 NULL，绝不编造");

    // auto 无法覆盖 user。
    storage.insert_usage_record(&record(None, 3)).unwrap();
    let (model, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings
             WHERE raw_model = 'claude-sonnet-4-5-20250929'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, "some-unpriced-model");
    assert_eq!(source, "user");
    drop(storage);
    cleanup(&path);
}

#[test]
fn overview_filters_disabled_tools_without_touching_data() {
    let (storage, path) = test_db("overview-disabled");
    storage.insert_usage_record(&record(None, 1)).unwrap();
    // 另一个工具的记录。
    let mut other = record(None, 2);
    other.tool = "codex".into();
    other.dedup_key = vec![2];
    storage.insert_usage_record(&other).unwrap();

    let all = storage.overview(&[]).unwrap().totals;
    assert_eq!(all.record_count, 2);
    let filtered = storage.overview(&["claude-code".into()]).unwrap().totals;
    assert_eq!(filtered.record_count, 1, "禁用工具的记录不参与聚合");
    assert_eq!(filtered.input_tokens, 1_000, "只算 codex 那条");

    // 禁用不删数据：恢复后数字原样回来。
    assert_eq!(storage.overview(&[]).unwrap().totals.record_count, 2);
    drop(storage);
    cleanup(&path);
}

#[test]
fn overview_aggregates_known_cost_and_counts_unknown_rows() {
    let (storage, path) = test_db("overview");
    storage.insert_usage_record(&record(None, 1)).unwrap();
    let mut rec = record(None, 2);
    rec.dedup_key = vec![2];
    rec.model = "unpriced-model".into();
    rec.model_raw = "unpriced-raw".into();
    storage.insert_usage_record(&rec).unwrap();
    // model_prices 外键指向 models：先用量建档再挂价。
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    storage.recompute_costs().unwrap();

    let payload = storage.overview(&[]).unwrap();
    let totals = payload.totals;
    assert_eq!(totals.record_count, 2);
    assert_eq!(totals.session_count, 0, "record(None, …) 不建会话");
    assert_eq!(totals.input_tokens, 2_000);
    // 已知成本只含定价行；未知行数单独给，不混进总数。
    assert_eq!(totals.known_cost_micros, 10_500);
    assert_eq!(totals.unknown_cost_rows, 1);

    let by_model: Vec<(String, i64, i64)> = payload
        .by_model
        .iter()
        .map(|row| {
            (
                row.model.clone(),
                row.known_cost_micros,
                row.unknown_cost_rows,
            )
        })
        .collect();
    assert_eq!(
        by_model,
        vec![
            ("claude-sonnet-4-5".into(), 10_500, 0),
            ("unpriced-model".into(), 0, 1),
        ],
        "按已知成本降序"
    );
    drop(storage);
    cleanup(&path);
}

#[test]
fn dashboard_aggregates_slices_and_conserves_totals() {
    let (storage, path) = test_db("dashboard");

    // 会话归属：一个有项目、一个无项目（NULL → "" 哨兵）。usage_records 的
    // session 外键指向 sessions，先建归属再插记录。
    storage
        .conn
        .execute(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
             VALUES ('/f1', 'claude-code', x'01', 1, 0, 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO sessions (tool, external_id, external_id_is_derived, project_dir, source_file_id, first_seen_at)
             VALUES ('claude-code', 's1', 0, '/proj/a', 1, 1)",
            [],
        )
        .unwrap();
    let s1 = storage.conn.last_insert_rowid();
    storage
        .conn
        .execute(
            "INSERT INTO sessions (tool, external_id, external_id_is_derived, project_dir, source_file_id, first_seen_at)
             VALUES ('claude-code', 's2', 0, NULL, 1, 1)",
            [],
        )
        .unwrap();
    let s2 = storage.conn.last_insert_rowid();

    // ts 用固定锚点 + 相对偏移：守恒型断言不依赖测试机的时区。
    // - 会话 s1 两条（相隔 1h → 时长 1h），有价模型；
    // - 会话 s2 一条（单条 → 时长 0），未定价模型 + 另一个工具，落在次日。
    const T0: i64 = 1_700_000_000_000;
    let mut r1 = record(Some(s1), 11);
    r1.ts = T0;
    storage.insert_usage_record(&r1).unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    let mut r2 = record(Some(s1), 12);
    // 距 r1 十分钟（≤ 30 分钟空闲阈值）→ 与 r1 同一活跃段。
    r2.ts = T0 + 600_000;
    r2.input_tokens = 2_000;
    r2.output_tokens = 1_000;
    let mut r3 = record(Some(s2), 13);
    r3.ts = T0 + 86_400_000;
    r3.tool = "codex".into();
    r3.model = "unpriced-model".into();
    r3.model_raw = "unpriced-raw".into();
    storage.insert_usage_record(&r2).unwrap();
    storage.insert_usage_record(&r3).unwrap();
    storage.recompute_costs().unwrap();

    let filters = UsageFilters {
        time_start: Some(T0),
        time_end: Some(T0 + 3 * 86_400_000),
        ..UsageFilters::default()
    };
    let payload = storage.dashboard(&filters, &[]).unwrap();

    // 总量：已知成本 = 10_500 + 21_000（未定价行不计）；五桶合计 6_000。
    let totals = payload.totals;
    assert_eq!(totals.calls, 3);
    assert_eq!(totals.cost_micros, 31_500);
    assert_eq!(totals.tokens, 6_000);
    assert_eq!(totals.sessions, 2);
    assert_eq!(totals.projects, 1, "无项目的会话不算项目");
    assert_eq!(totals.active_tools, 2);
    assert_eq!(
        totals.active_duration_ms, 600_000,
        "s1 一段 [T0, T0+10min]；r3 单条记录贡献 0"
    );
    assert_eq!(totals.cache_read_tokens, 0);

    // 会话消耗切片：s1（有价行）入切片；s2 只有未定价行 → SUM 全 NULL，被
    // HAVING 排除（全部未定价的会话没有费用可画）。
    assert_eq!(payload.sessions.len(), 1);
    let session = &payload.sessions[0];
    assert_eq!(session.tool, "claude-code");
    assert_eq!(session.external_id, "s1");
    assert_eq!(session.project_dir, "/proj/a");
    assert_eq!(session.model, "claude-sonnet-4-5");
    assert_eq!(session.calls, 2);
    assert_eq!(session.cost_micros, 31_500);
    assert_eq!(session.tokens, 4_500, "五桶合计 6_000 − 未定价行 1_500");
    assert_eq!(session.start_ms, T0, "首条记录 r1");
    assert_eq!(session.end_ms, T0 + 600_000, "末条记录 r2");

    // 趋势：窗口 3 天 → 天粒度 4 桶；守恒断言与时区无关。
    assert_eq!(payload.trend_grain, TrendGrain::Day);
    assert_eq!(payload.trend.len(), 4);
    assert_eq!(payload.trend.iter().map(|b| b.calls).sum::<i64>(), 3);
    assert_eq!(
        payload.trend.iter().map(|b| b.cost_micros).sum::<i64>(),
        31_500
    );
    assert_eq!(payload.trend.iter().map(|b| b.tokens).sum::<i64>(), 6_000);
    // 未定价记录所在的桶：量照常、成本为 0。
    let rec3 = payload
        .trend
        .iter()
        .find(|b| b.calls == 1)
        .expect("r1/r2 同桶（2 次），r3 独占一桶");
    assert_eq!(rec3.cost_micros, 0);
    assert_eq!(rec3.tokens, 1_500);

    // 按模型趋势：模型数与构成一致，数组与 trend 桶对齐，总量守恒。
    assert_eq!(payload.model_trend.len(), 2);
    let claude = &payload.model_trend[0];
    assert_eq!(claude.model, "claude-sonnet-4-5");
    assert_eq!(claude.cost_micros.len(), payload.trend.len());
    assert_eq!(claude.cost_micros.iter().sum::<i64>(), 31_500);
    assert_eq!(claude.calls.iter().sum::<i64>(), 2);

    // 构成：已知成本降序；未定价模型标注 unknown。
    assert_eq!(
        payload
            .composition
            .iter()
            .map(|r| (r.label.as_str(), r.cost_micros, r.calls, r.unknown_cost))
            .collect::<Vec<_>>(),
        vec![
            ("claude-sonnet-4-5", 31_500, 2, false),
            ("unpriced-model", 0, 1, true),
        ]
    );
    assert_eq!(payload.unknown_model_count, 1);

    // 项目构成："" 是无项目哨兵。
    assert_eq!(
        payload
            .projects
            .iter()
            .map(|r| (r.label.as_str(), r.cost_micros, r.calls))
            .collect::<Vec<_>>(),
        vec![("/proj/a", 31_500, 2), ("", 0, 1)]
    );

    // 流向：工具 → 模型、模型 → 项目两段；未定价段成本 0 但量照常。
    let flow: Vec<(String, String, i64, i64)> = payload
        .flow
        .iter()
        .map(|l| (l.source.clone(), l.target.clone(), l.cost_micros, l.calls))
        .collect();
    assert_eq!(flow.len(), 4);
    assert!(flow.contains(&("claude-code".into(), "claude-sonnet-4-5".into(), 31_500, 2)));
    assert!(flow.contains(&("codex".into(), "unpriced-model".into(), 0, 1)));
    assert!(flow.contains(&("claude-sonnet-4-5".into(), "/proj/a".into(), 31_500, 2)));
    assert!(flow.contains(&("unpriced-model".into(), String::new(), 0, 1)));

    // 活跃热力图：恒 168 格，量与成本守恒。
    assert_eq!(payload.activity.len(), 168);
    assert_eq!(payload.activity.iter().map(|c| c.calls).sum::<i64>(), 3);
    assert_eq!(
        payload.activity.iter().map(|c| c.cost_micros).sum::<i64>(),
        31_500
    );

    // 日历：不吃时间筛选（两天各一行、日期升序）；守恒。
    assert_eq!(payload.daily.len(), 2);
    assert!(payload.daily.windows(2).all(|w| w[0].date < w[1].date));
    assert_eq!(payload.daily.iter().map(|d| d.calls).sum::<i64>(), 3);
    assert_eq!(
        payload.daily.iter().map(|d| d.cost_micros).sum::<i64>(),
        31_500
    );

    // 箱线图：只含有价模型；两样本 [10_500, 21_000] 的线性插值五数概括。
    assert_eq!(payload.spread.len(), 1);
    assert_eq!(payload.spread[0].label, "claude-sonnet-4-5");
    assert_eq!(
        payload.spread[0].cost_micros,
        vec![10_500, 13_125, 15_750, 18_375, 21_000]
    );

    // 瀑布：分项之和精确等于已知总成本。
    let cc = payload.cost_composition;
    assert_eq!(cc.input_cost_micros, 9_000);
    assert_eq!(cc.output_cost_micros, 22_500);
    assert_eq!(cc.reasoning_cost_micros, 0, "record() 不带推理量");
    assert_eq!(cc.cache_read_cost_micros, 0);
    assert_eq!(cc.cache_write_cost_micros, 0);
    assert_eq!(
        cc.input_cost_micros
            + cc.output_cost_micros
            + cc.reasoning_cost_micros
            + cc.cache_read_cost_micros
            + cc.cache_write_cost_micros,
        31_500
    );

    drop(storage);
    cleanup(&path);
}

#[test]
fn dashboard_empty_database_returns_zero_payload() {
    let (storage, path) = test_db("dashboard-empty");
    let payload = storage.dashboard(&UsageFilters::default(), &[]).unwrap();
    assert_eq!(payload.totals.calls, 0);
    assert!(payload.trend.is_empty());
    assert!(payload.composition.is_empty());
    assert!(payload.daily.is_empty());
    assert_eq!(payload.activity.len(), 168, "热力图按网格画，空数据补 0");
    drop(storage);
    cleanup(&path);
}

#[test]
fn dashboard_filters_and_disabled_narrow_slices() {
    let (storage, path) = test_db("dashboard-filters");
    storage
        .conn
        .execute(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
             VALUES ('/f1', 'claude-code', x'01', 1, 0, 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO sessions (tool, external_id, external_id_is_derived, project_dir, source_file_id, first_seen_at)
             VALUES ('claude-code', 's1', 0, '/proj/a', 1, 1)",
            [],
        )
        .unwrap();
    let s1 = storage.conn.last_insert_rowid();
    let r1 = record(Some(s1), 21);
    let mut r2 = record(None, 22);
    r2.tool = "codex".into();
    storage.insert_usage_record(&r1).unwrap();
    storage.insert_usage_record(&r2).unwrap();

    // 工具筛选只留有会话归属的那条。
    let by_tool = storage
        .dashboard(
            &UsageFilters {
                tools: vec!["claude-code".into()],
                ..UsageFilters::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(by_tool.totals.calls, 1);
    assert_eq!(by_tool.totals.projects, 1);

    // 项目筛选的 "" 哨兵：孤儿行（无会话 → project_dir IS NULL）。
    let by_none = storage
        .dashboard(
            &UsageFilters {
                projects: vec![String::new()],
                ..UsageFilters::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(by_none.totals.calls, 1);
    assert_eq!(by_none.projects.len(), 1);
    assert_eq!(by_none.projects[0].label, "");

    // 禁用工具与筛选同效，但数据原样保留：恢复后数字回来。
    let disabled = storage
        .dashboard(&UsageFilters::default(), &["codex".to_string()])
        .unwrap();
    assert_eq!(disabled.totals.calls, 1);
    assert_eq!(
        storage
            .dashboard(&UsageFilters::default(), &[])
            .unwrap()
            .totals
            .calls,
        2
    );
    drop(storage);
    cleanup(&path);
}

// 前端"今天"预设的窗口（本地 0 点起、跨度不足 2 天）会命中 Hour 粒度分支，
// 与默认 7 天的 Day 粒度走的是不同 SQL/桶序列，这里单独覆盖。
#[test]
fn dashboard_today_window_uses_hour_grain() {
    let (storage, path) = test_db("dashboard-today");
    let mut r = record(None, 31);
    r.ts = Local::now().timestamp_millis();
    storage.insert_usage_record(&r).unwrap();

    // 与前端 presetRange("today") 同窗口：本地 0 点起、当日 23:59:59.999 止。
    let now = Local::now();
    let today_start = Local
        .with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0)
        .single()
        .expect("本地 0 点是唯一时刻")
        .timestamp_millis();
    let payload = storage
        .dashboard(
            &UsageFilters {
                time_start: Some(today_start),
                time_end: Some(today_start + 86_400_000 - 1),
                ..UsageFilters::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(payload.trend_grain, TrendGrain::Hour);
    assert_eq!(payload.trend.len(), 24);
    assert_eq!(payload.totals.calls, 1);
    // 趋势守恒：唯一一条记录恰好落进一个桶。
    assert_eq!(
        payload.trend.iter().map(|b| b.cost_micros).sum::<i64>(),
        payload.totals.cost_micros
    );
    drop(storage);
    cleanup(&path);
}

#[test]
fn union_duration_merges_overlaps_and_clips() {
    // 重叠合并为一段。
    assert_eq!(union_duration_ms(vec![(0, 100), (50, 150)], 0, 1_000), 150);
    // 裁剪到窗口：窗口外部分不计入。
    assert_eq!(union_duration_ms(vec![(-100, 50), (80, 200)], 0, 100), 70);
    // 端点相接的区间合并；并行会话因此不重复计时。
    assert_eq!(union_duration_ms(vec![(0, 10), (10, 20)], 0, 100), 20);
    // 单点区间（一次调用）与空输入、倒挂窗口均为 0。
    assert_eq!(union_duration_ms(vec![(5, 5)], 0, 100), 0);
    assert_eq!(union_duration_ms(vec![], 0, 100), 0);
    assert_eq!(union_duration_ms(vec![(10, 20)], 30, 10), 0);
}

#[test]
fn recompute_prices_cache_buckets_fall_back_to_input_price() {
    let (storage, path) = test_db("reprice");
    // 先有用量（它会给 models 建档），再挂价格——model_prices 外键指向 models。
    storage.insert_usage_record(&record(None, 1)).unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    storage.recompute_costs().unwrap();

    // input 1_000 × 3_000_000 = 3_000；output 500 × 15_000_000 = 7_500。
    let (input, output, total): (i64, i64, i64) = storage
        .conn
        .query_row(
            "SELECT input_cost_micros, output_cost_micros, cost_micros
             FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((input, output, total), (3_000, 7_500, 10_500));

    // 缓存桶无单价但有量 → 按输入价计：10 × 3_000_000 = 30，总分不再未知。
    storage
        .conn
        .execute(
            "UPDATE usage_records SET cache_write_tokens = 10 WHERE dedup_key = X'01'",
            [],
        )
        .unwrap();
    storage.recompute_costs().unwrap();
    let (input, cache_write, total): (i64, i64, i64) = storage
        .conn
        .query_row(
            "SELECT input_cost_micros, cache_write_cost_micros, cost_micros
             FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(cache_write, 30, "cache_write 无单价按输入价计");
    assert_eq!(total, 10_530, "缓存回落输入价后总分已知");
    assert_eq!(input, 3_000, "输入桶不受影响");

    // 输入价也缺：输入与缓存两桶一起 NULL、总分未知；已定价的输出分项保留。
    storage
        .conn
        .execute(
            "UPDATE model_prices SET input_per_mtok_micros = NULL
             WHERE model_id = 'claude-sonnet-4-5'",
            [],
        )
        .unwrap();
    storage.recompute_costs().unwrap();
    let (input, cache_write, output, total): (Option<i64>, Option<i64>, Option<i64>, Option<i64>) =
        storage
            .conn
            .query_row(
                "SELECT input_cost_micros, cache_write_cost_micros,
                        output_cost_micros, cost_micros
                 FROM v_usage_cost WHERE dedup_key = X'01'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
    assert_eq!(input, None, "输入价缺失，输入桶 NULL");
    assert_eq!(cache_write, None, "输入价也缺时缓存桶才 NULL");
    assert_eq!(output, Some(7_500), "已定价桶的分项不受其它桶连坐");
    assert_eq!(total, None, "有量桶未定价，总分必须 NULL");
    drop(storage);
    cleanup(&path);
}

#[test]
fn reasoning_tokens_are_billed_as_output() {
    let (storage, path) = test_db("reasoning");
    storage.insert_usage_record(&record(None, 1)).unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "UPDATE usage_records SET reasoning_tokens = 200 WHERE dedup_key = X'01'",
            [],
        )
        .unwrap();
    storage.recompute_costs().unwrap();

    // (output 500 + reasoning 200) × 15_000_000 / 1e6 = 10_500；总分 = 分项之和。
    let (output, total): (i64, i64) = storage
        .conn
        .query_row(
            "SELECT output_cost_micros, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(output, 10_500, "推理量并入输出桶计价");
    assert_eq!(total, 3_000 + 10_500, "总分 = 分项之和");
    drop(storage);
    cleanup(&path);
}

fn gateway_request(model_raw: &str, model: &str, tool: Option<&str>) -> NewGatewayRequest {
    NewGatewayRequest {
        tool: tool.map(str::to_string),
        model_raw: model_raw.into(),
        model: model.into(),
        ts: 1_000,
        input_tokens: 1_000,
        output_tokens: 500,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        status_code: Some(200),
        latency_ms: Some(10),
        upstream: Some("mock".into()),
        token_hash: Some(vec![7u8; 32]),
    }
}

#[test]
fn gateway_insert_costs_in_the_same_transaction() {
    let (storage, path) = test_db("gateway-price");
    // 映射建档 + 挂价发生在入库前：insert 返回时成本必须已经落库（单事务）。
    storage
        .upsert_model_mapping("raw-x", "claude-sonnet-4-5", 0)
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();

    let request = gateway_request("raw-x", "wrong-suggestion", Some("codex"));
    let id = storage.insert_gateway_request(&request).unwrap();

    let (model, input, output, total): (String, i64, i64, Option<i64>) = storage
        .conn
        .query_row(
            "SELECT model, input_cost_micros, output_cost_micros, cost_micros
             FROM v_gateway_cost WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(model, "claude-sonnet-4-5", "已有映射时建议值被覆盖");
    assert_eq!(
        (input, output),
        (3_000, 7_500),
        "成本在 insert 的同一事务内落库"
    );
    assert_eq!(total, Some(10_500));

    let payload = storage.gateway_overview().unwrap();
    assert_eq!(payload.totals.request_count, 1);
    assert_eq!(payload.totals.known_cost_micros, 10_500);
    assert_eq!(payload.totals.unknown_cost_rows, 0);
    assert_eq!(payload.totals.input_tokens, 1_000);
    drop(storage);
    cleanup(&path);
}

#[test]
fn gateway_unpriced_model_stays_null_and_is_reported_unknown() {
    let (storage, path) = test_db("gateway-null");
    let request = gateway_request("never-priced", "never-priced", None);
    storage.insert_gateway_request(&request).unwrap();

    let costs: (Option<i64>, Option<i64>, Option<i64>) = storage
        .conn
        .query_row(
            "SELECT input_cost_micros, output_cost_micros, cost_micros
             FROM v_gateway_cost",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(costs, (None, None, None), "无价格绝不编造数字");

    let payload = storage.gateway_overview().unwrap();
    assert_eq!(payload.totals.known_cost_micros, 0);
    assert_eq!(payload.totals.unknown_cost_rows, 1);
    drop(storage);
    cleanup(&path);
}

#[test]
fn mapping_change_rewrites_gateway_rows_and_reprices() {
    let (storage, path) = test_db("gateway-remap");
    storage
        .upsert_model_mapping("raw-x", "claude-sonnet-4-5", 0)
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    storage
        .insert_gateway_request(&gateway_request(
            "raw-x",
            "claude-sonnet-4-5",
            Some("codex"),
        ))
        .unwrap();

    // 用户把映射改到新模型：同事务改写 gateway_requests 并按新模型重算。
    storage
        .upsert_model_mapping("raw-x", "claude-opus-4-1", 1)
        .unwrap();
    let changed = storage.apply_model_mapping_changes(&["raw-x"]).unwrap();
    assert!(changed > 0);

    let model: String = storage
        .conn
        .query_row("SELECT model FROM gateway_requests", [], |row| row.get(0))
        .unwrap();
    assert_eq!(model, "claude-opus-4-1");
    // 新模型无价格行：分项被清 NULL，走"未知"而不是沿用旧价。
    let input: Option<i64> = storage
        .conn
        .query_row(
            "SELECT input_cost_micros FROM gateway_requests",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(input, None);
    drop(storage);
    cleanup(&path);
}

fn catalog_entry(
    provider: &str,
    model_id: &str,
    status: Option<&str>,
    input: Option<f64>,
    output: Option<f64>,
) -> crate::pricing::catalog::CatalogEntry {
    crate::pricing::catalog::CatalogEntry {
        provider: provider.to_string(),
        model_id: model_id.to_string(),
        display_name: None,
        status: status.map(str::to_string),
        input_per_mtok_micros: input.map(|v| (v * 1_000_000.0) as i64),
        output_per_mtok_micros: output.map(|v| (v * 1_000_000.0) as i64),
        cache_read_per_mtok_micros: None,
        cache_write_per_mtok_micros: None,
        reasoning_per_mtok_micros: None,
    }
}

#[test]
fn catalog_sync_fills_missing_and_respects_higher_sources() {
    let (storage, path) = test_db("catalog");
    // 建档：claude-sonnet-4-5（record 建议值）与 glm-4.6（经映射建档）。
    storage.insert_usage_record(&record(None, 1)).unwrap();
    storage
        .upsert_model_mapping("raw-glm", "glm-4.6", 1)
        .unwrap();

    // 同 id 两供应商：deprecated 的 azure 输给在售的 anthropic。
    storage
        .replace_catalog_entries(
            &[
                catalog_entry(
                    "anthropic",
                    "claude-sonnet-4-5",
                    None,
                    Some(3.0),
                    Some(15.0),
                ),
                catalog_entry(
                    "azure",
                    "claude-sonnet-4-5",
                    Some("deprecated"),
                    Some(9.9),
                    Some(9.9),
                ),
                // 目录没给 cost：只进快照，绝不估价。
                catalog_entry("zhipuai", "glm-4.6", None, None, None),
            ],
            100,
        )
        .unwrap();

    let (matched, priced, remapped) = storage.sync_catalog_prices().unwrap();
    assert_eq!(matched, 2, "glm-4.6 也在目录里（匹配），只是没价");
    assert_eq!(priced, 1);
    assert_eq!(remapped, 0, "映射已指向目录模型，无需补指");
    let (source, input): (String, i64) = storage
        .conn
        .query_row(
            "SELECT source, input_per_mtok_micros FROM model_prices
             WHERE model_id = 'claude-sonnet-4-5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((source.as_str(), input), ("catalog", 3_000_000));
    let glm_price: Option<i64> = storage
        .conn
        .query_row(
            "SELECT input_per_mtok_micros FROM model_prices WHERE model_id = 'glm-4.6'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert_eq!(glm_price, None, "目录没价绝不估价");

    // 用户改价后，目录刷新（价格变了）不覆盖 user 来源。
    storage
        .set_model_price(
            "claude-sonnet-4-5",
            Some(1_000_000),
            Some(2_000_000),
            None,
            None,
        )
        .unwrap();
    storage
        .replace_catalog_entries(
            &[catalog_entry(
                "anthropic",
                "claude-sonnet-4-5",
                None,
                Some(4.0),
                Some(4.0),
            )],
            200,
        )
        .unwrap();
    let (_, priced, _) = storage.sync_catalog_prices().unwrap();
    assert_eq!(priced, 0, "user 价目录不碰");
    let (source, input): (String, i64) = storage
        .conn
        .query_row(
            "SELECT source, input_per_mtok_micros FROM model_prices
             WHERE model_id = 'claude-sonnet-4-5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((source.as_str(), input), ("user", 1_000_000));
    // 挂价即算：record 是 1000 in / 500 out，微美元 = tokens × 单价 / 1e6。
    let total: i64 = storage
        .conn
        .query_row(
            "SELECT cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(total, 1_000 + 1_000, "set_model_price 后自动重算");
    drop(storage);
    cleanup(&path);
}

#[test]
fn set_model_mapping_repoints_facts_and_reprices() {
    let (storage, path) = test_db("remap-global");
    storage.insert_usage_record(&record(None, 1)).unwrap();
    storage
        .set_model_price(
            "claude-opus-4-1",
            Some(1_000_000),
            Some(1_000_000),
            None,
            None,
        )
        .unwrap();

    // 全局映射：record 的 raw 名改指 claude-opus-4-1，改写与重算同事务完成。
    let changed = storage
        .set_model_mapping("claude-sonnet-4-5-20250929", "claude-opus-4-1", 100)
        .unwrap();
    assert!(changed > 0);
    let (model, source, cost): (String, String, i64) = storage
        .conn
        .query_row(
            "SELECT r.model, m.source, v.cost_micros
             FROM usage_records AS r
             JOIN model_mappings AS m ON m.raw_model = r.model_raw
             JOIN v_usage_cost AS v ON v.id = r.id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(model, "claude-opus-4-1");
    assert_eq!(source, "user");
    assert_eq!(cost, 1_000 + 500);
    drop(storage);
    cleanup(&path);
}

#[test]
fn ingest_resolves_via_catalog_with_heuristic_suggestions() {
    let (storage, path) = test_db("resolve");
    // 先放目录：deepseek-v4-flash（归一精确命中）与 gemini-2.5-pro（启发候选命中）。
    storage
        .replace_catalog_entries(
            &[
                catalog_entry("deepseek", "deepseek-v4-flash", None, Some(1.0), Some(2.0)),
                catalog_entry("google", "gemini-2.5-pro", None, Some(1.0), Some(2.0)),
            ],
            100,
        )
        .unwrap();

    // 空格写法归一后精确命中目录 → auto。
    let mut spacey = record(None, 1);
    spacey.model_raw = "DeepSeek V4 Flash".into();
    spacey.model = "DeepSeek V4 Flash".into();
    storage.insert_usage_record(&spacey).unwrap();

    // 日期尾缀是启发候选 → suggested（猜的，待用户过目）。
    let mut dated = record(None, 2);
    dated.model_raw = "gemini-2.5-pro-06-05".into();
    dated.model = "gemini-2.5-pro-06-05".into();
    storage.insert_usage_record(&dated).unwrap();

    let (id, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings
             WHERE raw_model = 'DeepSeek V4 Flash'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (id.as_str(), source.as_str()),
        ("deepseek-v4-flash", "auto")
    );

    let (id, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings
             WHERE raw_model = 'gemini-2.5-pro-06-05'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (id.as_str(), source.as_str()),
        ("gemini-2.5-pro", "suggested")
    );
    drop(storage);
    cleanup(&path);
}

#[test]
fn sync_remaps_orphans_via_heuristic_and_confirm_pins() {
    let (storage, path) = test_db("remap-sync");
    // 直插 SQL 复刻遗留状态：自映射壳 raw → 自己（新入库路径已会剥尾缀）。
    storage
        .conn
        .execute(
            "INSERT INTO models (id, display_name, first_seen_at)
             VALUES ('gpt-4-0613', 'gpt-4-0613', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
             VALUES ('gpt-4-0613', 'gpt-4-0613', 'auto', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO usage_records (tool, model_raw, model, ts,
                 input_tokens, output_tokens, dedup_key)
             VALUES ('claude-code', 'gpt-4-0613', 'gpt-4-0613', 1, 1000, 500, X'01')",
            [],
        )
        .unwrap();

    // 目录同步带来 gpt-4：启发候选命中 → 映射改指（suggested）+ 事实改写 + 挂价重算。
    let payload = storage
        .apply_catalog(&[catalog_entry(
            "openai",
            "gpt-4",
            None,
            Some(30.0),
            Some(60.0),
        )])
        .unwrap();
    assert_eq!(payload.remapped, 1);
    assert_eq!(payload.priced, 1);

    let (id, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings WHERE raw_model = 'gpt-4-0613'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((id.as_str(), source.as_str()), ("gpt-4", "suggested"));
    let (model, cost): (String, Option<i64>) = storage
        .conn
        .query_row(
            "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, "gpt-4", "事实表跟着映射改写");
    assert_eq!(cost, Some(60_000), "30 USD/M × 1000 + 60 USD/M × 500");

    // 确认：suggested → user；之后目录刷新价，映射与价刷新照常，来源不再变。
    assert_eq!(storage.confirm_model("gpt-4", 300).unwrap(), 1);
    let payload = storage
        .apply_catalog(&[catalog_entry(
            "openai",
            "gpt-4",
            None,
            Some(31.0),
            Some(60.0),
        )])
        .unwrap();
    assert_eq!(payload.remapped, 0, "user 与已指向目录的映射不再补指");
    let source: String = storage
        .conn
        .query_row(
            "SELECT source FROM model_mappings WHERE raw_model = 'gpt-4-0613'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(source, "user", "确认钉住，同步不回退");
    let cost: i64 = storage
        .conn
        .query_row(
            "SELECT cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cost, 61_000, "catalog 价刷新后重算");
    drop(storage);
    cleanup(&path);
}

#[test]
fn ingest_strips_free_suffix_without_catalog() {
    let (storage, path) = test_db("free-strip");
    // 目录为空：修饰尾缀照样剥到母型号，标 suggested 待用户过目。
    let mut free = record(None, 1);
    free.model_raw = "mimo-v2.5-free".into();
    free.model = "mimo-v2.5-free".into();
    storage.insert_usage_record(&free).unwrap();
    let (id, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings WHERE raw_model = 'mimo-v2.5-free'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((id.as_str(), source.as_str()), ("mimo-v2.5", "suggested"));
    drop(storage);
    cleanup(&path);
}

#[test]
fn normalize_merges_legacy_self_shells_but_respects_user() {
    let (storage, path) = test_db("normalize");
    // 遗留自映射壳：老版本建档 raw → 自己（auto），用量也挂在壳下。
    // 当前的入库路径已会归一，只能直插 SQL 复刻老库状态。
    storage
        .conn
        .execute(
            "INSERT INTO models (id, display_name, first_seen_at)
             VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
             VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 'auto', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
             VALUES ('claude-code', 'z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1, X'01')",
            [],
        )
        .unwrap();
    // 第二类遗留：标准名自带 -free 尾缀（老版本没有剥尾缀逻辑）。
    storage
        .conn
        .execute(
            "INSERT INTO models (id, display_name, first_seen_at)
             VALUES ('mimo-v2.5-free', 'mimo-v2.5-free', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
             VALUES ('mimo-v2.5-free', 'mimo-v2.5-free', 'auto', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
             VALUES ('zcode', 'mimo-v2.5-free', 'mimo-v2.5-free', 1, X'02')",
            [],
        )
        .unwrap();

    // user 自映射是用户意志：不许动。
    storage
        .upsert_model_mapping("custom-x", "custom-x", 1)
        .unwrap();

    assert_eq!(storage.normalize_auto_mappings().unwrap(), 2);

    let (model_id, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings
             WHERE raw_model = 'z-ai/glm-5.3-flash'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (model_id.as_str(), source.as_str()),
        ("glm-5.3-flash", "auto")
    );
    let fact_model: String = storage
        .conn
        .query_row(
            "SELECT model FROM usage_records WHERE dedup_key = X'01'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fact_model, "glm-5.3-flash", "事实表跟着改写");
    let (free_id, free_source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings WHERE raw_model = 'mimo-v2.5-free'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (free_id.as_str(), free_source.as_str()),
        ("mimo-v2.5", "suggested"),
        "free 尾缀剥到母型号，标启发"
    );
    let shell: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM models WHERE id = 'z-ai/glm-5.3-flash'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(shell, 0, "空壳模型行删除");
    let user_shell: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM models WHERE id = 'custom-x'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(user_shell, 1, "user 自映射与它的模型行保留");
    // 幂等：再跑一遍不再有改动。
    assert_eq!(storage.normalize_auto_mappings().unwrap(), 0);
    drop(storage);
    cleanup(&path);
}

#[test]
fn sync_repoints_legacy_self_shell_without_catalog_evidence() {
    let (storage, path) = test_db("remap-self");
    // 直插 SQL 复刻老库的自映射壳（当前入库路径已会归一，造不出这种行）。
    storage
        .conn
        .execute(
            "INSERT INTO models (id, display_name, first_seen_at)
             VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_mappings (raw_model, model_id, source, updated_at)
             VALUES ('z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 'auto', 1)",
            [],
        )
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO usage_records (tool, model_raw, model, ts, dedup_key)
             VALUES ('claude-code', 'z-ai/glm-5.3-flash', 'z-ai/glm-5.3-flash', 1, X'01')",
            [],
        )
        .unwrap();

    let payload = storage
        .apply_catalog(&[catalog_entry(
            "zhipuai",
            "glm-5.3-flash",
            None,
            Some(2.0),
            Some(8.0),
        )])
        .unwrap();
    assert_eq!(payload.remapped, 1, "自映射壳借格式归一改指");
    assert_eq!(payload.priced, 1);
    let (model_id, source): (String, String) = storage
        .conn
        .query_row(
            "SELECT model_id, source FROM model_mappings
             WHERE raw_model = 'z-ai/glm-5.3-flash'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (model_id.as_str(), source.as_str()),
        ("glm-5.3-flash", "auto")
    );
    drop(storage);
    cleanup(&path);
}

#[test]
fn merge_models_moves_mappings_facts_and_price() {
    let (storage, path) = test_db("merge");
    storage.insert_usage_record(&record(None, 1)).unwrap();
    // raw 名现指 a-model；b-model 已有 user 价。
    storage
        .set_model_mapping("claude-sonnet-4-5-20250929", "a-model", 100)
        .unwrap();
    storage
        .set_model_price("b-model", Some(2_000_000), None, None, None)
        .unwrap();

    storage.merge_models("a-model", "b-model").unwrap();

    let (model, cost): (String, Option<i64>) = storage
        .conn
        .query_row(
            "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, "b-model", "事实表改写到并入方");
    assert_eq!(cost, None, "并入方已有的价保留，不是 a-model 的");
    let mapping_target: String = storage
        .conn
        .query_row(
            "SELECT model_id FROM model_mappings
             WHERE raw_model = 'claude-sonnet-4-5-20250929'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mapping_target, "b-model");
    let a_left: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM models WHERE id = 'a-model'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(a_left, 0, "被并入的壳必须消失");
    drop(storage);
    cleanup(&path);
}

#[test]
fn rename_model_merges_into_catalog_name_and_fills_price() {
    let (storage, path) = test_db("rename");
    // 本地模型 hy4-preview-f：一条用量 + 一个自映射变种，无价格行。
    let mut rec = record(None, 1);
    rec.model_raw = "hy4-preview-f".into();
    rec.model = "hy4-preview-f".into();
    storage.insert_usage_record(&rec).unwrap();
    storage
        .set_model_mapping("hy4-preview-f", "hy4-preview-f", 100)
        .unwrap();
    // 目录快照里有目标规范名（带价），同步从未跑过也照补。
    storage
        .replace_catalog_entries(
            &[catalog_entry(
                "zhipuai",
                "glm-5.3",
                None,
                Some(1.0),
                Some(2.0),
            )],
            200,
        )
        .unwrap();

    storage.rename_model("hy4-preview-f", "glm-5.3").unwrap();

    let (model, cost): (String, Option<i64>) = storage
        .conn
        .query_row(
            "SELECT model, cost_micros FROM v_usage_cost WHERE dedup_key = X'01'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(model, "glm-5.3", "事实改写到新标准名");
    assert_eq!(
        cost,
        Some(1_000 + 1_000),
        "改名后目录价立即生效：1000×$1/M + 500×$2/M"
    );
    let mapping: String = storage
        .conn
        .query_row(
            "SELECT model_id FROM model_mappings WHERE raw_model = 'hy4-preview-f'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mapping, "glm-5.3");
    let shell: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM models WHERE id = 'hy4-preview-f'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(shell, 0, "旧名的壳删除");
    drop(storage);
    cleanup(&path);
}

#[test]
fn pricing_overview_links_catalog_by_model_id_and_lists_candidates() {
    let (storage, path) = test_db("pricing-overview");
    let mut rec = record(None, 1);
    rec.model_raw = "hy4-preview-f".into();
    rec.model = "hy4-preview-f".into();
    storage.insert_usage_record(&rec).unwrap();
    storage
        .set_model_mapping("hy4-preview-f", "hy4-preview-f", 100)
        .unwrap();
    // provider 与 model_id 完全不同：目录若错按 provider 键关联必然 miss。
    storage
        .replace_catalog_entries(
            &[catalog_entry(
                "zhipuai",
                "hy4-preview-f",
                None,
                Some(1.0),
                Some(2.0),
            )],
            200,
        )
        .unwrap();

    let payload = storage.pricing_overview().unwrap();

    let row = payload
        .models
        .iter()
        .find(|m| m.id == "hy4-preview-f")
        .expect("本地模型在列表里");
    let hint = row.catalog.as_ref().expect("目录按 model_id 关联");
    assert_eq!(hint.provider, "zhipuai");
    let brief = payload
        .catalog_models
        .iter()
        .find(|b| b.model_id == "hy4-preview-f")
        .expect("改名候选里是真实的目录模型 id");
    assert_eq!(brief.provider, "zhipuai");
    drop(storage);
    cleanup(&path);
}

#[test]
fn gateway_requests_page_orders_desc_and_counts_errors() {
    let (storage, path) = test_db("gateway-page");
    let mut request = gateway_request("raw-x", "claude-sonnet-4-5", None);
    storage
        .upsert_model_mapping("raw-x", "claude-sonnet-4-5", 0)
        .unwrap();
    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    storage.insert_gateway_request(&request).unwrap();
    // 第二条：更晚的时间、非成功状态。
    request.ts = 2_000;
    request.status_code = Some(502);
    request.model_raw = "raw-y".into();
    request.model = "raw-y".into(); // tool=None：建议值直接生效，raw-y 无价格。
    storage.insert_gateway_request(&request).unwrap();

    let totals = storage.gateway_overview().unwrap().totals;
    assert_eq!(totals.request_count, 2);
    assert_eq!(totals.error_count, 1, "502 计入错误数");

    let page = storage.gateway_requests_page(0, 1).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].ts, 2_000, "时间倒序");
    assert_eq!(page.rows[0].status_code, Some(502));
    assert_eq!(page.rows[0].cost_micros, None, "raw-y 无价格，成本未知");

    let page2 = storage.gateway_requests_page(1, 1).unwrap();
    assert_eq!(page2.rows.len(), 1);
    assert_eq!(page2.rows[0].ts, 1_000);
    assert_eq!(
        page2.rows[0].cost_micros,
        Some(3_000 + 7_500),
        "已有价格行的成本走视图"
    );
    drop(storage);
    cleanup(&path);
}

/// 明细页数据：会话内定价记录（ts 1000，四桶 1000/500/0/0，耗时 1500ms）
/// + 孤儿未定价记录（ts 2000，tool codex，tokens 10/5，无会话无耗时）。
fn usage_page_fixture(storage: &Storage) {
    let file_id = storage
        .upsert_scanned_file("/logs/a.jsonl", "claude-code", &[0xAA], 10, 10, 0)
        .unwrap();
    let (session_id, _) = storage
        .get_or_create_session(
            &NewSession {
                tool: "claude-code".into(),
                external_id: "sess-1".into(),
                external_id_is_derived: false,
                title: None,
                project_dir: Some("E:/p".into()),
                source_file_id: file_id,
            },
            0,
        )
        .unwrap();

    let mut rec = record(Some(session_id), 1);
    rec.ts = 1_000;
    rec.duration_ms = Some(1_500);
    storage.insert_usage_record(&rec).unwrap();

    let mut orphan = record(None, 2);
    orphan.tool = "codex".into();
    orphan.model = "unpriced".into();
    orphan.model_raw = "unpriced-raw".into();
    orphan.ts = 2_000;
    orphan.input_tokens = 10;
    orphan.output_tokens = 5;
    storage.insert_usage_record(&orphan).unwrap();

    storage
        .conn
        .execute(
            "INSERT INTO model_prices (model_id, input_per_mtok_micros, output_per_mtok_micros)
             VALUES ('claude-sonnet-4-5', 3000000, 15000000)",
            [],
        )
        .unwrap();
    storage.recompute_costs().unwrap();
}

#[test]
fn usage_records_page_joins_sessions_and_views() {
    let (storage, path) = test_db("usage-page");
    usage_page_fixture(&storage);

    let page = storage
        .usage_records_page(&UsageRecordsQuery {
            sort_desc: true,
            limit: 10,
            ..UsageRecordsQuery::default()
        })
        .unwrap();
    assert_eq!(page.total, 2);

    // 时间倒序：孤儿记录（ts 2000）在前，会话归属为 NULL。
    let orphan = &page.rows[0];
    assert_eq!(orphan.tool, "codex");
    assert_eq!(orphan.session_external_id, None);
    assert_eq!(orphan.project_dir, None);
    assert_eq!(orphan.cost_micros, None, "未定价模型总分 NULL");
    assert_eq!(orphan.duration_ms, None);

    let priced = &page.rows[1];
    assert_eq!(priced.session_external_id.as_deref(), Some("sess-1"));
    assert_eq!(priced.project_dir.as_deref(), Some("E:/p"));
    assert_eq!(priced.cost_micros, Some(10_500), "成本走视图口径");
    assert_eq!(
        (priced.input_cost_micros, priced.output_cost_micros),
        (Some(3_000), Some(7_500)),
        "分桶费用由 IPC 直出，前端不再推导"
    );
    assert_eq!(priced.duration_ms, Some(1_500), "duration 经扫描入库存原样");
    drop(storage);
    cleanup(&path);
}

#[test]
fn usage_records_page_filters_and_sorts_server_side() {
    let (storage, path) = test_db("usage-filters");
    usage_page_fixture(&storage);

    let query = |f: UsageFilters| {
        storage.usage_records_page(&UsageRecordsQuery {
            filters: f,
            limit: 10,
            ..UsageRecordsQuery::default()
        })
    };

    // 工具 / 模型 / 项目（含无项目哨兵）/ 搜索 / 时间窗。
    assert_eq!(
        query(UsageFilters {
            tools: vec!["codex".into()],
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1
    );
    assert_eq!(
        query(UsageFilters {
            models: vec!["unpriced".into()],
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1
    );
    assert_eq!(
        query(UsageFilters {
            projects: vec!["E:/p".into()],
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1,
        "有项目只中会话行"
    );
    assert_eq!(
        query(UsageFilters {
            projects: vec![String::new()],
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1,
        "空串哨兵只中孤儿行"
    );
    assert_eq!(
        query(UsageFilters {
            projects: vec![String::new(), "E:/p".into()],
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        2,
        "哨兵与具名项目可并用"
    );
    assert_eq!(
        query(UsageFilters {
            search: Some("sess".into()),
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1,
        "按会话 id 搜索"
    );
    assert_eq!(
        query(UsageFilters {
            search: Some("E:/p".into()),
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1,
        "按项目目录搜索"
    );
    assert_eq!(
        query(UsageFilters {
            search: Some("no-such-thing".into()),
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        0
    );
    assert_eq!(
        query(UsageFilters {
            time_start: Some(1_500),
            ..UsageFilters::default()
        })
        .unwrap()
        .total,
        1,
        "时间下界含端点"
    );

    // 排序：tokens DESC 量级大者在前；cost ASC 时未知（NULL）在最前。
    let by_tokens = storage
        .usage_records_page(&UsageRecordsQuery {
            sort_key: Some("tokens".into()),
            sort_desc: true,
            limit: 10,
            ..UsageRecordsQuery::default()
        })
        .unwrap();
    assert_eq!(by_tokens.rows[0].input_tokens, 1_000, "tokens 降序");

    let by_cost = storage
        .usage_records_page(&UsageRecordsQuery {
            sort_key: Some("cost".into()),
            sort_desc: false,
            limit: 10,
            ..UsageRecordsQuery::default()
        })
        .unwrap();
    assert_eq!(by_cost.rows[0].cost_micros, None, "升序时未知成本在最前");

    // 禁用工具与读路径其它入口同一语义：不出现，但不删数据。
    let disabled = storage
        .usage_records_page(&UsageRecordsQuery {
            disabled: vec!["claude-code".into()],
            limit: 10,
            ..UsageRecordsQuery::default()
        })
        .unwrap();
    assert_eq!(disabled.total, 1);
    assert_eq!(disabled.rows[0].tool, "codex");

    // 分页：limit 1 翻页能取全两条。
    let p1 = storage
        .usage_records_page(&UsageRecordsQuery {
            sort_desc: true,
            limit: 1,
            ..UsageRecordsQuery::default()
        })
        .unwrap();
    let p2 = storage
        .usage_records_page(&UsageRecordsQuery {
            sort_desc: true,
            offset: 1,
            limit: 1,
            ..UsageRecordsQuery::default()
        })
        .unwrap();
    assert_eq!(p1.rows.len() + p2.rows.len(), 2);
    drop(storage);
    cleanup(&path);
}

#[test]
fn usage_filter_options_faceted_counts_and_respect_disabled() {
    let (storage, path) = test_db("usage-options");
    usage_page_fixture(&storage);

    let options = storage
        .usage_filter_options(&UsageFilters::default(), &[])
        .unwrap();
    assert_eq!(
        options
            .tools
            .iter()
            .map(|o| (o.value.clone(), o.count))
            .collect::<Vec<_>>(),
        vec![(Some("claude-code".into()), 1), (Some("codex".into()), 1),],
        "计数相同时按名字升序"
    );
    assert!(
        options
            .projects
            .iter()
            .any(|o| o.value.is_none() && o.count == 1),
        "孤儿行以无项目选项出现"
    );
    assert_eq!(options.projects.len(), 2);

    // 分面口径：筛选模型后，工具/项目计数只剩命中该模型的记录，
    // 而模型维度自身照常列出全部选项（不含本维度筛选）。
    let by_model = storage
        .usage_filter_options(
            &UsageFilters {
                models: vec!["unpriced".into()],
                ..UsageFilters::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(
        by_model
            .tools
            .iter()
            .map(|o| (o.value.clone(), o.count))
            .collect::<Vec<_>>(),
        vec![(Some("codex".into()), 1)],
        "工具计数跟随模型筛选"
    );
    assert_eq!(
        by_model
            .projects
            .iter()
            .map(|o| (o.value.clone(), o.count))
            .collect::<Vec<_>>(),
        vec![(None, 1)],
        "项目计数跟随模型筛选，孤儿行即无项目"
    );
    assert_eq!(by_model.models.len(), 2, "模型维度不含自身筛选");

    let without_codex = storage
        .usage_filter_options(&UsageFilters::default(), &["codex".into()])
        .unwrap();
    assert_eq!(
        without_codex
            .tools
            .iter()
            .map(|o| o.value.clone())
            .collect::<Vec<_>>(),
        vec![Some("claude-code".into())],
        "禁用工具的记录不进选项"
    );
    assert_eq!(
        without_codex.projects.len(),
        1,
        "无项目选项随孤儿行一起消失（孤儿属于 codex）"
    );
    drop(storage);
    cleanup(&path);
}

/// 建两个会话：s1 有两条用量（其一未定价 → 成本未知），s2 只有会话壳。
fn seed_sessions(storage: &Storage) -> (i64, i64) {
    let file_id = storage
        .upsert_scanned_file("/logs/a.jsonl", "claude-code", &[1], 3, 3, 0)
        .unwrap();
    let (s1, _) = storage
        .get_or_create_session(
            &NewSession {
                tool: "claude-code".into(),
                external_id: "s1".into(),
                external_id_is_derived: false,
                title: None,
                project_dir: Some("E:/proj/demo".into()),
                source_file_id: file_id,
            },
            100,
        )
        .unwrap();
    storage
        .update_session_activity(s1, 2_000, Some("修 bug"))
        .unwrap();
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
            200,
        )
        .unwrap();
    storage
        .update_session_activity(s2, 3_000, Some("codex 会话"))
        .unwrap();
    storage
        .insert_usage_record(&NewUsageRecord {
            session_id: Some(s1),
            tool: "claude-code".into(),
            model_raw: "m".into(),
            model: "m".into(),
            ts: 1_500,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 10,
            cache_write_tokens: 0,
            reasoning_tokens: None,
            duration_ms: None,
            request_count: 1,
            dedup_key: vec![1],
        })
        .unwrap();
    // 未定价模型：聚合里进 unknown_cost_rows，不冒充已知成本。
    storage
        .insert_usage_record(&NewUsageRecord {
            session_id: Some(s1),
            tool: "claude-code".into(),
            model_raw: "unpriced".into(),
            model: "unpriced".into(),
            ts: 1_800,
            input_tokens: 200,
            output_tokens: 20,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
            duration_ms: None,
            request_count: 1,
            dedup_key: vec![2],
        })
        .unwrap();
    (s1, s2)
}

#[test]
fn sessions_page_aggregates_usage_per_session() {
    let (storage, path) = test_db("sessions-agg");
    let (s1, s2) = seed_sessions(&storage);

    let page = storage
        .sessions_page(&SessionsPageQuery {
            sort_desc: true,
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(page.total, 2);
    // 默认按最近活跃倒序：s2（3_000）在前。
    assert_eq!(page.rows[0].id, s2);
    assert_eq!(page.rows[0].request_count, 0, "零用量壳保留，聚合为 0");

    let row = &page.rows[1];
    assert_eq!(row.id, s1);
    assert_eq!(row.title.as_deref(), Some("修 bug"));
    assert_eq!(row.request_count, 2);
    assert_eq!(row.input_tokens, 300);
    assert_eq!(row.output_tokens, 70);
    assert_eq!(row.cache_read_tokens, 10);

    drop(storage);
    cleanup(&path);
}

#[test]
fn sessions_page_filters_search_and_disabled() {
    let (storage, path) = test_db("sessions-filter");
    seed_sessions(&storage);

    // 搜索命中标题。
    let hit = storage
        .sessions_page(&SessionsPageQuery {
            filters: SessionFilters {
                search: Some("修 bug".into()),
                ..Default::default()
            },
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hit.total, 1);
    assert_eq!(hit.rows[0].external_id, "s1");

    // 搜索命中项目目录。
    let by_project = storage
        .sessions_page(&SessionsPageQuery {
            filters: SessionFilters {
                search: Some("E:/proj/demo".into()),
                ..Default::default()
            },
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_project.total, 1);

    // 工具白名单。
    let codex_only = storage
        .sessions_page(&SessionsPageQuery {
            filters: SessionFilters {
                tools: vec!["codex".into()],
                ..Default::default()
            },
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(codex_only.total, 1);
    assert_eq!(codex_only.rows[0].external_id, "s2");

    // 禁用工具整体隐藏。
    let disabled = storage
        .sessions_page(&SessionsPageQuery {
            disabled: vec!["codex".into()],
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(disabled.total, 1);
    assert_eq!(disabled.rows[0].external_id, "s1");

    // 时间窗落在 s2 的最近活跃之后：只剩 s2。
    let recent = storage
        .sessions_page(&SessionsPageQuery {
            filters: SessionFilters {
                time_start: Some(2_500),
                ..Default::default()
            },
            limit: 10,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(recent.total, 1);
    assert_eq!(recent.rows[0].external_id, "s2");

    drop(storage);
    cleanup(&path);
}

#[test]
fn sessions_page_sorts_by_whitelisted_keys_and_paginates() {
    let (storage, path) = test_db("sessions-sort");
    let (s1, _s2) = seed_sessions(&storage);

    // 按请求数排：s1（2 条）在前。
    let by_requests = storage
        .sessions_page(&SessionsPageQuery {
            sort_key: Some("requests".into()),
            sort_desc: true,
            limit: 1,
            offset: 0,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_requests.rows.len(), 1);
    assert_eq!(by_requests.rows[0].id, s1, "按请求数降序 s1 在前");

    // 白名单外的键退回最近活跃；分页偏移生效。
    let second_page = storage
        .sessions_page(&SessionsPageQuery {
            sort_key: Some("hacker".into()),
            sort_desc: true,
            limit: 1,
            offset: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(second_page.total, 2);
    assert_eq!(second_page.rows[0].id, s1, "活跃倒序的第二位是 s1");

    drop(storage);
    cleanup(&path);
}

#[test]
fn marking_file_missing_clears_its_transcript_index() {
    let (storage, path) = test_db("missing-index");
    let file_id = storage
        .upsert_scanned_file("/x/model-io-sess_a.jsonl", "zcode", b"h", 10, 10, 1)
        .unwrap();

    // 提交两条索引条目 + 建站状态，模拟已建好索引的文件。
    storage
        .commit_transcript_index(
            file_id,
            &[NewTranscriptEntry {
                seq: 0,
                role: "user".into(),
                ts_ms: Some(1),
                model: None,
                kind: 0,
                frag_offset: 0,
                frag_len: 5,
                skip_blocks: None,
                frag_meta: None,
            }],
            &TranscriptIndexState {
                built_offset: 10,
                size: 10,
                mtime_ms: 1,
                complete: true,
                baseline_offset: None,
                baseline_len: None,
                pending_offset: None,
                pending_len: None,
                pending_ts: None,
                pending_model: None,
            },
        )
        .unwrap();

    // 标记消失：索引条目与状态必须一并清空——源没了，字节区间无法解引用。
    storage.set_file_missing(file_id, Some(99)).unwrap();
    let entries: i64 = storage
        .conn
        .query_row(
            "SELECT COUNT(*) FROM transcript_entries WHERE file_id = ?1",
            params![file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(entries, 0, "文件消失后索引条目必须被清理");
    assert!(
        storage.transcript_index_state(file_id).unwrap().is_none(),
        "文件消失后建站状态必须被清理"
    );
    let missing: Option<i64> = storage
        .conn
        .query_row(
            "SELECT missing_since FROM scanned_files WHERE id = ?1",
            params![file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(missing, Some(99), "消失标记本身必须保留");

    // 清除消失标记（文件失而复得）：不再有索引行可清，状态保持"未建站"。
    storage.set_file_missing(file_id, None).unwrap();
    assert!(storage.transcript_index_state(file_id).unwrap().is_none());
    let missing: Option<i64> = storage
        .conn
        .query_row(
            "SELECT missing_since FROM scanned_files WHERE id = ?1",
            params![file_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(missing, None);

    // 重复标记已消失的文件：幂等，不报错。
    storage.set_file_missing(file_id, Some(100)).unwrap();

    drop(storage);
    cleanup(&path);
}

/// 脏端点不再挂死分桶：Month 分支的年份递增、Hour/Day 的 epoch 步进都被桶数
/// 硬上限 / 不可解析游标终止拦住。旧实现在 end 越过 chrono 表示域（约 8.2e15 ms）
/// 后 Month 分支 y 递增越过 262143、local_ms 恒 0，永不 break；Hour/Day 从
/// 不可解析的 start 一路步进到 last 是天文级循环。
#[test]
fn bucket_starts_is_capped_on_dirty_endpoints() {
    // Month 真死循环场景：end 超出 chrono 可表示范围（floor 原样返回）。
    let dirty_end = 9_000_000_000_000_000_i64;
    let starts = bucket_starts(1_700_000_000_000, dirty_end, TrendGrain::Month);
    assert!(
        (1..=401).contains(&starts.len()),
        "脏 end 下 Month 分桶必须在硬上限内终止：{}",
        starts.len()
    );

    // Hour/Day 天文级步进场景：start 在表示域之下（floor 原样返回），
    // 游标不可解析即终止，产出为空。
    let dirty_start = -9_000_000_000_000_000_i64;
    assert!(bucket_starts(dirty_start, 1_700_000_000_000, TrendGrain::Hour).is_empty());
    assert!(bucket_starts(dirty_start, 1_700_000_000_000, TrendGrain::Day).is_empty());
}

/// 正常窗口的桶序列不受防线影响：日/月粒度的数量与单调性都保持原口径。
#[test]
fn bucket_starts_covers_normal_windows() {
    let day = 86_400_000;
    // 3 天窗口：本地日桶 3~5 个（时区偏移与 DST 伸缩的合法范围）。
    let starts = bucket_starts(
        1_700_000_000_000,
        1_700_000_000_000 + 3 * day,
        TrendGrain::Day,
    );
    assert!(
        (3..=5).contains(&starts.len()),
        "3 天窗口应产出 3~5 个本地日桶：{starts:?}"
    );
    assert!(starts.windows(2).all(|w| w[0] < w[1]), "桶起点必须严格递增");

    // 2023-11-15 前后 ~90 天，跨 Nov/Dec/Jan/Feb：恰 4 个月桶（对时区不敏感）。
    let starts = bucket_starts(
        1_700_000_000_000,
        1_700_000_000_000 + 90 * day,
        TrendGrain::Month,
    );
    assert_eq!(starts.len(), 4);
}

/// 迁移脚本的形状约束：非空、每条 SQL 都有实际语句。版本连续性由"下标即版本"
/// 的数组编码保证，无需断言；这里拦的是误提交空串或纯注释 SQL。
#[test]
fn migration_scripts_are_non_empty() {
    assert!(!migrations::MIGRATIONS.is_empty());
    for (idx, sql) in migrations::MIGRATIONS.iter().enumerate() {
        assert!(
            sql.split(';').any(|stmt| stmt
                .split_whitespace()
                .any(|token| !token.starts_with("--"))),
            "MIGRATIONS[{idx}] 是空迁移"
        );
    }
}

/// 后段迁移（v13 起）必须可重入：测试基建靠"回拨版本重跑后段"验证单条迁移，
/// 这要求回拨到 12..len 的任何版本后重开，链尾都能在同一份 schema 上重跑成功。
/// （v18 的 IF NOT EXISTS、v22 的整表重建就是为这个约束服务的；更早的迁移
/// 含无 IF NOT EXISTS 的 CREATE TABLE，重跑会撞名，不属于此契约。）
#[test]
fn tail_migrations_are_reentrant_from_any_rollback_point() {
    for rollback_to in 12..migrations::MIGRATIONS.len() {
        let (storage, path) = test_db(&format!("reentrant-{rollback_to}"));
        storage
            .conn
            .execute(&format!("PRAGMA user_version = {rollback_to}"), [])
            .unwrap();
        drop(storage);

        let reopened = open(&path).unwrap();
        let version: i64 = reopened
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version as usize,
            migrations::MIGRATIONS.len(),
            "回拨到 v{rollback_to} 重开后必须补齐到最新版"
        );
        drop(reopened);
        cleanup(&path);
    }
}
