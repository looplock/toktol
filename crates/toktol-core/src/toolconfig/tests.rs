//! toolconfig 的行为测试：脱敏口径、MCP 提取、文件树与内容回读的闸门。
//! 全部走 `inspect_in` / `read_entry_in`（注入假 home），不碰真实用户目录。

use std::fs;
use std::path::Path;

use super::{inspect_in, read_entry_in};

fn fake_home(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toktol-toolconfig-test-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("建假 home");
    dir
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().expect("有父目录")).expect("建目录");
    fs::write(path, text).expect("写文件");
}

#[test]
fn unknown_tool_is_rejected() {
    let home = fake_home("unknown");
    let report = inspect_in(&home, "nope");
    assert!(!report.supported);
    assert!(report.roots.is_empty());
    // 回读同样拒绝未知工具。
    assert!(read_entry_in(&home, "nope", "home", "x").is_err());
}

#[test]
fn uninstalled_tool_reports_unsupported() {
    let home = fake_home("missing");
    let report = inspect_in(&home, "codex");
    assert!(!report.supported);
    assert!(report.mcp.is_empty());
    assert!(report.roots.iter().all(|r| r.entries.is_empty()));
}

#[test]
fn claude_mcp_metadata_is_extracted_and_redacted() {
    let home = fake_home("claude");
    write(
        &home.join(".claude.json"),
        r#"{
  "mcpServers": {
    "tavily": {
      "type": "http",
      "url": "https://mcp.tavily.com/mcp/?tavilyApiKey=tvly-secret-1&layout=default"
    },
    "fs": {
      "command": "npx",
      "args": ["-y", "mcp-server-fs"],
      "env": {"FS_ALLOWED": "/tmp"}
    }
  },
  "projects": {
    "E:\\work": {"mcpServers": {"proj": {"command": "node", "args": ["srv.js"]}}}
  },
  "customApiKeyResponses": {"approved": ["sk-ant-xxx"]}
}"#,
    );
    write(&home.join(".claude").join("settings.json"), "{}");

    let report = inspect_in(&home, "claude-code");
    assert!(report.supported);
    assert_eq!(report.home_label, "~/.claude");
    assert_eq!(report.mcp.len(), 3);

    let pick = |name: &str| {
        report
            .mcp
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("缺少 {name}"))
    };
    let tavily = pick("tavily");
    assert_eq!(tavily.transport, "http");
    assert_eq!(
        tavily.url.as_deref(),
        Some("https://mcp.tavily.com/mcp/?tavilyApiKey=***&layout=default")
    );

    let fs_server = pick("fs");
    assert_eq!(fs_server.transport, "stdio");
    assert_eq!(fs_server.command.as_deref(), Some("npx -y mcp-server-fs"));
    assert_eq!(fs_server.env_keys, vec!["FS_ALLOWED".to_string()]);

    let proj = pick("proj");
    assert_eq!(proj.scope, "project");
    assert_eq!(proj.project.as_deref(), Some("E:\\work"));
}

#[test]
fn codex_mcp_from_config_toml() {
    let home = fake_home("codex");
    write(
        &home.join(".codex").join("config.toml"),
        "model = \"gpt-5\"\n\n[mcp_servers.tavily]\ncommand = \"npx\"\nargs = [\"-y\", \"mcp-remote\", \"https://mcp.tavily.com/mcp/?tavilyApiKey=tvly-x\"]\nenv = { FS_ALLOWED = \"/tmp\" }\n",
    );
    write(
        &home
            .join(".codex")
            .join("skills")
            .join("review")
            .join("SKILL.md"),
        "---\nname: review\ndescription: \"Review code\"\n---\nbody",
    );

    let report = inspect_in(&home, "codex");
    assert!(report.supported);
    assert_eq!(report.mcp.len(), 1);
    let server = &report.mcp[0];
    assert_eq!(server.transport, "stdio");
    assert_eq!(
        server.command.as_deref(),
        Some("npx -y mcp-remote https://mcp.tavily.com/mcp/?tavilyApiKey=***")
    );
    assert_eq!(server.env_keys, vec!["FS_ALLOWED".to_string()]);

    assert_eq!(report.skills.len(), 1);
    assert_eq!(report.skills[0].name, "review");
    assert_eq!(report.skills[0].description.as_deref(), Some("Review code"));
    assert!(report.skills[0].path.starts_with("~/.codex/skills/"));
}

#[test]
fn opencode_local_and_remote_mcp() {
    let home = fake_home("opencode");
    write(
        &home.join(".config").join("opencode").join("opencode.json"),
        r#"{"mcp": {"local-one": {"type": "local", "command": ["bun", "x", "srv"]}, "remote-one": {"type": "remote", "url": "https://x.dev/mcp?apiKey=k-1"}}}"#,
    );
    let report = inspect_in(&home, "opencode");
    assert_eq!(report.mcp.len(), 2);
    let pick = |name: &str| {
        report
            .mcp
            .iter()
            .find(|m| m.name == name)
            .unwrap_or_else(|| panic!("缺少 {name}"))
    };
    assert_eq!(pick("local-one").transport, "stdio");
    assert_eq!(pick("local-one").command.as_deref(), Some("bun x srv"));
    assert_eq!(pick("remote-one").transport, "http");
    assert_eq!(
        pick("remote-one").url.as_deref(),
        Some("https://x.dev/mcp?apiKey=***")
    );
}

#[test]
fn file_tree_lists_depth_and_skips_deps() {
    let home = fake_home("tree");
    write(&home.join(".codex").join("config.toml"), "x = 1");
    write(
        &home
            .join(".codex")
            .join("skills")
            .join("a")
            .join("SKILL.md"),
        "d",
    );
    write(
        &home
            .join(".codex")
            .join("vendor")
            .join("node_modules")
            .join("pkg")
            .join("index.js"),
        "js",
    );
    let report = inspect_in(&home, "codex");
    let entries = &report.roots[0].entries;
    assert!(entries.iter().any(|e| e.rel == "config.toml" && !e.dir));
    assert!(entries.iter().any(|e| e.rel == "skills/a" && e.dir));
    assert!(entries.iter().any(|e| e.rel == "skills/a/SKILL.md"));
    assert!(!entries.iter().any(|e| e.rel.contains("node_modules")));
    // 排序：目录在前。
    let first_dir = entries.iter().position(|e| e.dir).expect("有目录");
    assert!(entries[..first_dir].iter().all(|e| !e.dir));
}

#[test]
fn content_read_redacts_and_gates() {
    let home = fake_home("content");
    write(
        &home.join(".codex").join("settings.toml"),
        "api_key = \"sk-live-123\"\nnormal = \"keep\"\n# url https://x.dev/v1?token=t-9",
    );
    write(&home.join(".codex").join("auth.json"), "{\"oauth\": true}");
    fs::write(home.join(".codex").join("blob.bin"), [0u8, 1, 2]).expect("写二进制");

    let ok = read_entry_in(&home, "codex", "home", "settings.toml").expect("普通文件可读");
    assert!(ok.text.contains("api_key = \"***\""));
    assert!(ok.text.contains("normal = \"keep\""));
    assert!(ok.text.contains("https://x.dev/v1?token=***"));
    assert!(!ok.truncated);

    // 凭据类文件名拒读；二进制拒读；路径越界拒绝；未知根键拒绝。
    assert!(matches!(
        read_entry_in(&home, "codex", "home", "auth.json"),
        Err(crate::error::Error::Unsupported)
    ));
    assert!(read_entry_in(&home, "codex", "home", "blob.bin").is_err());
    assert!(read_entry_in(&home, "codex", "home", "../.codex/auth.json").is_err());
    assert!(read_entry_in(&home, "codex", "home", "..\\.codex\\auth.json").is_err());
    assert!(read_entry_in(&home, "codex", "nope", "settings.toml").is_err());

    // 目录不是文件。
    assert!(read_entry_in(&home, "codex", "home", "skills").is_err());
}

#[test]
fn claude_json_root_is_not_offered_and_credential_denial_still_holds() {
    let home = fake_home("claudejson");
    write(&home.join(".claude.json"), "{\"mcpServers\": {}}");
    write(
        &home.join(".claude").join("settings.json"),
        "{\"model\": \"opus\"}",
    );

    let report = inspect_in(&home, "claude-code");
    assert!(
        report.roots.iter().all(|r| r.key != "user-json"),
        "user-json 根已删除：永远被凭据闸门拒读的根不该出现在报告里"
    );

    // .claude.json 内含 OAuth 与 API key 记录，凭据闸门继续兜底（哪怕将来
    // 有人把它误登记回某个根）；树里登记的 settings.json 正常可读。
    assert!(super::is_credential_path(&home.join(".claude.json")));
    let settings =
        read_entry_in(&home, "claude-code", "home", "settings.json").expect("settings 可读");
    assert!(settings.text.contains("\"model\": \"opus\""));
}

#[test]
fn oversized_content_is_truncated() {
    let home = fake_home("oversize");
    let big = "x = \"y\"\n".repeat(60_000);
    write(&home.join(".codex").join("big.toml"), &big);
    let content = read_entry_in(&home, "codex", "home", "big.toml").expect("可读");
    assert!(content.truncated);
    assert!(content.text.len() < super::CONTENT_MAX_BYTES);
    assert!(content.text.starts_with("x = \"y\""));
}

#[test]
fn redaction_masks_json_toml_env_and_url_forms() {
    assert_eq!(
        super::redact_line("\"apiKey\": \"sk-1\", \"retries\": 3"),
        "\"apiKey\": \"***\", \"retries\": 3"
    );
    assert_eq!(
        super::redact_line("access_token = \"t-1\""),
        "access_token = \"***\""
    );
    assert_eq!(
        super::redact_line("ANTHROPIC_API_KEY=sk-2"),
        "ANTHROPIC_API_KEY=***"
    );
    assert_eq!(
        super::redact_line("cmd https://mcp.dev/mcp?secret=s-1&a=b"),
        "cmd https://mcp.dev/mcp?secret=***&a=b"
    );
    // 无关键名不动。
    assert_eq!(
        super::redact_line("\"model\": \"opus\""),
        "\"model\": \"opus\""
    );
}
