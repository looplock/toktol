//! 定价引擎：标准模型名归一、models.dev 公共目录接入。
//! 红线：目录里没有的模型绝不估价——分项保持 NULL，前端显示"未知"。

pub mod catalog;

/// 标准模型名（canonical id）的格式归一：全小写、`-` 连接、去 `vendor/` 前缀
/// （`z-ai/glm-5.3-flash` → `glm-5.3-flash`）、去 `:latest` 尾缀、`_` 与空格折叠成 `-`。
/// 只做**无归属判断**的格式统一——归属修正（哪个变种属于哪个标准模型）是
/// 映射与定价页上用户确认的事，这里绝不猜。各适配器的专门规则（如 claude
/// 去日期后缀）仍在前置，本函数是入库建议值的最后一道。
pub fn canonicalize_model_id(raw: &str) -> String {
    let text = raw.trim().to_lowercase();
    let text = text.rsplit('/').next().unwrap_or(&text);
    let text = text.strip_suffix(":latest").unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let mut prev_dash = false;
    for ch in text.chars() {
        let is_dash = ch == '-' || ch == '_' || ch == ' ';
        if is_dash && prev_dash {
            continue;
        }
        if is_dash {
            out.push('-');
        } else {
            out.push(ch);
        }
        prev_dash = is_dash;
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        raw.trim().to_lowercase()
    } else {
        out
    }
}

/// 启发候选：从归一名反复剥掉"修饰性"尾缀得到的基础名，按剥离深度顺序返回
/// （不含原值，末位是剥得最深的母型号）。目录命中才被采纳是首选；目录没收录
/// 时也取末位候选标 `suggested` 待用户确认（产品决策：免费/日期等修饰尾缀的
/// 母型号语义明确，剥错用户一键改回，绝不静默）。
pub fn heuristic_candidates(canonical: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = canonical.to_string();
    while let Some(next) = strip_one_suffix(&current) {
        out.push(next.clone());
        current = next;
    }
    out
}

/// 剥一层尾缀。返回 None 表示当前名没有可剥的了。
fn strip_one_suffix(text: &str) -> Option<String> {
    if let Some(base) = text.strip_suffix(":free")
        && !base.is_empty()
    {
        return Some(base.to_string());
    }
    for modifier in ["-thinking", "-high", "-low", "-medium", ":free", "-free"] {
        if let Some(base) = text.strip_suffix(modifier)
            && !base.is_empty()
        {
            return Some(base.to_string());
        }
    }
    // 纯数字版本尾缀（≥3 位）：gpt-4-0613、deepseek-v3-0324、…-20240620。
    if let Some((base, digits)) = split_trailing_digit_group(text)
        && digits.len() >= 3
    {
        return Some(base);
    }
    // 日期对 -MM-DD（两位、月份 ≤12、日期 ≤31）：gemini-2.5-pro-06-05。
    // 两位零填充 + 范围守卫是必须的，否则 claude-sonnet-4-5 会被剥成 claude-sonnet。
    if let Some(base) = strip_month_day_suffix(text) {
        return Some(base);
    }
    None
}

/// `-<纯数字>` 结尾且数字 ≥3 位：返回（去掉尾缀的名字, 数字段）。
fn split_trailing_digit_group(text: &str) -> Option<(String, &str)> {
    let pos = text.rfind('-')?;
    let digits = &text[pos + 1..];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((text[..pos].to_string(), digits))
}

/// `-MM-DD` 结尾：两个两位零填充数字段，前者 ≤12（月）、后者 ≤31（日）。
fn strip_month_day_suffix(text: &str) -> Option<String> {
    let last = text.rfind('-')?;
    let day = &text[last + 1..];
    if day.len() != 2 || !day.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let head = &text[..last];
    let prev = head.rfind('-')?;
    let month = &head[prev + 1..];
    if month.len() != 2 || !month.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month_v: u32 = month.parse().ok()?;
    let day_v: u32 = day.parse().ok()?;
    if month_v == 0 || month_v > 12 || day_v == 0 || day_v > 31 || prev == 0 {
        return None;
    }
    Some(head[..prev].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_id_is_lowercase_dash_joined_without_vendor_prefix() {
        assert_eq!(
            canonicalize_model_id("deepseek/deepseek-v4-flash"),
            "deepseek-v4-flash"
        );
        assert_eq!(canonicalize_model_id("Z-AI/Glm_5.3_Flash"), "glm-5.3-flash");
        assert_eq!(canonicalize_model_id("  GPT-5.6:latest  "), "gpt-5.6");
        assert_eq!(canonicalize_model_id("GLM 5.3 Flash"), "glm-5.3-flash");
        assert_eq!(canonicalize_model_id("claude-opus-4-1"), "claude-opus-4-1");
        // 前后缀连字符折叠；无法成名的输入原样小写返回，不编造。
        assert_eq!(canonicalize_model_id("-glm-"), "glm");
        assert_eq!(canonicalize_model_id("-/:"), ":");
    }

    #[test]
    fn heuristic_candidates_strip_modifiers_and_dates_only() {
        assert_eq!(
            heuristic_candidates("claude-sonnet-4-5-thinking"),
            ["claude-sonnet-4-5"]
        );
        assert_eq!(
            heuristic_candidates("deepseek-chat:free"),
            ["deepseek-chat"]
        );
        assert_eq!(heuristic_candidates("mimo-v2.5-free"), ["mimo-v2.5"]);
        assert_eq!(heuristic_candidates("deepseek-v3-0324"), ["deepseek-v3"]);
        assert_eq!(
            heuristic_candidates("gpt-4.1-2025-04-14"),
            ["gpt-4.1-2025", "gpt-4.1"]
        );
        // 守卫：真实版本号不是日期——claude-sonnet-4-5 不许被剥。
        assert!(heuristic_candidates("claude-sonnet-4-5").is_empty());
        assert!(heuristic_candidates("glm-5.3-flash").is_empty());
    }
}
