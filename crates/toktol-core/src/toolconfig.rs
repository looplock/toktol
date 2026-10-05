//! 工具配置视图（配置页数据源）：只读枚举各工具本地的 MCP 服务器、Skills 与
//! 配置文件/目录，并可按白名单路径回读文件内容。
//!
//! 与扫描侧的分工：适配器的红线是"绝不打开配置与凭据文件"；本模块是产品功能
//! 明确要读配置文件的唯一例外，约束从"不读"放宽为"只读元数据、只出脱敏值"：
//! - MCP 只提取连接元数据；`env` 只出键名不出值，URL 查询串与命令行参数里
//!   疑似密钥的值打码为 `***`（与 README"绝不读取密钥"的承诺同口径）。
//! - 文件内容回读有双闸门：凭据类文件名（auth/credentials/keyblob 等）直接
//!   拒读；其余文件展示前逐行脱敏（JSON/TOML/env 的键值对与 URL 查询串）。
//!   大小超过 [`CONTENT_MAX_BYTES`] 截断，二进制拒读。
//! - 目录枚举有深度与条数上限，跳过依赖目录；文件不存在的工具返回
//!   `supported = false`（没装是常态，不是错误）。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::paths;

/// 内容回读的单文件字节上限：超过即截断（flag 标记），不整份载入。
pub const CONTENT_MAX_BYTES: usize = 256 * 1024;

/// 目录枚举上限（每个根）：真实工具目录（如 ~/.claude、~/.workbuddy）在深度 3
/// 内的条目远小于此；兜住异常目录（挂载点、失控的缓存）。
const ENTRIES_MAX: usize = 400;

/// 目录枚举深度（相对根）：配置页只展示概貌，更深的内容走子目录逐层回读不到，
/// 由用户在文件管理器里看。3 层足够覆盖 `skills/<name>/SKILL.md` 与
/// `plugins/cache/<marketplace>/<plugin>/skills/<name>` 的常见布局。
const TREE_MAX_DEPTH: usize = 3;

/// 枚举时跳过的目录名：体积黑洞（依赖/缓存/版本库）对配置视图没有信息量。
const SKIP_DIRS: [&str; 4] = ["node_modules", ".git", "__pycache__", ".venv"];

/// Skills frontmatter 里 description 的截断长度（字符）。
const SKILL_DESC_MAX_CHARS: usize = 200;

/// 一个工具的配置视图。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfigReport {
    /// 工具 id（与 `model::Tool::as_str` 同一取值）。
    pub tool: String,
    /// 工具主目录的展示标签（`~/.claude` 形态，展示用）。
    pub home_label: String,
    /// 主目录/主配置是否存在；false 表示工具未安装，前端显示不支持态。
    pub supported: bool,
    /// MCP 服务器连接元数据（已脱敏）。
    pub mcp: Vec<McpServerView>,
    /// Skills 清单。
    pub skills: Vec<SkillView>,
    /// 文件树根（目录根带子树条目；单文件根条目为空）。
    pub roots: Vec<ConfigRoot>,
}

/// 一台 MCP 服务器的连接元数据。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerView {
    /// 配置里的服务器名。
    pub name: String,
    /// 传输形态：`stdio` / `http` / `sse`。
    pub transport: String,
    /// 配置层级：`user` / `project` / `plugin`。
    pub scope: String,
    /// stdio 命令行（command + args 拼接，已脱敏）。
    pub command: Option<String>,
    /// 远端 URL（查询串已脱敏）。
    pub url: Option<String>,
    /// env 的键名清单；值绝不输出。
    pub env_keys: Vec<String>,
    /// project 层级的归属项目目录；其余层级为 `None`。
    pub project: Option<String>,
}

/// 一个 Skill 条目。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    /// 技能名（目录名）。
    pub name: String,
    /// 配置层级：`user` / `project` / `plugin`。
    pub scope: String,
    /// SKILL.md frontmatter 的 description（截断到 200 字符）。
    pub description: Option<String>,
    /// 展示路径（`~/.claude/skills/foo` 形态）。
    pub path: String,
    /// 内容回读定位：SKILL.md 所在的根键与根内相对路径（抽屉详情用）。
    pub root: String,
    /// SKILL.md 相对根的路径（`/` 分隔）。
    pub rel: String,
}

/// 文件树的一个根：目录根带 [`FileEntry`] 子树，单文件根（如 ~/.claude.json）
/// 条目为空、内容走 [`read_entry`] 以空 `rel` 回读。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigRoot {
    /// 根标识，[`read_entry`] 的定位键（每工具内唯一）。
    pub key: String,
    /// 展示标签（`~/.claude` 形态）。
    pub label: String,
    /// 子树条目（目录根）。
    pub entries: Vec<FileEntry>,
}

/// 文件树的一个条目。`rel` 相对根（`/` 分隔），回读内容时原样传回。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntry {
    /// 根内相对路径（目录根的直系子项为文件/目录名，深层带前缀）。
    pub rel: String,
    /// 末段名（展示）。
    pub name: String,
    /// 是否目录。
    pub dir: bool,
    /// 文件字节大小；目录为 `None`。
    pub size: Option<u64>,
}

/// 文件内容回读结果。文本一律经脱敏；`truncated` 标记超出上限被截断。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// 文件文本（已脱敏）。
    pub text: String,
    /// 超出 [`CONTENT_MAX_BYTES`] 被截断。
    pub truncated: bool,
}

/// 配置页全量视图：现读磁盘，不落库。
///
/// # Errors
/// 主目录不可得（`core.home_dir_unavailable`）或工具 id 未注册（`core.unsupported`）。
pub fn inspect(tool: &str) -> Result<ToolConfigReport> {
    let home = paths::home_dir().ok_or(Error::HomeDirUnavailable)?;
    if !is_known_tool(tool) {
        return Err(Error::Unsupported);
    }
    Ok(inspect_in(&home, tool))
}

/// 回读一个文件的内容（已脱敏、可能截断）。`rel` 为空串表示单文件根本身。
///
/// # Errors
/// 根键不存在或路径越界（`core.internal`）、目标缺失/非文件（`core.data_file`）、
/// 凭据类文件或二进制内容（`core.unsupported`）。
pub fn read_entry(tool: &str, root_key: &str, rel: &str) -> Result<FileContent> {
    let home = paths::home_dir().ok_or(Error::HomeDirUnavailable)?;
    read_entry_in(&home, tool, root_key, rel)
}

// ── 实现 ────────────────────────────────────────────────────────

fn is_known_tool(tool: &str) -> bool {
    crate::model::Tool::ALL.iter().any(|t| t.as_str() == tool)
}

fn inspect_in(home: &Path, tool: &str) -> ToolConfigReport {
    match tool {
        "claude-code" => claude_code(home),
        "codex" => codex(home),
        "grok" => grok(home),
        "zcode" => zcode(home),
        "opencode" => opencode(home),
        "codebuddy" => codebuddy(home),
        "pi" => pi(home),
        "workbuddy" => workbuddy(home),
        "dsh" => dsh(home),
        _ => ToolConfigReport {
            tool: tool.to_string(),
            home_label: String::new(),
            supported: false,
            mcp: vec![],
            skills: vec![],
            roots: vec![],
        },
    }
}

fn read_entry_in(home: &Path, tool: &str, root_key: &str, rel: &str) -> Result<FileContent> {
    if !is_known_tool(tool) {
        return Err(Error::Unsupported);
    }
    let root = root_path(home, tool, root_key)
        .ok_or_else(|| Error::Internal("toolconfig: unknown root key".into()))?;

    // rel 逐段校验：只允许普通段，`..`、绝对路径、隐藏的 `\` 一律拒绝——
    // 回读路径绝不能越出根目录。
    let mut path = root.clone();
    if !rel.is_empty() {
        for seg in rel.split('/') {
            if seg.is_empty()
                || seg == "."
                || seg == ".."
                || seg.contains('\\')
                || seg.contains(':')
            {
                return Err(Error::Internal("toolconfig: rel rejected".into()));
            }
            path.push(seg);
        }
    }
    let meta = std::fs::metadata(&path).map_err(|err| Error::DataFile {
        path: path.clone(),
        source: err,
    })?;
    if !meta.is_file() {
        return Err(Error::DataFile {
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "not a file"),
        });
    }
    if is_credential_path(&path) {
        return Err(Error::Unsupported);
    }
    let size = meta.len();
    let cap = CONTENT_MAX_BYTES as u64;
    let truncated = size > cap;
    let bytes = std::fs::read(&path).map_err(|err| Error::DataFile {
        path: path.clone(),
        source: err,
    })?;
    let bytes: &[u8] = if truncated {
        &bytes[..CONTENT_MAX_BYTES]
    } else {
        &bytes[..]
    };
    if bytes.contains(&b'\0') {
        return Err(Error::Unsupported);
    }
    Ok(FileContent {
        text: redact_text(&String::from_utf8_lossy(bytes)),
        truncated,
    })
}

/// 工具内根键 → 磁盘路径。只有这里登记的路径可以回读，越键即拒绝。
fn root_path(home: &Path, tool: &str, root_key: &str) -> Option<PathBuf> {
    match (tool, root_key) {
        ("claude-code", "home") => Some(home.join(".claude")),
        ("claude-code", "user-json") => Some(home.join(".claude.json")),
        ("codex", "home") => Some(home.join(".codex")),
        ("grok", "home") => Some(home.join(".grok")),
        ("zcode", "home") => Some(home.join(".zcode")),
        ("opencode", "home") => Some(home.join(".config").join("opencode")),
        ("codebuddy", "home") => Some(home.join(".codebuddy")),
        ("pi", "home") => Some(home.join(".pi").join("agent")),
        ("workbuddy", "home") => Some(home.join(".workbuddy")),
        ("dsh", "home") => Some(home.join(".dsh")),
        _ => None,
    }
}

// ── 各工具的装配 ────────────────────────────────────────────────

fn claude_code(home: &Path) -> ToolConfigReport {
    let dir = home.join(".claude");
    let json_path = home.join(".claude.json");
    let supported = dir.is_dir() || json_path.is_file();

    let mut mcp = vec![];
    if let Ok(text) = std::fs::read_to_string(&json_path)
        && let Ok(value) = serde_json::from_str::<Value>(&text)
    {
        mcp.extend(mcp_from_json_map(value.get("mcpServers"), "user", None));
        if let Some(projects) = value.get("projects").and_then(Value::as_object) {
            for (path, obj) in projects {
                mcp.extend(mcp_from_json_map(
                    obj.get("mcpServers"),
                    "project",
                    Some(path.clone()),
                ));
            }
        }
    }

    let mut skills = scan_skills_flat(&dir.join("skills"), "user", home, "home", &dir);
    skills.extend(scan_skills_tree(
        &dir.join("plugins"),
        "plugin",
        home,
        "home",
        &dir,
    ));

    ToolConfigReport {
        tool: "claude-code".into(),
        home_label: "~/.claude".into(),
        supported,
        mcp,
        skills,
        roots: vec![
            ConfigRoot {
                key: "home".into(),
                label: "~/.claude".into(),
                entries: list_tree(&dir),
            },
            ConfigRoot {
                key: "user-json".into(),
                label: "~/.claude.json".into(),
                entries: vec![],
            },
        ],
    }
}

fn codex(home: &Path) -> ToolConfigReport {
    let dir = home.join(".codex");
    let supported = dir.is_dir();
    let mcp = config_toml_mcp(&dir.join("config.toml"));
    let skills = scan_skills_flat(&dir.join("skills"), "user", home, "home", &dir);
    report_base("codex", "~/.codex", supported, mcp, skills, &dir)
}

fn grok(home: &Path) -> ToolConfigReport {
    let dir = home.join(".grok");
    let supported = dir.is_dir();
    let mcp = config_toml_mcp(&dir.join("config.toml"));
    let skills = scan_skills_tree(
        &dir.join("skills-marketplace"),
        "plugin",
        home,
        "home",
        &dir,
    );
    report_base("grok", "~/.grok", supported, mcp, skills, &dir)
}

fn zcode(home: &Path) -> ToolConfigReport {
    let dir = home.join(".zcode");
    let supported = dir.is_dir();
    let mut skills = scan_skills_flat(&dir.join("skills"), "user", home, "home", &dir);
    skills.extend(scan_skills_tree(
        &dir.join("cli").join("plugins"),
        "plugin",
        home,
        "home",
        &dir,
    ));
    // zcode 的 MCP 配置没有公开的文件位置（MCP 由插件注入），留空。
    report_base("zcode", "~/.zcode", supported, vec![], skills, &dir)
}

fn opencode(home: &Path) -> ToolConfigReport {
    let dir = home.join(".config").join("opencode");
    let supported = dir.is_dir();
    let mcp = read_json_file(&dir.join("opencode.json"))
        .map_or_else(Vec::new, |value| opencode_mcp(&value));
    let skills = scan_skills_flat(&dir.join("skill"), "user", home, "home", &dir);
    report_base(
        "opencode",
        "~/.config/opencode",
        supported,
        mcp,
        skills,
        &dir,
    )
}

fn codebuddy(home: &Path) -> ToolConfigReport {
    let dir = home.join(".codebuddy");
    let supported = dir.is_dir();
    let mcp = read_json_file(&dir.join("mcp.json")).map_or_else(Vec::new, |value| {
        mcp_from_json_map(value.get("mcpServers"), "user", None)
    });
    let mut skills = scan_skills_flat(&dir.join("skills"), "user", home, "home", &dir);
    skills.extend(scan_skills_tree(
        &dir.join("skills-marketplace"),
        "plugin",
        home,
        "home",
        &dir,
    ));
    report_base("codebuddy", "~/.codebuddy", supported, mcp, skills, &dir)
}

fn pi(home: &Path) -> ToolConfigReport {
    let dir = home.join(".pi").join("agent");
    let supported = dir.is_dir();
    let mcp = read_json_file(&dir.join("mcp.json")).map_or_else(Vec::new, |value| {
        mcp_from_json_map(value.get("mcpServers"), "user", None)
    });
    let skills = scan_skills_flat(&dir.join("skills"), "user", home, "home", &dir);
    report_base("pi", "~/.pi/agent", supported, mcp, skills, &dir)
}

fn workbuddy(home: &Path) -> ToolConfigReport {
    let dir = home.join(".workbuddy");
    let supported = dir.is_dir();
    let mcp = read_json_file(&dir.join("mcp.json")).map_or_else(Vec::new, |value| {
        mcp_from_json_map(value.get("mcpServers"), "user", None)
    });
    report_base("workbuddy", "~/.workbuddy", supported, mcp, vec![], &dir)
}

fn dsh(home: &Path) -> ToolConfigReport {
    let dir = home.join(".dsh");
    let supported = dir.is_dir();
    let mcp = read_json_file(&dir.join("mcp.json")).map_or_else(Vec::new, |value| {
        mcp_from_json_map(value.get("mcpServers"), "user", None)
    });
    report_base("dsh", "~/.dsh", supported, mcp, vec![], &dir)
}

fn report_base(
    tool: &str,
    label: &str,
    supported: bool,
    mcp: Vec<McpServerView>,
    skills: Vec<SkillView>,
    dir: &Path,
) -> ToolConfigReport {
    ToolConfigReport {
        tool: tool.to_string(),
        home_label: label.to_string(),
        supported,
        mcp,
        skills,
        roots: vec![ConfigRoot {
            key: "home".into(),
            label: label.to_string(),
            entries: list_tree(dir),
        }],
    }
}

// ── MCP 提取 ────────────────────────────────────────────────────

/// 读一个 JSON 配置文件；文件缺失或解析失败都视为无配置（配置写坏不该把
/// 配置页打挂），不区分报错。
fn read_json_file(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// JSON 形态的 `mcpServers` 表（claude/codebuddy/pi/dsh/workbuddy 同构）。
/// 条目字段：`command` + `args[]`、`url`、`type`、`env{}`；只提取连接元数据，
/// 其余字段（含 env 值）一律丢弃。
fn mcp_from_json_map(
    value: Option<&Value>,
    scope: &str,
    project: Option<String>,
) -> Vec<McpServerView> {
    let Some(map) = value.and_then(Value::as_object) else {
        return vec![];
    };
    let mut out = vec![];
    for (name, entry) in map {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let url = entry.get("url").and_then(Value::as_str);
        let command = match (
            entry.get("command").and_then(Value::as_str),
            entry.get("args"),
        ) {
            (None, _) => None,
            (Some(cmd), args) => Some(join_command(cmd, args)),
        };
        let transport = entry
            .get("type")
            .and_then(Value::as_str)
            .filter(|t| matches!(*t, "stdio" | "http" | "sse"))
            .map_or_else(
                || if url.is_some() { "http" } else { "stdio" }.to_string(),
                str::to_string,
            );
        out.push(McpServerView {
            name: name.clone(),
            transport,
            scope: scope.to_string(),
            command: command.map(|c| redact_line(&c)),
            url: url.map(redact_url),
            env_keys: env_keys_of(entry.get("env")),
            project: project.clone(),
        });
    }
    out
}

/// opencode 的 `mcp` 表：`type: "local"|"remote"`，local 走 `command[]`，
/// remote 走 `url`。
fn opencode_mcp(value: &Value) -> Vec<McpServerView> {
    let Some(map) = value.get("mcp").and_then(Value::as_object) else {
        return vec![];
    };
    let mut out = vec![];
    for (name, entry) in map {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let is_remote = entry.get("type").and_then(Value::as_str) == Some("remote");
        let url = entry.get("url").and_then(Value::as_str);
        let command = entry.get("command").and_then(Value::as_array).map(|args| {
            let cmd = args
                .first()
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let rest = Value::Array(args[1.min(args.len())..].to_vec());
            join_command(&cmd, Some(&rest))
        });
        out.push(McpServerView {
            name: name.clone(),
            transport: if is_remote { "http" } else { "stdio" }.to_string(),
            scope: "user".into(),
            command: command.map(|c| redact_line(&c)),
            url: url.map(redact_url),
            env_keys: env_keys_of(entry.get("env")),
            project: None,
        });
    }
    out
}

/// TOML 形态的 `[mcp_servers.<name>]` 段（codex/grok 同构）。解析失败整体
/// 视为无 MCP（配置可能尚未写这段），不向页面报错。
fn config_toml_mcp(path: &Path) -> Vec<McpServerView> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return vec![];
    };
    let Some(servers) = table.get("mcp_servers").and_then(toml::Value::as_table) else {
        return vec![];
    };
    let mut out = vec![];
    for (name, entry) in servers {
        let Some(entry) = entry.as_table() else {
            continue;
        };
        let command = entry
            .get("command")
            .and_then(toml::Value::as_str)
            .map(|cmd| {
                let args = entry
                    .get("args")
                    .and_then(toml::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let args_value = toml_to_json(&toml::Value::Array(args));
                join_command(cmd, Some(&args_value))
            });
        let url = entry.get("url").and_then(toml::Value::as_str);
        out.push(McpServerView {
            name: name.clone(),
            transport: if url.is_some() { "http" } else { "stdio" }.to_string(),
            scope: "user".into(),
            command: command.map(|c| redact_line(&c)),
            url: url.map(redact_url),
            env_keys: entry
                .get("env")
                .and_then(toml::Value::as_table)
                .map(|env| env.keys().cloned().collect())
                .unwrap_or_default(),
            project: None,
        });
    }
    out
}

/// toml::Value → serde_json::Value（只用于 args 数组，规模可控）。
fn toml_to_json(value: &toml::Value) -> Value {
    match value {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::from(*b),
        toml::Value::Array(items) => Value::Array(items.iter().map(toml_to_json).collect()),
        toml::Value::Table(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
        toml::Value::Datetime(dt) => Value::String(dt.to_string()),
    }
}

fn join_command(command: &str, args: Option<&Value>) -> String {
    let mut parts = vec![command.to_string()];
    if let Some(args) = args.and_then(Value::as_array) {
        parts.extend(args.iter().filter_map(Value::as_str).map(str::to_string));
    }
    shell_join(&parts)
}

/// 展示用的轻量拼接：带空白的段加引号。不是 shell 语法解析，只是可读性。
fn shell_join(parts: &[String]) -> String {
    parts
        .iter()
        .map(|p| {
            if p.contains(' ') {
                format!("\"{p}\"")
            } else {
                p.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn env_keys_of(env: Option<&Value>) -> Vec<String> {
    env.and_then(Value::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default()
}

// ── 脱敏 ────────────────────────────────────────────────────────

/// 键名/参数名疑似携带密钥。宁可错杀：URL 查询串与 KV 值的泄露不可逆。
fn is_secret_name(name: &str) -> bool {
    const HINTS: [&str; 7] = [
        "key", "token", "secret", "password", "passwd", "auth", "apikey",
    ];
    let lower = name.to_ascii_lowercase();
    HINTS.iter().any(|h| lower.contains(h))
}

/// URL 查询串脱敏：疑似密钥参数的值替换为 `***`，其余原样。
fn redact_url(url: &str) -> String {
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let pairs: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if is_secret_name(k) => format!("{k}=***"),
            _ => pair.to_string(),
        })
        .collect();
    format!("{base}?{}", pairs.join("&"))
}

/// 单行文本脱敏：URL 查询串 + KV 键值对（JSON `"k": "v"`、TOML `k = "v"`、
/// dotenv `K=v`，键带不带引号都认）。内容查看器对每一行过这个闸。
fn redact_line(line: &str) -> String {
    mask_kv_pairs(&redact_url_in_line(line))
}

fn redact_text(text: &str) -> String {
    text.lines().map(redact_line).collect::<Vec<_>>().join("\n")
}

/// 行内 URL 查询串脱敏（内容查看器用；独立 URL 走 [`redact_url`]）。
fn redact_url_in_line(line: &str) -> String {
    // 只处理 https?:// 到行尾/空白前的段，避免误伤普通文本里的 `?`。
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(pos) = rest.find("http://").or_else(|| rest.find("https://")) {
        let (before, url_part) = rest.split_at(pos);
        out.push_str(before);
        let end = url_part
            .find([' ', '\t', '"', '\''])
            .unwrap_or(url_part.len());
        let (url, after) = url_part.split_at(end);
        out.push_str(&redact_url(url));
        rest = after;
    }
    out.push_str(rest);
    out
}

/// KV 键值对脱敏：一次扫描同时覆盖带引号键（JSON）与标识符键（TOML/dotenv）。
/// 疑似密钥键的值替换为 `***`，其余原样重建。
fn mask_kv_pairs(line: &str) -> String {
    let bytes = line.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            // 引号键："key" : "value"
            let Some(rel) = line[i + 1..].find('"') else {
                out.push_str(&line[i..]);
                break;
            };
            let key = &line[i + 1..i + 1 + rel];
            let after_key = i + 1 + rel + 1;
            let rest = &line[after_key..];
            let trimmed = rest.trim_start();
            let value = trimmed
                .strip_prefix(':')
                .or_else(|| trimmed.strip_prefix('='))
                .map(str::trim_start);
            match value {
                Some(v) if v.starts_with('"') => match v[1..].find('"') {
                    Some(vrel) => {
                        let sep = trimmed.as_bytes()[0] as char;
                        out.push('"');
                        out.push_str(key);
                        out.push('"');
                        out.push_str(&rest[..rest.len() - trimmed.len()]);
                        out.push(sep);
                        out.push_str(&trimmed[1..trimmed.len() - v.len()]);
                        out.push('"');
                        out.push_str(if is_secret_name(key) {
                            "***"
                        } else {
                            &v[1..1 + vrel]
                        });
                        out.push('"');
                        i = line.len() - v.len() + 1 + vrel + 1;
                    }
                    None => {
                        out.push_str(&line[i..]);
                        break;
                    }
                },
                _ => {
                    out.push('"');
                    out.push_str(key);
                    out.push('"');
                    i = after_key;
                }
            }
        } else if is_ident(bytes[i]) {
            // 标识符键：key = "value"（TOML）或 KEY=value（dotenv）
            let mut j = i;
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            let key = &line[i..j];
            let rest = &line[j..];
            let trimmed = rest.trim_start();
            let bare_value = trimmed
                .strip_prefix('=')
                .filter(|v| !v.starts_with('='))
                .map(str::trim_start);
            if let Some(v) = bare_value {
                let ws_before = &rest[..rest.len() - trimmed.len()];
                let ws_after = &trimmed[1..trimmed.len() - v.len()];
                if let Some(vrel) = v.strip_prefix('"').and_then(|s| s.find('"')) {
                    out.push_str(key);
                    out.push_str(ws_before);
                    out.push('=');
                    out.push_str(ws_after);
                    out.push('"');
                    out.push_str(if is_secret_name(key) {
                        "***"
                    } else {
                        &v[1..1 + vrel]
                    });
                    out.push('"');
                    i = line.len() - v.len() + 1 + vrel + 1;
                    continue;
                }
                if is_secret_name(key) {
                    // 裸值到 token 终止符为止（dotenv 值不含空白，行内
                    // 复合表达式以 `&` 等收尾）。
                    out.push_str(key);
                    out.push_str(ws_before);
                    out.push('=');
                    out.push_str(ws_after);
                    out.push_str("***");
                    i = line.len() - v.len()
                        + v.find([' ', '\t', '&', ';', ',', ')']).unwrap_or(v.len());
                    continue;
                }
            }
            out.push_str(key);
            i = j;
        } else {
            let ch_len = line[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&line[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

// ── Skills ──────────────────────────────────────────────────────

/// 平铺布局：`<dir>/<name>/SKILL.md`。`root_key`/`root_dir` 是技能所在
/// 文件树根（内容回读定位用），`root_dir` 必须是 `dir` 的祖先。
fn scan_skills_flat(
    dir: &Path,
    scope: &str,
    home: &Path,
    root_key: &str,
    root_dir: &Path,
) -> Vec<SkillView> {
    let mut out = vec![];
    let Ok(read) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in read.flatten() {
        let skill_md = entry.path().join("SKILL.md");
        if skill_md.is_file() {
            push_skill(&mut out, &skill_md, scope, home, root_key, root_dir);
        }
    }
    out
}

/// 树形布局：marketplace/plugin 缓存里任意深度的 `**/skills/<name>/SKILL.md`
/// 与直接命名为 `SKILL.md` 的文件。跳过依赖目录，条目有上限。
fn scan_skills_tree(
    dir: &Path,
    scope: &str,
    home: &Path,
    root_key: &str,
    root_dir: &Path,
) -> Vec<SkillView> {
    let mut out = vec![];
    if !dir.is_dir() {
        return out;
    }
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(8)
        .into_iter()
        .filter_entry(|e| e.file_name().to_string_lossy().to_lowercase() != "node_modules")
        .flatten()
    {
        if entry.file_type().is_file() && entry.file_name() == "SKILL.md" {
            push_skill(&mut out, entry.path(), scope, home, root_key, root_dir);
            if out.len() >= ENTRIES_MAX {
                break;
            }
        }
    }
    out
}

fn push_skill(
    out: &mut Vec<SkillView>,
    skill_md: &Path,
    scope: &str,
    home: &Path,
    root_key: &str,
    root_dir: &Path,
) {
    let Some(name) = skill_md.parent().and_then(Path::file_name) else {
        return;
    };
    // 根内相对路径：抽屉详情按 (root, rel) 回读 SKILL.md。前缀不在根下
    // （理论不可能，扫描目录都从根派生）时退化为空串，前端禁用回读。
    let rel = skill_md
        .strip_prefix(root_dir)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    out.push(SkillView {
        name: name.to_string_lossy().to_string(),
        scope: scope.to_string(),
        description: skill_description(skill_md),
        path: home_label_of(skill_md.parent().unwrap_or(skill_md), home),
        root: root_key.to_string(),
        rel,
    });
}

/// SKILL.md frontmatter 的 `description:` 行。只取单行值（多行 `>`/`|` 形态
/// 取首行）；解析失败静默返回 `None`——描述缺失不影响技能列出。
fn skill_description(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        if let Some(rest) = trimmed.strip_prefix("description:") {
            let desc = rest.trim().trim_matches(['"', '\'']).trim();
            let truncated: String = if desc.chars().count() > SKILL_DESC_MAX_CHARS {
                desc.chars().take(SKILL_DESC_MAX_CHARS).collect::<String>() + "…"
            } else {
                desc.to_string()
            };
            return Some(truncated);
        }
    }
    None
}

/// 路径 → `~/...` 展示标签。
fn home_label_of(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.to_string_lossy().replace('\\', "/")),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

// ── 文件树 ──────────────────────────────────────────────────────

/// 根目录的概貌树：深度 3、条目 400 上限，目录优先按字母序。
fn list_tree(dir: &Path) -> Vec<FileEntry> {
    if !dir.is_dir() {
        return vec![];
    }
    let mut out = vec![];
    let root_label_len = dir.to_string_lossy().len() + 1;
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(TREE_MAX_DEPTH)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            !e.file_type().is_dir()
                || !SKIP_DIRS
                    .iter()
                    .any(|s| e.file_name().eq_ignore_ascii_case(s))
        })
        .flatten()
    {
        if entry.depth() == 0 {
            continue;
        }
        let rel = entry.path().to_string_lossy()[root_label_len..].replace('\\', "/");
        out.push(FileEntry {
            rel,
            name: entry.file_name().to_string_lossy().to_string(),
            dir: entry.file_type().is_dir(),
            size: entry
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len()),
        });
        if out.len() >= ENTRIES_MAX {
            break;
        }
    }
    out
}

// ── 凭据识别 ────────────────────────────────────────────────────

/// 凭据类文件拒读。子串匹配 + 扩展名，宁可错杀：内容展示不可逆。
fn is_credential_path(path: &Path) -> bool {
    const HINTS: [&str; 7] = [
        "auth",
        "credential",
        "keyblob",
        "passwd",
        "password",
        "secret",
        "cap_sid",
    ];
    const EXTENSIONS: [&str; 5] = ["pem", "key", "p12", "pfx", "crt"];
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if HINTS.iter().any(|h| name.contains(h))
        || name == ".env"
        || name.starts_with(".env.")
        || name == ".claude.json"
    {
        return true;
    }
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[cfg(test)]
mod tests;
