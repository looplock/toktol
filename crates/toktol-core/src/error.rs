//! 全 crate 统一的错误类型：上层（网关 / 壳 / 前端命令）只处理一种错误形状，
//! 新阶段的变体追加在这里，不另起炉灶。
//! **语言归属**：领域层只判定"出了什么错"，不产出用户文案——前端拿 [`ErrorCode`]
//! 结合 locale 选文案；[`Display`] 实现仅服务日志，一律英文，避免 i18n 被硬编码文案锁死。

use std::path::PathBuf;

use serde::Serialize;

/// 对外暴露的稳定错误码：跨 IPC 传给前端按 locale 选文案。
/// 取值即契约，发布后不再更改或复用（新语义加新变体）；规范由 verify-constants-parity.mjs 校验。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ErrorCode {
    /// 主目录不可用，`~/.toktol/` 无法定位。
    #[serde(rename = "core.home_dir_unavailable")]
    HomeDirUnavailable,

    /// 数据目录或其中的文件读写失败。
    #[serde(rename = "core.data_file")]
    DataFile,

    /// SQLite 操作失败。
    #[serde(rename = "core.sqlite")]
    Sqlite,

    /// JSON 序列化/反序列化失败。
    #[serde(rename = "core.json")]
    Json,

    /// 逻辑错误：调用方违反前置条件。
    #[serde(rename = "core.internal")]
    Internal,

    /// 请求的实体不存在（如会话已被删除/未入库）。
    #[serde(rename = "core.not_found")]
    NotFound,

    /// 该功能对此来源不适用（如数据库类来源的会话没有文件可进回收站）。
    #[serde(rename = "core.unsupported")]
    Unsupported,

    /// 会话源日志超过转录现读上限（且该工具不支持索引转录）：拒绝读取，
    /// 前端明确告知，而不是把进程拖进全量读的泥潭。
    #[serde(rename = "core.transcript_oversize")]
    TranscriptOversize,

    /// 会话的源日志已被工具自己回收（如 zcode 滚动清理只留最近几个会话），
    /// 转录不可查看；用量事实已在库，不受影响。
    #[serde(rename = "core.source_gone")]
    SourceGone,

    /// 模型目录（models.dev 公共 API）拉取失败：断网、非 2xx、响应不可读。
    #[serde(rename = "core.catalog_fetch")]
    CatalogFetch,

    /// 网关配置缺失或非法（gateway.json 不存在、校验不过）。
    #[serde(rename = "gateway.config")]
    GatewayConfig,

    /// 访问令牌缺失或不匹配。
    #[serde(rename = "gateway.auth")]
    GatewayAuth,

    /// 上游转发失败（无路由、上游不可达、上游返回错误）。
    #[serde(rename = "gateway.upstream")]
    GatewayUpstream,
}

impl ErrorCode {
    /// 稳定错误码的字符串形态。IPC 的错误通道用 `String` 传码，这里的取值与
    /// serde rename 是同一份契约的两面，一致性由测试钉住。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::HomeDirUnavailable => "core.home_dir_unavailable",
            ErrorCode::DataFile => "core.data_file",
            ErrorCode::Sqlite => "core.sqlite",
            ErrorCode::Json => "core.json",
            ErrorCode::Internal => "core.internal",
            ErrorCode::NotFound => "core.not_found",
            ErrorCode::Unsupported => "core.unsupported",
            ErrorCode::TranscriptOversize => "core.transcript_oversize",
            ErrorCode::SourceGone => "core.source_gone",
            ErrorCode::CatalogFetch => "core.catalog_fetch",
            ErrorCode::GatewayConfig => "gateway.config",
            ErrorCode::GatewayAuth => "gateway.auth",
            ErrorCode::GatewayUpstream => "gateway.upstream",
        }
    }
}

/// Toktol 领域层错误的唯一出口。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The user home directory cannot be determined, so `~/.toktol/` is unreachable.
    #[error("home directory unavailable")]
    HomeDirUnavailable,

    /// Reading or writing a file under the data directory failed.
    #[error("data file operation failed ({path}): {source}")]
    DataFile {
        /// 出问题的路径。
        path: PathBuf,
        /// 底层 IO 错误。
        source: std::io::Error,
    },

    /// SQLite 操作失败。
    #[error("sqlite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// JSON 序列化/反序列化失败。
    #[error("json (de)serialization failed: {0}")]
    Json(#[from] serde_json::Error),

    /// 逻辑错误：调用方违反前置条件。后续阶段按模块细分。
    #[error("internal error: {0}")]
    Internal(String),

    /// 请求的实体不存在（如会话已被删除/未入库）。
    #[error("entity not found: {0}")]
    NotFound(String),

    /// 该功能对此来源不适用（如数据库类来源的会话没有文件可进回收站）。
    #[error("operation not supported for this source")]
    Unsupported,

    /// 会话源日志超过转录现读上限（且该工具不支持索引转录）：拒绝读取，
    /// 而不是把进程拖进全量读的泥潭。详见 sessions 的转录编排。
    #[error("transcript source file exceeds the read limit")]
    TranscriptOversize,

    /// 会话的源日志已被工具自己回收（如 zcode 滚动清理）。详见
    /// [`Adapter::resolve_transcript_source`](crate::adapter::Adapter::resolve_transcript_source)。
    #[error("session source log is gone: {0}")]
    SourceGone(PathBuf),

    /// 模型目录（models.dev 公共 API）拉取失败。
    #[error("model catalog fetch failed: {0}")]
    CatalogFetch(String),

    /// 网关配置缺失或非法。
    #[error("gateway configuration invalid: {0}")]
    GatewayConfig(String),

    /// 访问令牌缺失或不匹配。Display 不含任何令牌线索。
    #[error("gateway authentication failed")]
    GatewayAuth,

    /// 上游转发失败。
    #[error("gateway upstream failure: {0}")]
    GatewayUpstream(String),
}

impl Error {
    /// 对外的机器可读错误码，供前端按 locale 选择展示文案。
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::HomeDirUnavailable => ErrorCode::HomeDirUnavailable,
            Error::DataFile { .. } => ErrorCode::DataFile,
            Error::Sqlite(_) => ErrorCode::Sqlite,
            Error::Json(_) => ErrorCode::Json,
            Error::Internal(_) => ErrorCode::Internal,
            Error::NotFound(_) => ErrorCode::NotFound,
            Error::Unsupported => ErrorCode::Unsupported,
            Error::TranscriptOversize => ErrorCode::TranscriptOversize,
            Error::SourceGone(_) => ErrorCode::SourceGone,
            Error::CatalogFetch(_) => ErrorCode::CatalogFetch,
            Error::GatewayConfig(_) => ErrorCode::GatewayConfig,
            Error::GatewayAuth => ErrorCode::GatewayAuth,
            Error::GatewayUpstream(_) => ErrorCode::GatewayUpstream,
        }
    }
}

/// 领域层统一的 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// 领域层文案必须语言中立：断言不含 CJK，拦住面向用户的文案被塞回 `#[error(...)]`。
    #[test]
    fn error_messages_are_language_neutral() {
        let err = Error::HomeDirUnavailable;
        assert!(
            !err.to_string()
                .chars()
                .any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)),
            "领域层文案只服务日志，不应含中文：{:?}",
            err.to_string()
        );
        assert_eq!(err.code(), ErrorCode::HomeDirUnavailable);
    }

    /// 不同变体必须给出不同的码，否则前端无法区分；且码必须可序列化、带来源前缀。
    #[test]
    fn error_codes_are_serializeable_and_prefixed() {
        let samples = [
            Error::HomeDirUnavailable,
            Error::DataFile {
                path: PathBuf::from("/tmp/boom"),
                source: std::io::Error::other("boom"),
            },
            Error::Internal("boom".to_string()),
            Error::NotFound("session 42".to_string()),
            Error::GatewayConfig("boom".to_string()),
            Error::GatewayAuth,
            Error::GatewayUpstream("boom".to_string()),
        ];

        let encoded: Vec<String> = samples
            .iter()
            .map(|err| serde_json::to_string(&err.code()).expect("ErrorCode 必须可序列化"))
            .collect();

        for text in &encoded {
            assert!(
                text.starts_with("\"core.") || text.starts_with("\"gateway."),
                "错误码必须带来源族前缀：{text}"
            );
        }

        let unique: HashSet<&String> = encoded.iter().collect();
        assert_eq!(unique.len(), encoded.len(), "错误码必须互不相同");

        // as_str 与 serde 输出是同一契约的两面，漂移会骗过按字符串匹配的前端。
        for (serialized, as_str) in encoded
            .iter()
            .zip(samples.iter().map(|err| err.code().as_str()))
        {
            assert_eq!(*serialized, format!("\"{as_str}\""));
        }
    }

    #[test]
    fn sqlite_error_is_convertible() {
        let sqlite_err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
            Some("boom".to_string()),
        );
        let err: Error = sqlite_err.into();
        assert!(matches!(err, Error::Sqlite(_)));
        assert_eq!(err.code(), ErrorCode::Sqlite);
    }
}
