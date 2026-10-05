//! 适配器注册表与适配器契约：每个工具一个适配器，报告日志目录、把日志行解析成
//! 统一模型。隐私红线：只读取会话日志，绝不打开工具的配置或凭据文件，绝不读取
//! 密钥或环境变量。
//!
//! 契约：实现必须无状态——同一行在任何一次扫描里都解析出同一结果，因为重扫时
//! 同一行会再次进来，靠 dedup_key 去重；解析出不同结果反而会当成新记录重复入账。
//! 需要跨行上下文的工具（如 codex 的模型随回合变化）改实现文件级的
//! [`Adapter::parse_file`]，同样的确定性要求按"同一文件内容 + 同一 start"理解。
//! 会话存在自有 SQLite 里、没有可切行日志的工具（如 opencode）实现
//! [`Adapter::read_db_source`]，去重与增量由扫描层按行键与指纹兜住。
//! 日志是压缩文件的工具（如 dsh 的 zstd）改写 [`Adapter::decode`]。

pub mod claude_code;
pub mod codebuddy;
pub mod codex;
pub(crate) mod dbdelete;
pub mod dsh;
pub mod grok;
pub mod opencode;
pub mod pi;
pub mod transcript;
pub mod workbuddy;
pub mod zcode;

pub use transcript::{TranscriptBlock, TranscriptEntry, TranscriptRole};

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{TokenUsage, Tool};

/// 适配器契约。[`Self::parse_line`] 按行喂入：扫描层负责游标与断行，适配器只看单行。
pub trait Adapter: Sync {
    /// 本适配器负责的工具。
    fn tool(&self) -> Tool;

    /// 会话日志的候选目录。目录不存在是常态（工具没装），不是错误。
    fn log_dirs(&self) -> Vec<PathBuf>;

    /// 只匹配会话日志文件；配置、记忆、索引等一律排除。
    fn is_session_log(&self, path: &Path) -> bool;

    /// 工具日志不提供会话 id、由扫描层从文件派生时为 `true`。
    fn external_id_is_derived(&self) -> bool {
        false
    }

    /// 超过扫描层单文件读取上限的明文日志能否流式增量解析。只有行级自洽的
    /// 适配器（未改写 [`Self::parse_file`]，单行不依赖前文）才可以——流式没有
    /// 前缀行可重建跨行上下文。默认 `false`：超限文件整体跳过。
    fn supports_streaming_scan(&self) -> bool {
        false
    }

    /// 文件字节 → 待切行的文本字节。默认原样透传（明文 JSONL）；压缩日志的工具
    /// 改写为解压，解压失败返回 `None`（扫描层跳过该文件，不推进游标，下次重试）。
    /// 解压过的文件没有安全的增量游标：扫描层按整文件重解析，重复入账由
    /// dedup_key 拦住，指纹未变则整体跳过。
    fn decode<'a>(&self, raw: &'a [u8]) -> Option<Cow<'a, [u8]>> {
        Some(Cow::Borrowed(raw))
    }

    /// 解析一行日志。同一行必须永远解析出同一结果（见模块文档）。
    fn parse_line(&self, line: &str) -> LineParse;

    /// 文件级解析：扫描层把完整行连游标前的前缀一起喂入，`lines[..start]` 只供
    /// 重建跨行上下文（当前回合的模型、会话 id 等），返回值与 `lines[start..]`
    /// 一一对应，前缀行不得重复产出事实。默认逐行委托 [`Self::parse_line`]。
    fn parse_file(&self, lines: &[&str], start: usize) -> Vec<LineParse> {
        lines[start..]
            .iter()
            .map(|line| self.parse_line(line))
            .collect()
    }

    /// 带来源路径的文件级解析：`source_file` 供从日志的存放路径派生事实
    /// （如 workbuddy 的项目目录编码在日志目录名里）。默认忽略路径，
    /// 委托 [`Self::parse_file`]。
    fn parse_file_at(&self, source_file: &Path, lines: &[&str], start: usize) -> Vec<LineParse> {
        let _ = source_file;
        self.parse_file(lines, start)
    }

    /// 数据库类来源：工具把会话存进自有 SQLite，没有可切行的日志文件。返回全量
    /// 事实快照，每轮重新读取，增量入账靠行键去重、整体跳过靠指纹；`Ok(None)`
    /// 表示本适配器没有数据库来源。红线：只 SELECT 会话/消息类数据表，同一库
    /// 里的凭据类表（credential、account 等）与库外配置、凭据文件绝不触碰。
    fn read_db_source(&self) -> Result<Option<DbSource>> {
        Ok(None)
    }

    /// 数据库类来源的库路径（轻量，不读内容）。会话删除的防复活判定用：会话
    /// 登记的源文件是工具自有库时，删行会被下轮扫描重建，删除必须拒绝。
    /// 返回值与 [`Self::read_db_source`] 的库路径一致。
    fn db_source_path(&self) -> Option<PathBuf> {
        None
    }

    /// 转录的真实来源路径：默认即会话登记的源文件。事实出自治有库、转录在
    /// 别处的适配器（zcode：库出用量、rollout 出转录）改写——按 `external_id`
    /// 定位真实转录文件；已被工具回收（滚动清理）时返回
    /// [`Error::SourceGone`](crate::error::Error::SourceGone)，前端据此提示。
    fn resolve_transcript_source(&self, source_file: &Path, external_id: &str) -> Result<PathBuf> {
        let _ = external_id;
        Ok(source_file.to_path_buf())
    }

    /// 读取一个会话的完整消息转录（会话详情视图）。文件类来源 `source_file` 是
    /// 会话日志文件；数据库类来源（[`Self::read_db_source`]）是它的库文件，此时
    /// `external_id` 定位会话。默认不支持：日志不携带消息正文，或提取口径未定
    /// （如 grok 的消息在未经验证的 chat_history 里）。坏行跳过不报错；源文件
    /// 缺失返回 [`Error::DataFile`]。
    fn read_transcript(
        &self,
        source_file: &Path,
        external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let _ = (source_file, external_id);
        Err(Error::Unsupported)
    }

    /// 删除会话时应移入系统回收站的文件与目录（产品红线：绝不硬删）。
    /// 默认是会话的源日志文件本身；目录结构知识住在本适配器——如 claude-code 的
    /// subagents 子目录、grok/pi/dsh 的整会话目录。不存在的路径由编排层跳过。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        vec![source_file.to_path_buf()]
    }

    /// 删除数据库类来源的一个会话。这是"用户主动删除"的写例外落地处：把会话
    /// 在工具自有库里的全部行导出为 JSON bundle、经 `trash` 移入系统回收站
    /// （bundle 是"文件进回收站"的数据库等价物），入桶成功后才对工具库删行，
    /// 级联清理由库自己的外键承担。返回入桶 bundle 的路径供报告展示；会话行
    /// 已不存在时返回 `Ok(None)`——来源无可清之物，删除意图视为达成。
    /// 红线：只按会话主键 DELETE、不 UPDATE、不建表；导出只认各适配器的
    /// 白名单表；凭据类表（credential、account 等）绝不触碰。
    /// 默认 [`Error::Unsupported`]：未实现删除的数据库类来源（codebuddy）
    /// 维持拒绝；文件类来源不走此方法。
    fn delete_db_session(
        &self,
        _db_path: &Path,
        _external_id: &str,
        _bundle_dir: &Path,
        _trash: &dyn Fn(&Path) -> Result<()>,
    ) -> Result<Option<PathBuf>> {
        Err(Error::Unsupported)
    }

    /// 文件类来源的会话在工具自有库里也有事实行时为 `true`（zcode：rollout
    /// 文件出转录、应用库出用量与标题）。删除须双清：文件入桶之外，还要按
    /// [`Self::delete_db_session`] 清库行——只清文件会留下可被重扫重建的空壳。
    fn purge_db_rows_for_file_sessions(&self) -> bool {
        false
    }

    /// 转录是否走字节区间索引（单会话日志可达多 GB 的工具）。默认 false：
    /// 详情页现读全量（有大小闸门）。见 `transcript_index_state` 相关编排。
    fn transcript_indexed(&self) -> bool {
        false
    }

    /// 流式构建转录索引（检查点续建、可取消、进度回调）。返回是否建完整。
    /// 仅 [`Self::transcript_indexed`] 为 true 的适配器实现。
    #[allow(clippy::too_many_arguments)]
    fn build_transcript_index(
        &self,
        _storage: &crate::storage::Storage,
        _file_id: i64,
        _source: &Path,
        _external_id: &str,
        _checkpoint_bytes: u64,
        _cancel: &std::sync::atomic::AtomicBool,
        _progress: &dyn Fn(u64, u64),
    ) -> Result<bool> {
        Err(Error::Unsupported)
    }

    /// 从索引片段还原一条转录条目（片段语义由建站器定义）。渲染为空返回
    /// `Ok(None)`。
    #[allow(clippy::too_many_arguments)]
    fn transcript_entry_from_frag(
        &self,
        _kind: i64,
        _frag: &[u8],
        _skip_blocks: Option<i64>,
        _role: &str,
        _ts_ms: Option<i64>,
        _model: Option<&str>,
        _frag_meta: Option<&str>,
    ) -> Result<Option<TranscriptEntry>> {
        Err(Error::Unsupported)
    }
}

/// 数据库类来源的一次全量读取。
pub struct DbSource {
    /// 库文件路径：扫描簿记的来源标识（missing 标记、指纹存储）。
    pub path: PathBuf,
    /// 来源指纹（如 行数 + 最大更新时间的哈希）；与上次相同则本轮整体跳过。
    pub fingerprint: Vec<u8>,
    /// (去重键种子, 事实)。种子是来源内稳定唯一的行标识，扫描层拿它替代日志行
    /// 参与去重键；重扫同一行必须给出同一种子与同一事实。
    pub facts: Vec<(String, LineFacts)>,
}

/// 单行解析的三种结局。
#[derive(Debug, Clone, PartialEq)]
pub enum LineParse {
    /// 没有可入账的事实（心跳、队列操作、摘要行等）。
    Skip,
    /// 行损坏或截断。扫描层计数后跳过，不中止整个文件。
    Malformed,
    /// 有可入账的事实。
    Facts(Box<LineFacts>),
    /// 同一行拆出的多条事实（grok 请求级拆分：一次回合聚合事件 → 每次真实
    /// 网络请求一条）。顺序即请求顺序；扫描层逐条走 apply_facts——dedup 键
    /// 对同一条源行计算，靠 [`LineFacts::dedup_suffix`] 区分。每行至多一个
    /// `Facts`/`Split`，与 `lines[start..]` 的一一对应不受影响。
    Split(Vec<LineFacts>),
}

/// 一行日志里可入账的事实：会话归属，可选的元数据与用量。
#[derive(Debug, Clone, PartialEq)]
pub struct LineFacts {
    /// 工具侧的会话标识；子会话（如 subagent）与主会话共用同一 id，用量自然归并。
    pub session_external_id: String,
    /// 去重键后缀。同一行拆出的多条事实（如 grok 把一次轮次拆成多次请求）各自
    /// 带不同后缀，扫描层把它追加到去重键末尾；整行单条事实为 `None`。
    pub dedup_suffix: Option<String>,
    /// 本条事实代表的网络请求数；整行单条事实恒为 1。
    pub request_count: i64,
    /// 标题候选（如首条用户消息）。扫描层每会话只采第一次，之后保持不变。
    pub title: Option<String>,
    /// 项目目录；日志行未携带则 `None`。
    pub project_dir: Option<String>,
    /// 该行时间戳（epoch ms，UTC）；无则 `None`，不参与会话活跃度更新。
    pub ts_ms: Option<i64>,
    /// 用量事实；非 assistant 行为 `None`。
    pub usage: Option<UsageFacts>,
}

/// 一次请求的用量事实。
#[derive(Debug, Clone, PartialEq)]
pub struct UsageFacts {
    /// 请求时间（epoch ms，UTC）。
    pub ts_ms: i64,
    /// 日志里的原始模型名，永不改写。
    pub model_raw: String,
    /// 归一化建议值；`(tool, model_raw)` 已有映射时存储层以映射为准。
    pub model: String,
    /// 请求耗时（毫秒）；日志没带计时则为 `None`。
    pub duration_ms: Option<i64>,
    /// token 分桶用量。
    pub usage: TokenUsage,
}

/// 当前注册的全部适配器。新工具在这里登记，扫描管线自动覆盖。
pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![
        Box::new(claude_code::ClaudeCodeAdapter),
        Box::new(codex::CodexAdapter),
        Box::new(codebuddy::CodeBuddyAdapter {
            app_data_dir: None,
            local_data_dir: None,
        }),
        Box::new(pi::PiAdapter),
        Box::new(opencode::OpenCodeAdapter { db_path: None }),
        Box::new(zcode::ZcodeAdapter {
            db_path: None,
            rollout_dir: None,
            session_meta: Default::default(),
        }),
        Box::new(dsh::DshAdapter),
        Box::new(workbuddy::WorkBuddyAdapter {
            decoded: Default::default(),
        }),
        Box::new(grok::GrokAdapter),
    ]
}
