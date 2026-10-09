//! 用量计量：把一次代理请求的事实写进主库 `gateway_requests` 并落分项成本。
//! 写记录、建档、计价在 [`toktol_core::storage::Storage::insert_gateway_request`]
//! 的单事务里完成（为什么必须单库单事务，见 `paths::main_db_path`）。
//! 用量本身由 translate 模块从上游响应/流中抽取，本模块只负责落库。

use std::path::Path;

use toktol_core::model::TokenUsage;
use toktol_core::storage::{self, NewGatewayRequest};

/// 一次代理请求的计量事实。
pub struct MeterFacts {
    /// 请求开始的 epoch ms（UTC）。
    pub ts: i64,
    /// 请求里的原始模型名。
    pub model_raw: String,
    /// 归一化建议值（入库时可能被映射覆盖）。
    pub model: String,
    /// 上游返回的用量；上游没给时为默认全 0。
    pub usage: TokenUsage,
    /// 上游响应状态码。
    pub status_code: Option<i64>,
    /// 请求总耗时（毫秒）。
    pub latency_ms: i64,
    /// 命中的上游名。
    pub upstream: String,
    /// 访问令牌的 sha256；只落哈希，明文永不入参、不入库。
    pub token_hash: [u8; 32],
}

/// 落库。**静默失败**：此刻响应已经（或正在）回传给客户端，计量失败既无法挽回
/// 也不该让本次请求报错——宁可漏一行统计，不引入对请求路径的错误传播。
/// 失败打 stderr（无 tracing 依赖阶段的最低限度可见性），不打断请求。
///
/// 带少量重试：全新库上首个连接要建 schema，`PRAGMA journal_mode=WAL` 在其它连接
/// 正开着库时会短暂 BUSY（busy_timeout 对 journal_mode 变更不总生效），重试即可越过。
///
/// **阻塞调用**：开库含完整迁移与 WAL 协商，最坏带 3×50ms 重试——绝不能在
/// async 上下文直接调用（会占死 tokio worker 线程），调用方必须 `spawn_blocking`。
pub fn record(db_path: &Path, facts: MeterFacts) {
    let mut open_error = None;
    for _ in 0..3 {
        match storage::open(db_path) {
            Ok(storage) => {
                if let Err(err) = storage.insert_gateway_request(&NewGatewayRequest {
                    tool: None,
                    model_raw: facts.model_raw.clone(),
                    model: facts.model.clone(),
                    ts: facts.ts,
                    input_tokens: facts.usage.input_tokens,
                    output_tokens: facts.usage.output_tokens,
                    cache_read_tokens: facts.usage.cache_read_tokens,
                    cache_write_tokens: facts.usage.cache_write_tokens,
                    reasoning_tokens: facts.usage.reasoning_tokens,
                    status_code: facts.status_code,
                    latency_ms: Some(facts.latency_ms),
                    upstream: Some(facts.upstream.clone()),
                    token_hash: Some(facts.token_hash.to_vec()),
                }) {
                    eprintln!("toktol-gateway: metering insert failed: {err}");
                }
                return;
            }
            Err(err) => open_error = Some(err),
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if let Some(err) = open_error {
        eprintln!("toktol-gateway: metering open failed: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 独占临时目录里的库路径；Drop 时连目录一起清掉。
    struct TempDb(PathBuf);

    impl TempDb {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "toktol-metering-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("创建临时目录");
            Self(dir.join("toktol.db"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            if let Some(dir) = self.0.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    fn facts(ts: i64) -> MeterFacts {
        MeterFacts {
            ts,
            model_raw: "raw-test-model".into(),
            model: "canonical-test-model".into(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                cache_read_tokens: 5,
                cache_write_tokens: 0,
                reasoning_tokens: Some(8),
            },
            status_code: Some(200),
            latency_ms: 42,
            upstream: "upstream-a".into(),
            token_hash: [7u8; 32],
        }
    }

    /// 全新库路径上首次落库即成功：record 自带开库建 schema（重试注释里
    /// "首个连接建 schema"的契约），行内容与入参一致；未定价模型的成本列
    /// 为 NULL——"不发明数字"的红线在网关计量侧同样成立。
    #[test]
    fn record_builds_schema_and_writes_usage_on_fresh_db() {
        let db = TempDb::new("fresh");
        record(db.path(), facts(1000));

        let storage = storage::open(db.path()).expect("record 应已建好 schema");
        let page = storage.gateway_requests_page(0, 10).unwrap();
        assert_eq!(page.total, 1);
        let row = &page.rows[0];
        assert_eq!(row.model_raw, "raw-test-model");
        assert_eq!(row.input_tokens, 10);
        assert_eq!(row.output_tokens, 20);
        assert_eq!(row.status_code, Some(200));
        assert_eq!(row.latency_ms, Some(42));
        assert_eq!(row.upstream.as_deref(), Some("upstream-a"));
        assert_eq!(row.cost_micros, None, "未定价模型不得编造成本");
    }

    /// 每次调用一行：网关计量没有 dedup 概念（dedup_key 是扫描侧的），
    /// 两次 record 必须两行——漏写成 upsert 会静默少计请求量。
    #[test]
    fn record_appends_one_row_per_call() {
        let db = TempDb::new("append");
        record(db.path(), facts(1000));
        record(db.path(), facts(2000));

        let storage = storage::open(db.path()).unwrap();
        let page = storage.gateway_requests_page(0, 10).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(page.rows[0].ts, 2000, "按时间倒序");
    }

    /// 开库失败把重试轮走完仍静默返回：计量发生在响应回传之后，失败不
    /// 传播、不 panic——宁可漏一行统计（见 record 文档）。db 路径的父级
    /// 是普通文件，SQLite 无法在该路径建库，三次尝试必然全败。
    #[test]
    fn record_silently_survives_unopenable_db() {
        let dir = std::env::temp_dir().join(format!(
            "toktol-metering-block-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("创建临时目录");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();

        record(&blocker.join("inner.db"), facts(1000));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
