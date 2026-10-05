//! 统一用量模型：工具枚举与跨适配器共享的解析产物类型。
//! 陷阱：[`TokenUsage::output_tokens`] 不得含推理部分——reasoning 按输出价计费，
//! 含了会双算。

/// 9 个受支持的 AI 编程工具。[`Self::as_str`] 的值进数据库 `tool` 列，
/// 取值即契约：只增不改；[`crate::adapter::adapters`] 注册表与它一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    /// Anthropic Claude Code。
    ClaudeCode,
    /// OpenAI Codex CLI。
    Codex,
    /// OpenCode。
    OpenCode,
    /// ZCode。
    ZCode,
    /// 腾讯 CodeBuddy。
    CodeBuddy,
    /// WorkBuddy。
    WorkBuddy,
    /// Grok CLI。
    Grok,
    /// Pi。
    Pi,
    /// DSH。
    Dsh,
}

impl Tool {
    /// 全部工具，顺序即展示顺序。
    pub const ALL: [Tool; 9] = [
        Tool::ClaudeCode,
        Tool::Codex,
        Tool::OpenCode,
        Tool::ZCode,
        Tool::CodeBuddy,
        Tool::WorkBuddy,
        Tool::Grok,
        Tool::Pi,
        Tool::Dsh,
    ];

    /// 数据库 `tool` 列存的标识，跨语言契约。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude-code",
            Tool::Codex => "codex",
            Tool::OpenCode => "opencode",
            Tool::ZCode => "zcode",
            Tool::CodeBuddy => "codebuddy",
            Tool::WorkBuddy => "workbuddy",
            Tool::Grok => "grok",
            Tool::Pi => "pi",
            Tool::Dsh => "dsh",
        }
    }
}

/// 一次请求的 token 分桶。`output_tokens` 不含推理部分；`reasoning_tokens` 是从输出
/// 拆出的明细（仍按输出价计费），不是额外增量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// 输入桶计数。
    pub input_tokens: i64,
    /// 输出桶计数（净量，不含推理）。
    pub output_tokens: i64,
    /// 缓存读桶计数。
    pub cache_read_tokens: i64,
    /// 缓存写桶计数。
    pub cache_write_tokens: i64,
    /// 推理明细；老日志或无推理时为 `None`。
    pub reasoning_tokens: Option<i64>,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// tool 值进数据库并作为跨语言契约，必须是干净的单词标识。
    #[test]
    fn tool_names_are_clean_identifiers() {
        for tool in Tool::ALL {
            let name = tool.as_str();
            assert!(!name.is_empty());
            assert_eq!(name.trim(), name);
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name:?} 含非法字符"
            );
        }

        let unique: HashSet<&str> = Tool::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(unique.len(), Tool::ALL.len(), "工具名必须互不相同");
    }
}
