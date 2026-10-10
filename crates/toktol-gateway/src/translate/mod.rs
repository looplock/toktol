//! 聊天补全协议转换：OpenAI Chat Completions ↔ Anthropic Messages ↔ OpenAI
//! Responses 三方互转，请求、非流式响应与 SSE 流式三套都覆盖。同协议对字节
//! 透传，不做字段级转换。
//!
//! 全部纯函数/无 IO，确定性契约与 core 的适配器一致：同输入恒同输出（计量靠
//! 重放解析，结果不稳定会重复入账）。请求/响应体用 [`serde_json::Value`] 而非
//! 类型化模型：两侧字段集合都远大于我们关心的子集，Value 透传未知字段比维护
//! 全量类型更不易丢信息。
//!
//! 流式转换的已知取舍：OpenAI 上游的 input usage 要到流末尾才出现，转成
//! Anthropic 事件时 `message_start` 里的 input_tokens 为 0、真实值在 `message_delta`
//! 补上——计量不受影响（计量直接从上游事件抽取，见 [`UsageScanner`]）。
//! 推理内容双向走 reasoning_content ↔ thinking 块（含流式的 thinking_delta）；
//! Anthropic 的 redacted_thinking 与 signature 无对端概念，丢弃；Responses 的
//! reasoning summary → thinking / reasoning_content 是尽力回放（见 responses.rs）。

use serde_json::{Value, json};

use toktol_core::error::{Error, Result};
use toktol_core::model::TokenUsage;

use super::config::Protocol;

pub mod gemini;
pub mod responses;

/// Anthropic 上游要求的版本头值；写死官方当前稳定版。
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

fn bad(message: impl Into<String>) -> Error {
    Error::Internal(format!("translate: {}", message.into()))
}

/// reasoning_effort 档位 ↔ Anthropic thinking 预算的约定映射（双向共用一张表），
/// 取值刻意保守；预算语义见 openai_request_to_anthropic。
fn effort_to_budget(effort: &str) -> Option<i64> {
    match effort {
        "low" => Some(1024),
        "medium" => Some(4096),
        "high" => Some(16384),
        _ => None,
    }
}

fn budget_to_effort(budget: i64) -> &'static str {
    if budget <= 2048 {
        "low"
    } else if budget <= 8192 {
        "medium"
    } else {
        "high"
    }
}

/// 把入站请求体翻译成上游协议的请求体；协议一致时原样返回。
pub fn translate_request(inbound: Protocol, upstream: Protocol, body: Value) -> Result<Value> {
    match (inbound, upstream) {
        (Protocol::OpenAI, Protocol::OpenAI)
        | (Protocol::Anthropic, Protocol::Anthropic)
        | (Protocol::Responses, Protocol::Responses)
        | (Protocol::Gemini, Protocol::Gemini) => Ok(body),
        (Protocol::OpenAI, Protocol::Anthropic) => openai_request_to_anthropic(body),
        (Protocol::Anthropic, Protocol::OpenAI) => anthropic_request_to_openai(body),
        (Protocol::Responses, Protocol::OpenAI) => responses::request_to_openai_chat(body),
        (Protocol::Responses, Protocol::Anthropic) => responses::request_to_anthropic(body),
        (Protocol::OpenAI, Protocol::Responses) => responses::openai_chat_to_request(body),
        (Protocol::Anthropic, Protocol::Responses) => responses::anthropic_to_request(body),
        (Protocol::OpenAI, Protocol::Gemini) => gemini::openai_chat_to_gemini(body),
        (Protocol::Anthropic, Protocol::Gemini) => gemini::anthropic_to_gemini(body),
        (Protocol::Responses, Protocol::Gemini) => gemini::responses_to_gemini(body),
        (Protocol::Gemini, Protocol::OpenAI) => gemini::gemini_to_openai_request(body),
        (Protocol::Gemini, Protocol::Anthropic) => gemini::gemini_to_anthropic_request(body),
        (Protocol::Gemini, Protocol::Responses) => gemini::gemini_to_responses_request(body),
    }
}

/// 翻译一次非流式响应体（上游协议 → 入站协议）；协议一致时原样返回。
pub fn translate_response(inbound: Protocol, upstream: Protocol, body: Value) -> Result<Value> {
    match (inbound, upstream) {
        (Protocol::OpenAI, Protocol::OpenAI)
        | (Protocol::Anthropic, Protocol::Anthropic)
        | (Protocol::Responses, Protocol::Responses)
        | (Protocol::Gemini, Protocol::Gemini) => Ok(body),
        (Protocol::OpenAI, Protocol::Anthropic) => anthropic_message_to_openai_completion(body),
        (Protocol::Anthropic, Protocol::OpenAI) => openai_completion_to_anthropic_message(body),
        (Protocol::Responses, Protocol::OpenAI) => responses::openai_chat_to_response(body),
        (Protocol::Responses, Protocol::Anthropic) => {
            responses::anthropic_message_to_response(body)
        }
        (Protocol::OpenAI, Protocol::Responses) => responses::response_to_openai_chat(body),
        (Protocol::Anthropic, Protocol::Responses) => responses::response_to_anthropic(body),
        (Protocol::OpenAI, Protocol::Gemini) => gemini::gemini_to_openai_chat(body),
        (Protocol::Anthropic, Protocol::Gemini) => gemini::gemini_to_anthropic(body),
        (Protocol::Responses, Protocol::Gemini) => gemini::gemini_to_responses(body),
        (Protocol::Gemini, Protocol::OpenAI) => gemini::openai_completion_to_gemini(body),
        (Protocol::Gemini, Protocol::Anthropic) => gemini::anthropic_message_to_gemini(body),
        (Protocol::Gemini, Protocol::Responses) => gemini::responses_object_to_gemini(body),
    }
}

/// OpenAI 上游开流时注入 usage 末包，否则流式计量拿不到用量。
pub fn inject_openai_include_usage(body: &mut Value) {
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        body["stream_options"] = json!({"include_usage": true});
    }
}

// ── 请求转换 ────────────────────────────────────────────────

// 明确忽略的入站字段（Anthropic 无对应物，静默丢弃而非报错；钉在测试里）：
//   presence_penalty / frequency_penalty / logit_bias / logprobs / top_logprobs
//   n（Anthropic 单候选）/ response_format（JSON 模式无等价物）/ seed
//   user / stream_options / service_tier / metadata。
// Anthropic → OpenAI 方向：stop_sequence 之外的 anthropic-beta 等头部在 proxy 层处理。

fn openai_request_to_anthropic(body: Value) -> Result<Value> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("openai 请求缺少 model"))?
        .to_string();

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("openai 请求缺少 messages"))?;
    for message in raw_messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("openai message 缺少 role"))?;
        match role {
            "system" | "developer" => {
                if let Some(text) = message_text(&message) {
                    system_parts.push(text);
                }
            }
            "user" => messages.push(json!({
                "role": "user",
                "content": openai_content_to_anthropic_blocks(&message, false)?,
            })),
            "assistant" => {
                let mut blocks = openai_content_to_anthropic_blocks(&message, true)?;
                if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in tool_calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.get("id"),
                            "name": call.pointer("/function/name"),
                            "input": call
                                .pointer("/function/arguments")
                                .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                                .unwrap_or(json!({})),
                        }));
                    }
                }
                messages.push(json!({"role": "assistant", "content": blocks}));
            }
            "tool" => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": message.get("tool_call_id"),
                        "content": message.get("content").cloned().unwrap_or(json!("")),
                    }],
                }));
            }
            other => return Err(bad(format!("openai message role 不支持: {other}"))),
        }
    }

    let mut out = json!({
        "model": model,
        "messages": messages,
        // Anthropic 必填；OpenAI 侧缺省时给一个保守上限。
        "max_tokens": body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .cloned()
            .unwrap_or(json!(4096)),
    });
    // 推理配置：reasoning_effort → thinking 预算（共用映射表）。Anthropic 要求
    // budget_tokens < max_tokens，不满足时抬 max_tokens，否则每个请求都 400。
    if let Some(effort) = body.get("reasoning_effort").and_then(Value::as_str)
        && let Some(budget) = effort_to_budget(effort)
    {
        out["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        let max_tokens = out["max_tokens"].as_i64().unwrap_or(0);
        if max_tokens <= budget {
            out["max_tokens"] = json!(budget + 1024);
        }
    }
    if !system_parts.is_empty() {
        out["system"] = Value::String(system_parts.join("\n\n"));
    }
    for field in ["temperature", "top_p"] {
        if let Some(v) = body.get(field) {
            out[field] = v.clone();
        }
    }
    if let Some(stop) = body.get("stop").cloned() {
        out["stop_sequences"] = match stop {
            Value::String(s) => json!([s]),
            Value::Array(items) => Value::Array(items),
            _ => return Err(bad("openai stop 只支持字符串或数组")),
        };
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                let f = tool.get("function")?;
                Some(json!({
                    "name": f.get("name")?,
                    "description": f.get("description").cloned().unwrap_or(json!("")),
                    "input_schema": f.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                }))
            })
            .collect();
        if !mapped.is_empty() {
            out["tools"] = Value::Array(mapped);
        }
    }
    match body.get("tool_choice") {
        Some(Value::String(s)) if s == "auto" => out["tool_choice"] = json!({"type": "auto"}),
        Some(Value::String(s)) if s == "none" => out["tool_choice"] = json!({"type": "none"}),
        Some(Value::String(s)) if s == "required" => out["tool_choice"] = json!({"type": "any"}),
        Some(choice) if choice.get("function").is_some() => {
            let name = choice
                .pointer("/function/name")
                .cloned()
                .unwrap_or_default();
            out["tool_choice"] = json!({"type": "tool", "name": name});
        }
        _ => {}
    }
    Ok(out)
}

/// OpenAI message 的 content（string 或多段）转 Anthropic blocks。
/// `assistant: true` 时跳过 tool 段（tool_calls 另行映射）与图片段。
fn openai_content_to_anthropic_blocks(message: &Value, assistant: bool) -> Result<Vec<Value>> {
    let mut blocks = Vec::new();
    match message.get("content") {
        Some(Value::String(text)) if !text.is_empty() => {
            blocks.push(json!({"type": "text", "text": text}));
        }
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                    }
                    // 图片只出现在 user 消息；data URL 转 base64 块，
                    // http URL 无对等表达，明确报错而不是静默丢图。
                    Some("image_url") if !assistant => {
                        let url = part
                            .pointer("/image_url/url")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        blocks.push(image_url_to_anthropic_block(url)?);
                    }
                    Some("tool_calls") if assistant => {}
                    _ => {}
                }
            }
        }
        _ => {}
    }
    if blocks.is_empty() && !assistant {
        blocks.push(json!({"type": "text", "text": ""}));
    }
    Ok(blocks)
}

/// `data:<media>;base64,<data>` 形态拆解；非 data URL 返回 `None`。
fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    Some((media_type.to_string(), data.to_string()))
}

/// openai image_url part → anthropic image block。
fn image_url_to_anthropic_block(url: &str) -> Result<Value> {
    let (media_type, data) = parse_data_url(url).ok_or_else(|| {
        bad("anthropic 上游只支持 base64 图片：http URL 无法转写，请改用 data URL")
    })?;
    Ok(json!({
        "type": "image",
        "source": {"type": "base64", "media_type": media_type, "data": data},
    }))
}

/// anthropic image block → openai image_url part（base64 回包成 data URL）。
/// 非 base64 source（web URL beta 形态）无对等表达，返回 `None` 跳过。
fn anthropic_image_to_openai_part(block: &Value) -> Option<Value> {
    let source = block.get("source")?;
    if source.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let media_type = source
        .get("media_type")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    let data = source.get("data").and_then(Value::as_str)?;
    Some(json!({
        "type": "image_url",
        "image_url": {"url": format!("data:{media_type};base64,{data}")},
    }))
}

fn message_text(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => Some(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(""),
        ),
        _ => None,
    }
}

fn anthropic_request_to_openai(body: Value) -> Result<Value> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("anthropic 请求缺少 model"))?
        .to_string();

    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = body.get("system")
        && let Some(text) = message_text(&json!({"content": system}))
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("anthropic 请求缺少 messages"))?;
    for message in raw_messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("anthropic message 缺少 role"))?;
        let content = message.get("content").cloned().unwrap_or(json!(""));
        match role {
            "user" => {
                // 一个 user 消息里可能同时有 text 与 tool_result 块；tool_result
                // 按 OpenAI 语义必须独立成 tool 消息，拆开。image 块回包成
                // image_url part；有图时 content 必须是数组而不是拼接字符串。
                let blocks = content.as_array().cloned().unwrap_or_else(|| {
                    vec![json!({"type": "text", "text": content.as_str().unwrap_or_default()})]
                });
                let mut parts: Vec<Value> = Vec::new();
                let mut texts: Vec<&Value> = Vec::new();
                for block in &blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => texts.push(block),
                        Some("image") => {
                            if let Some(part) = anthropic_image_to_openai_part(block) {
                                parts.push(part);
                            }
                        }
                        Some("tool_result") => messages.push(json!({
                            "role": "tool",
                            "tool_call_id": block.get("tool_use_id"),
                            "content": block
                                .get("content")
                                .and_then(|c| {
                                    c.as_str().map(str::to_string).or_else(|| {
                                        serde_json::to_string(c).ok()
                                    })
                                })
                                .unwrap_or_default(),
                        })),
                        _ => {}
                    }
                }
                if !texts.is_empty() {
                    let text: String = texts
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("");
                    if parts.is_empty() {
                        messages.push(json!({"role": "user", "content": text}));
                        continue;
                    }
                    parts.insert(0, json!({"type": "text", "text": text}));
                }
                if !parts.is_empty() {
                    messages.push(json!({"role": "user", "content": parts}));
                }
            }
            "assistant" => {
                let blocks = content.as_array().cloned().unwrap_or_default();
                let mut tool_calls = Vec::new();
                let mut text = String::new();
                for block in &blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
                        }
                        Some("tool_use") => tool_calls.push(json!({
                            "id": block.get("id"),
                            "type": "function",
                            "function": {
                                "name": block.get("name"),
                                "arguments": block
                                    .get("input")
                                    .map(|input| input.to_string())
                                    .unwrap_or_else(|| "{}".into()),
                            },
                        })),
                        _ => {}
                    }
                }
                let mut out = json!({"role": "assistant", "content": text});
                if !tool_calls.is_empty() {
                    out["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(out);
            }
            other => return Err(bad(format!("anthropic message role 不支持: {other}"))),
        }
    }

    let mut out = json!({"model": model, "messages": messages});
    for field in ["max_tokens", "temperature", "top_p", "stream", "tools"] {
        if let Some(v) = body.get(field) {
            out[field] = v.clone();
        }
    }
    // 推理配置：thinking 预算 → reasoning_effort 档位，映射与正向同一张约定表
    //（档位是 coarse 粒度，预算只取相对大小）。历史消息里的 thinking 块已被
    // 上面的解析丢弃——OpenAI 协议不接受推理内容回传。
    if let Some(budget) = body
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_i64)
        && body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled")
    {
        out["reasoning_effort"] = json!(budget_to_effort(budget));
    }
    if let Some(stops) = body.get("stop_sequences").and_then(Value::as_array) {
        out["stop"] = Value::Array(stops.clone());
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        out["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.get("name"),
                            "description": tool.get("description").cloned().unwrap_or(json!("")),
                            "parameters": tool.get("input_schema").cloned().unwrap_or(json!({"type": "object"})),
                        },
                    })
                })
                .collect(),
        );
    }
    match body
        .get("tool_choice")
        .and_then(|c| c.get("type"))
        .and_then(Value::as_str)
    {
        Some("auto") => out["tool_choice"] = json!("auto"),
        Some("none") => out["tool_choice"] = json!("none"),
        Some("any") => out["tool_choice"] = json!("required"),
        Some("tool") => {
            let name = body
                .pointer("/tool_choice/name")
                .cloned()
                .unwrap_or_default();
            out["tool_choice"] = json!({"type": "function", "function": {"name": name}});
        }
        _ => {}
    }
    Ok(out)
}

// ── 非流式响应转换 ──────────────────────────────────────────

fn anthropic_message_to_openai_completion(message: Value) -> Result<Value> {
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut tool_calls = Vec::new();
    let mut call_index = 0usize;
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks.iter() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
                }
                // 推理通道：thinking 块 ↔ reasoning_content；redacted_thinking 无法
                // 还原明文，丢弃（客户端只是少看到一段不可读的占位）。
                Some("thinking") => {
                    reasoning.push_str(block.get("thinking").and_then(Value::as_str).unwrap_or(""));
                }
                Some("tool_use") => {
                    tool_calls.push(json!({
                        "index": call_index,
                        "id": block.get("id"),
                        "type": "function",
                        "function": {
                            "name": block.get("name"),
                            "arguments": block
                                .get("input")
                                .map(|input| input.to_string())
                                .unwrap_or_else(|| "{}".into()),
                        },
                    }));
                    call_index += 1;
                }
                _ => {}
            }
        }
    }
    let mut message_out = json!({"role": "assistant", "content": text});
    if !reasoning.is_empty() {
        message_out["reasoning_content"] = json!(reasoning);
    }
    if !tool_calls.is_empty() {
        message_out["tool_calls"] = Value::Array(tool_calls);
    }

    let usage = extract_anthropic_usage(&message).unwrap_or_default();
    let choice = json!({
        "index": 0,
        "message": message_out,
        "finish_reason": anthropic_stop_to_openai(message.get("stop_reason")),
    });
    Ok(json!({
        "id": message.get("id").cloned().unwrap_or(json!("msg-toktol")),
        "object": "chat.completion",
        "created": unix_now(),
        "model": model,
        "choices": [choice],
        "usage": openai_usage_json(&usage),
    }))
}

fn openai_completion_to_anthropic_message(completion: Value) -> Result<Value> {
    let model = completion
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let choice = completion
        .pointer("/choices/0")
        .ok_or_else(|| bad("openai 响应缺少 choices[0]"))?;
    let mut blocks = Vec::new();
    // 推理通道：reasoning_content → thinking 块，惯例上放在正文之前。
    if let Some(reasoning) = choice
        .pointer("/message/reasoning_content")
        .and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        blocks.push(json!({"type": "thinking", "thinking": reasoning}));
    }
    if let Some(text) = choice.pointer("/message/content").and_then(Value::as_str)
        && !text.is_empty()
    {
        blocks.push(json!({"type": "text", "text": text}));
    }
    if let Some(calls) = choice
        .pointer("/message/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            blocks.push(json!({
                "type": "tool_use",
                "id": call.get("id"),
                "name": call.pointer("/function/name"),
                "input": call
                    .pointer("/function/arguments")
                    .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                    .unwrap_or(json!({})),
            }));
        }
    }

    let usage = extract_openai_usage(&completion).unwrap_or_default();
    Ok(json!({
        "id": completion.get("id").cloned().unwrap_or(json!("chatcmpl-toktol")),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": openai_finish_to_anthropic(choice.get("finish_reason")),
        "usage": anthropic_usage_json(&usage),
    }))
}

pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn anthropic_stop_to_openai(stop: Option<&Value>) -> &'static str {
    match stop.and_then(Value::as_str) {
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_calls",
        Some("refusal") => "content_filter",
        _ => "stop",
    }
}

fn openai_finish_to_anthropic(finish: Option<&Value>) -> &'static str {
    match finish.and_then(Value::as_str) {
        Some("length") => "max_tokens",
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("content_filter") => "refusal",
        _ => "end_turn",
    }
}

pub(super) fn extract_anthropic_usage(message: &Value) -> Option<TokenUsage> {
    let usage = message.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_read_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_write_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        reasoning_tokens: usage.get("reasoning_tokens").and_then(Value::as_i64),
    })
}

pub(super) fn extract_openai_usage(completion: &Value) -> Option<TokenUsage> {
    let usage = completion.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        output_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_read_tokens: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: usage
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_i64),
    })
}

fn openai_usage_json(usage: &TokenUsage) -> Value {
    json!({
        "prompt_tokens": usage.input_tokens,
        "completion_tokens": usage.output_tokens,
        "total_tokens": usage.input_tokens + usage.output_tokens,
        "prompt_tokens_details": {"cached_tokens": usage.cache_read_tokens},
        "completion_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
    })
}

fn anthropic_usage_json(usage: &TokenUsage) -> Value {
    json!({
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cache_read_input_tokens": usage.cache_read_tokens,
        "cache_creation_input_tokens": usage.cache_write_tokens,
    })
}

// ── 流式转换 ────────────────────────────────────────────────

/// 上游 SSE → 入站 SSE 的事件级翻译器；协议一致时不需要实例（字节透传）。
pub trait StreamTranslator {
    /// 喂一条上游 `data:` 载荷（`[DONE]` 原样），返回**完整的 SSE 块**（含
    /// `data:`/`event:` 行与尾随空行），proxy 原样发给客户端。Responses 入站
    /// 需要 `event:` 行，所以契约以块为单位而不是裸载荷。
    fn feed(&mut self, payload: &str) -> Vec<String>;
    /// 上游流结束时调用，冲刷未闭合的事件。
    fn finish(&mut self) -> Vec<String>;
}

/// 把一条 `data:` 载荷包成 SSE 块（OpenAI/Anthropic 入站方向的通用形态）。
fn data_block(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// 按方向构造流式翻译器；协议一致返回 `None`（走透传路径）。
pub fn stream_translator(
    inbound: Protocol,
    upstream: Protocol,
) -> Option<Box<dyn StreamTranslator + Send>> {
    match (inbound, upstream) {
        (Protocol::OpenAI, Protocol::OpenAI)
        | (Protocol::Anthropic, Protocol::Anthropic)
        | (Protocol::Responses, Protocol::Responses)
        | (Protocol::Gemini, Protocol::Gemini) => None,
        // 入站 OpenAI，上游 Anthropic。
        (Protocol::OpenAI, Protocol::Anthropic) => Some(Box::new(AnthropicToOpenAiStream::new())),
        // 入站 Anthropic，上游 OpenAI。
        (Protocol::Anthropic, Protocol::OpenAI) => Some(Box::new(OpenAiToAnthropicStream::new())),
        // 入站 Responses。
        (Protocol::Responses, Protocol::OpenAI) => {
            Some(Box::new(responses::OpenAiChatToResponsesStream::new()))
        }
        (Protocol::Responses, Protocol::Anthropic) => {
            Some(Box::new(responses::AnthropicToResponsesStream::new()))
        }
        // 上游 Responses。
        (Protocol::OpenAI, Protocol::Responses) => {
            Some(Box::new(responses::ResponsesToOpenAiStream::new()))
        }
        (Protocol::Anthropic, Protocol::Responses) => {
            Some(Box::new(responses::ResponsesToAnthropicStream::new()))
        }
        // 入站 Gemini。
        (Protocol::Gemini, Protocol::OpenAI) => Some(Box::new(gemini::OpenAiToGeminiStream::new())),
        (Protocol::Gemini, Protocol::Anthropic) => {
            Some(Box::new(gemini::AnthropicToGeminiStream::new()))
        }
        (Protocol::Gemini, Protocol::Responses) => {
            Some(Box::new(gemini::ResponsesToGeminiStream::new()))
        }
        // 上游 Gemini。
        (Protocol::OpenAI, Protocol::Gemini) => Some(Box::new(gemini::GeminiToOpenAiStream::new())),
        (Protocol::Anthropic, Protocol::Gemini) => {
            Some(Box::new(gemini::GeminiToAnthropicStream::new()))
        }
        (Protocol::Responses, Protocol::Gemini) => {
            Some(Box::new(gemini::GeminiToResponsesStream::new()))
        }
    }
}

/// 上游 Anthropic SSE → 客户端 OpenAI chunks。
struct AnthropicToOpenAiStream {
    id: String,
    model: String,
    usage: TokenUsage,
    started: bool,
    /// 下一个可分配的 OpenAI tool_calls index。
    next_tool: usize,
    /// Anthropic 块号 → OpenAI tool_calls index。并行 tool_use 块各自独立编号，
    /// 客户端靠它区分并合并参数片段。
    block_to_tool: std::collections::HashMap<usize, usize>,
}

impl AnthropicToOpenAiStream {
    fn new() -> Self {
        Self {
            id: format!("chatcmpl-toktol-{}", unix_now()),
            model: String::new(),
            usage: TokenUsage::default(),
            started: false,
            next_tool: 0,
            block_to_tool: std::collections::HashMap::new(),
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        data_block(
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": unix_now(),
                "model": self.model,
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish,
                }],
            })
            .to_string(),
        )
    }
}

impl StreamTranslator for AnthropicToOpenAiStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        let event: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "message_start" => {
                self.started = true;
                if let Some(model) = event.pointer("/message/model").and_then(Value::as_str) {
                    self.model = model.to_string();
                }
                if let Some(usage) = event.pointer("/message/usage") {
                    self.usage.input_tokens = usage
                        .get("input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.usage.cache_read_tokens = usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                    self.usage.cache_write_tokens = usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                }
                vec![self.chunk(json!({"role": "assistant", "content": ""}), None)]
            }
            "content_block_start" => {
                if event.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
                {
                    let block = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let tool = self.next_tool;
                    self.next_tool += 1;
                    self.block_to_tool.insert(block, tool);
                    vec![self.chunk(
                        json!({"tool_calls": [{
                            "index": tool,
                            "id": event.pointer("/content_block/id"),
                            "type": "function",
                            "function": {
                                "name": event.pointer("/content_block/name"),
                                "arguments": "",
                            },
                        }]}),
                        None,
                    )]
                } else {
                    vec![]
                }
            }
            "content_block_delta" => {
                let delta = event.get("delta");
                let block = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let kind = delta.and_then(|d| d.get("type")).and_then(Value::as_str);
                match kind {
                    Some("text_delta") => {
                        let text = delta
                            .and_then(|d| d.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![self.chunk(json!({"content": text}), None)]
                        }
                    }
                    // 推理通道：Anthropic thinking_delta ↔ OpenAI reasoning_content。
                    Some("thinking_delta") => {
                        let text = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![self.chunk(json!({"reasoning_content": text}), None)]
                        }
                    }
                    // signature_delta 无 OpenAI 对应物（客户端不需要校验签名），丢弃。
                    Some("signature_delta") => vec![],
                    Some("input_json_delta") => {
                        let Some(tool) = self.block_to_tool.get(&block).copied() else {
                            return vec![];
                        };
                        let partial = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if partial.is_empty() {
                            vec![]
                        } else {
                            vec![self.chunk(
                                json!({"tool_calls": [{
                                    "index": tool,
                                    "function": {"arguments": partial},
                                }]}),
                                None,
                            )]
                        }
                    }
                    _ => vec![],
                }
            }
            "message_delta" => {
                if let Some(usage) = event.get("usage") {
                    self.usage.output_tokens = usage
                        .get("output_tokens")
                        .and_then(Value::as_i64)
                        .unwrap_or(0);
                }
                vec![self.chunk(
                    json!({}),
                    Some(anthropic_stop_to_openai(
                        event.pointer("/delta/stop_reason"),
                    )),
                )]
            }
            "message_stop" => {
                self.started = false;
                vec![data_block("[DONE]")]
            }
            _ => vec![],
        }
    }

    fn finish(&mut self) -> Vec<String> {
        // 正常收尾在 message_stop 处已完成；这里只兜底上游断流未发 stop 的情况。
        if self.started {
            self.started = false;
            vec![data_block("[DONE]")]
        } else {
            vec![]
        }
    }
}

/// 上游 OpenAI SSE → 客户端 Anthropic 事件。
struct OpenAiToAnthropicStream {
    model: String,
    message_started: bool,
    /// 下一个待开 block 的索引（Anthropic 块号必须按 start 事件顺序递增）。
    next_block: usize,
    /// 当前开着的文本块；与 thinking 块互斥（同一时刻至多一个开着的块）。
    open_text_block: Option<usize>,
    /// 当前开着的 thinking 块。
    open_thinking_block: Option<usize>,
    /// OpenAI tool_calls index → Anthropic 块号。并行工具调用按上游 index 各建
    /// 各的块，参数片段交错到达也能归位。
    tool_blocks: std::collections::HashMap<usize, usize>,
    /// 已开（未关）的 tool_use 块号，按创建顺序；finish 时统一补 stop。
    open_tool_blocks: Vec<usize>,
    input_usage: TokenUsage,
    output_usage: TokenUsage,
    finish_reason: Option<Value>,
}

impl OpenAiToAnthropicStream {
    fn new() -> Self {
        Self {
            model: String::new(),
            message_started: false,
            next_block: 0,
            open_text_block: None,
            open_thinking_block: None,
            tool_blocks: std::collections::HashMap::new(),
            open_tool_blocks: Vec::new(),
            input_usage: TokenUsage::default(),
            output_usage: TokenUsage::default(),
            finish_reason: None,
        }
    }

    fn start_message(&mut self) -> String {
        self.message_started = true;
        json!({
            "type": "message_start",
            "message": {
                "id": format!("msg-toktol-{}", unix_now()),
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": Value::Null,
                // input usage 在 OpenAI 流里末尾才出现，先置 0，message_delta 补真实值。
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        })
        .to_string()
    }

    fn event(&self, value: Value) -> String {
        data_block(&value.to_string())
    }

    /// 开一个新块（关闭当前开着的其它块——Anthropic 的块不重叠）。
    fn start_block(&mut self, out: &mut Vec<String>, content_block: Value) -> usize {
        self.close_text_block(out);
        self.close_thinking_block(out);
        let index = self.next_block;
        self.next_block += 1;
        out.push(self.event(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": content_block,
        })));
        index
    }

    fn close_text_block(&mut self, out: &mut Vec<String>) {
        if let Some(index) = self.open_text_block.take() {
            out.push(self.stop_event(index));
        }
    }

    fn close_thinking_block(&mut self, out: &mut Vec<String>) {
        if let Some(index) = self.open_thinking_block.take() {
            out.push(self.stop_event(index));
        }
    }

    fn stop_event(&self, index: usize) -> String {
        self.event(json!({
            "type": "content_block_stop",
            "index": index,
        }))
    }
}

impl StreamTranslator for OpenAiToAnthropicStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let chunk: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        if let Some(model) = chunk.get("model").and_then(Value::as_str)
            && self.model.is_empty()
        {
            self.model = model.to_string();
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
            let full = json!({"usage": usage});
            if let Some(parsed) = extract_openai_usage(&full) {
                self.input_usage.input_tokens = parsed.input_tokens;
                self.input_usage.cache_read_tokens = parsed.cache_read_tokens;
                self.output_usage.output_tokens = parsed.output_tokens;
                self.output_usage.reasoning_tokens = parsed.reasoning_tokens;
            }
        }

        let mut out = Vec::new();
        if !self.message_started {
            out.push(self.start_message());
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return out;
        };
        let delta = choice.get("delta");

        // 推理通道：reasoning_content ↔ thinking 块。惯例上推理先于正文到达。
        if let Some(text) = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(Value::as_str)
            && !text.is_empty()
        {
            if self.open_thinking_block.is_none() {
                let index = self.start_block(&mut out, json!({"type": "thinking", "thinking": ""}));
                self.open_thinking_block = Some(index);
            }
            out.push(self.event(json!({
                "type": "content_block_delta",
                "index": self.open_thinking_block,
                "delta": {"type": "thinking_delta", "thinking": text},
            })));
        }

        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str)
            && !text.is_empty()
        {
            if self.open_text_block.is_none() {
                let index = self.start_block(&mut out, json!({"type": "text", "text": ""}));
                self.open_text_block = Some(index);
            }
            out.push(self.event(json!({
                "type": "content_block_delta",
                "index": self.open_text_block,
                "delta": {"type": "text_delta", "text": text},
            })));
        }

        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in calls {
                // 上游并行工具调用用 index 区分；首个片段带 id+name，后续只带 index。
                let upstream_index =
                    call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if !self.tool_blocks.contains_key(&upstream_index) {
                    // 没有新调用标志（id/name）的片段无法归属成新块，丢弃防串写。
                    if call.get("id").is_none() && call.pointer("/function/name").is_none() {
                        continue;
                    }
                    let index = self.start_block(
                        &mut out,
                        json!({
                            "type": "tool_use",
                            "id": call.get("id"),
                            "name": call.pointer("/function/name"),
                            "input": {},
                        }),
                    );
                    self.tool_blocks.insert(upstream_index, index);
                    self.open_tool_blocks.push(index);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str)
                    && !args.is_empty()
                {
                    out.push(self.event(json!({
                        "type": "content_block_delta",
                        "index": self.tool_blocks[&upstream_index],
                        "delta": {"type": "input_json_delta", "partial_json": args},
                    })));
                }
            }
        }

        if let Some(finish) = choice.get("finish_reason").filter(|f| !f.is_null()) {
            self.finish_reason = Some(finish.clone());
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        if !self.message_started {
            return vec![];
        }
        let mut out = Vec::new();
        self.close_text_block(&mut out);
        self.close_thinking_block(&mut out);
        for index in std::mem::take(&mut self.open_tool_blocks) {
            out.push(self.stop_event(index));
        }
        out.push(self.event(json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": openai_finish_to_anthropic(self.finish_reason.as_ref()),
                "stop_sequence": Value::Null,
            },
            "usage": {
                "input_tokens": self.input_usage.input_tokens,
                "output_tokens": self.output_usage.output_tokens,
            },
        })));
        out.push(self.event(json!({"type": "message_stop"})));
        self.message_started = false;
        out
    }
}

// ── 计量抽取 ────────────────────────────────────────────────

/// 从上游 SSE 事件里累积用量，供计量落库。独立于流式转换：计量直接消费上游
/// 原生事件，不依赖客户端方向的翻译保真度。
pub struct UsageScanner {
    protocol: Protocol,
    usage: TokenUsage,
}

impl UsageScanner {
    pub fn new(protocol: Protocol) -> Self {
        Self {
            protocol,
            usage: TokenUsage::default(),
        }
    }

    /// 喂一条上游 `data:` 载荷；`[DONE]` 与坏行安全忽略。
    pub fn feed(&mut self, payload: &str) {
        if payload.trim() == "[DONE]" {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return;
        };
        match self.protocol {
            Protocol::Anthropic => match event.get("type").and_then(Value::as_str) {
                Some("message_start") => {
                    if let Some(usage) = event.pointer("/message/usage") {
                        self.usage.input_tokens = usage
                            .get("input_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                        self.usage.cache_read_tokens = usage
                            .get("cache_read_input_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                        self.usage.cache_write_tokens = usage
                            .get("cache_creation_input_tokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(0);
                    }
                }
                Some("message_delta") => {
                    if let Some(output) = event
                        .pointer("/usage/output_tokens")
                        .and_then(Value::as_i64)
                    {
                        // Anthropic 的 message_delta usage 是累计输出量。
                        self.usage.output_tokens = output;
                    }
                }
                _ => {}
            },
            Protocol::OpenAI => {
                if let Some(usage) = event.get("usage").filter(|u| u.is_object()) {
                    let full = json!({"usage": usage});
                    if let Some(parsed) = extract_openai_usage(&full) {
                        self.usage = parsed;
                    }
                }
            }
            // Responses 的 usage 在收尾事件（completed/incomplete/failed）里。
            Protocol::Responses => {
                let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
                if responses::is_response_done(kind)
                    && let Some(parsed) = responses::extract_responses_usage(
                        event.get("response").unwrap_or(&Value::Null),
                    )
                {
                    self.usage = parsed;
                }
            }
            // Gemini 每个 chunk 都可能带 usageMetadata（末块最全），取最后一次。
            Protocol::Gemini => {
                if let Some(parsed) = gemini::extract_gemini_usage(&event) {
                    self.usage = parsed;
                }
            }
        }
    }

    /// 累积到的用量；没抽到（如上游没发 usage）返回 `None`。
    pub fn take(self) -> Option<TokenUsage> {
        let empty = TokenUsage::default();
        if self.usage == empty && self.usage.reasoning_tokens.is_none() {
            None
        } else {
            Some(self.usage)
        }
    }
}

/// 非流式响应体的用量抽取；流式路径请用 [`UsageScanner`]。
pub fn extract_usage_json(protocol: Protocol, body: &Value) -> Option<TokenUsage> {
    match protocol {
        Protocol::Anthropic => extract_anthropic_usage(body),
        Protocol::OpenAI => extract_openai_usage(body),
        Protocol::Responses => responses::extract_responses_usage(body),
        Protocol::Gemini => gemini::extract_gemini_usage(body),
    }
}

/// SSE 单行缓冲上限（8 MiB）：上游被声明为 SSE 但持续输出无换行的超大响应
///（被误标 `text/event-stream` 的错误页、故障/被劫持的上游）时，行缓冲无界
/// 增长直到 OOM。超限整行放弃、缓冲清空，调用方终止泵并向客户端发错误事件。
pub const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;

/// [`SseScanner::push`] 的行超限哨兵：缓冲已清空，调用方应终止泵。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SseOverflow;

/// 增量切分 SSE 字节流为 `data:` 载荷；处理跨 chunk 断行。
pub struct SseScanner {
    buf: Vec<u8>,
}

impl SseScanner {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 喂一段上游字节，返回其中完整的 `data:` 行载荷；无换行的字节堆积超过
    /// [`MAX_SSE_LINE_BYTES`] 时返回 [`SseOverflow`]——缓冲已清空，调用方应
    /// 终止泵而不是陪病态上游烧内存。
    pub fn push(&mut self, bytes: &[u8]) -> std::result::Result<Vec<String>, SseOverflow> {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > MAX_SSE_LINE_BYTES {
            self.buf.clear();
            return Err(SseOverflow);
        }
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            if let Some(payload) = sse_data_payload(&line[..line.len() - 1]) {
                out.push(payload);
            }
        }
        Ok(out)
    }

    /// 流结束：把残余的不完整行也吐出来。
    pub fn finish(&mut self) -> Vec<String> {
        let line = std::mem::take(&mut self.buf);
        if line.is_empty() {
            vec![]
        } else {
            sse_data_payload(&line).into_iter().collect()
        }
    }
}

impl Default for SseScanner {
    fn default() -> Self {
        Self::new()
    }
}

/// 一行 SSE（不含行尾）→ data 载荷；非 data 行与注释返回 `None`。
fn sse_data_payload(line: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches('\r').trim();
    let rest = text.strip_prefix("data:")?;
    let payload = rest.trim_start();
    if payload.is_empty() || payload.starts_with(':') {
        None
    } else {
        Some(payload.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oai_request() -> Value {
        json!({
            "model": "gpt-x",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"}
            ],
            "max_tokens": 100,
            "stream": true
        })
    }

    #[test]
    fn openai_images_map_to_anthropic_blocks_and_back() {
        const PNG: &str = "iVBORw0KGgo=";
        let body = json!({
            "model": "gpt-x",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "看这张图"},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{PNG}")}},
                ],
            }],
            "max_tokens": 100,
        });
        let out = openai_request_to_anthropic(body).unwrap();
        let blocks = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(
            blocks[1],
            json!({"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": PNG}}),
        );

        // 反向走一圈：base64 回包成 data URL，content 退化为数组形态。
        let back = anthropic_request_to_openai(out).unwrap();
        let parts = back["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "看这张图");
        assert_eq!(
            parts[1],
            json!({"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{PNG}")}}),
        );

        // http URL 无对等表达：明确报错而不是静默丢图。
        let http = json!({
            "model": "gpt-x",
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "https://example.com/a.png"}}],
            }],
        });
        assert!(openai_request_to_anthropic(http).is_err());
    }

    #[test]
    fn openai_request_maps_to_anthropic() {
        let out = openai_request_to_anthropic(oai_request()).unwrap();
        assert_eq!(out["model"], "gpt-x");
        assert_eq!(out["max_tokens"], 100);
        assert_eq!(out["system"], "be brief");
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][0]["content"][0]["type"], "text");
    }

    #[test]
    fn anthropic_request_maps_to_openai() {
        let body = json!({
            "model": "claude-x",
            "system": "be brief",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "t1", "name": "get", "input": {"q": "x"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
                ]}
            ],
            "max_tokens": 50,
            "stop_sequences": ["END"]
        });
        let out = anthropic_request_to_openai(body.clone()).unwrap();
        assert_eq!(out["model"], "claude-x");
        assert_eq!(
            out["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(out["messages"][1]["content"], "hi");
        assert_eq!(out["messages"][2]["tool_calls"][0]["id"], "t1");
        assert_eq!(out["messages"][3]["role"], "tool");
        assert_eq!(out["messages"][3]["tool_call_id"], "t1");
        assert_eq!(out["max_tokens"], 50);
        assert_eq!(out["stop"], json!(["END"]));
    }

    #[test]
    fn tool_definitions_map_both_ways() {
        let openai = json!({
            "model": "gpt-x",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {
                "name": "get_weather", "description": "w",
                "parameters": {"type": "object", "properties": {}}}}],
            "tool_choice": {"type": "function", "function": {"name": "get_weather"}}
        });
        let anthropic = openai_request_to_anthropic(openai).unwrap();
        assert_eq!(anthropic["tools"][0]["name"], "get_weather");
        assert_eq!(anthropic["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(anthropic["tool_choice"]["type"], "tool");
        assert_eq!(anthropic["tool_choice"]["name"], "get_weather");

        let back = anthropic_request_to_openai(anthropic).unwrap();
        assert_eq!(back["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(back["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn anthropic_response_maps_to_openai_completion() {
        let message = json!({
            "id": "msg_1", "type": "message", "role": "assistant",
            "model": "claude-x",
            "content": [{"type": "text", "text": "hello"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5,
                "cache_read_input_tokens": 3, "cache_creation_input_tokens": 2}
        });
        let out = anthropic_message_to_openai_completion(message).unwrap();
        assert_eq!(out["object"], "chat.completion");
        assert_eq!(out["choices"][0]["message"]["content"], "hello");
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
        assert_eq!(out["usage"]["prompt_tokens"], 10);
        assert_eq!(out["usage"]["completion_tokens"], 5);
        assert_eq!(out["usage"]["prompt_tokens_details"]["cached_tokens"], 3);

        // 反向走一圈，usage 语义保持；OpenAI 没有 cache-write 概念，该桶在往返中归零。
        let back = openai_completion_to_anthropic_message(out).unwrap();
        assert_eq!(back["type"], "message");
        assert_eq!(back["content"][0]["text"], "hello");
        assert_eq!(back["stop_reason"], "end_turn");
        assert_eq!(back["usage"]["cache_read_input_tokens"], 3);
        assert_eq!(back["usage"]["cache_creation_input_tokens"], 0);
    }

    /// 流式翻译器现在返回完整 SSE 块；测试里剥出裸载荷方便断言。
    fn payload(line: &str) -> String {
        line.trim_start_matches("data: ").trim_end().to_string()
    }

    #[test]
    fn sse_scanner_handles_split_lines() {
        let mut scanner = SseScanner::new();
        assert!(scanner.push(b"data: {\"a\"").unwrap().is_empty());
        let events = scanner.push(b":1}\n\ndata: [DONE]\n").unwrap();
        assert_eq!(events, vec![r#"{"a":1}"#, "[DONE]"]);
        assert!(scanner.finish().is_empty());
    }

    /// 行超限：push 报 SseOverflow 并清空缓冲（finish 无残留），scanner 可继续用；
    /// 上限以内的正常多行推送不受影响。
    #[test]
    fn sse_scanner_caps_runaway_lines() {
        let mut scanner = SseScanner::new();
        let big = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
        assert_eq!(scanner.push(&big), Err(SseOverflow));
        assert!(scanner.finish().is_empty(), "缓冲已被清空");
        assert!(
            scanner.push(b"data: next\n").unwrap().len() == 1,
            "清空后可继续用"
        );

        // 恰好在上限内的无换行推送：不触发。
        let mut scanner = SseScanner::new();
        assert!(
            scanner
                .push(&vec![b'x'; MAX_SSE_LINE_BYTES])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn anthropic_stream_translates_to_openai_chunks() {
        let mut translator = AnthropicToOpenAiStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "message_start", "message": {"model": "claude-x",
                "usage": {"input_tokens": 7, "cache_read_input_tokens": 2}}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "he"}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "y"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 3}}),
            json!({"type": "message_stop"}),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        assert_eq!(lines.len(), 5);
        let first: Value = serde_json::from_str(&payload(&lines[0])).unwrap();
        assert_eq!(first["object"], "chat.completion.chunk");
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
        let text0: Value = serde_json::from_str(&payload(&lines[1])).unwrap();
        assert_eq!(text0["choices"][0]["delta"]["content"], "he");
        let finish: Value = serde_json::from_str(&payload(&lines[3])).unwrap();
        assert_eq!(finish["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            lines[4],
            "data: [DONE]

",
            "message_stop 转成 DONE 终止块"
        );
        assert!(
            translator.finish().is_empty(),
            "已收到 message_stop，finish 不再补 DONE"
        );
    }

    #[test]
    fn openai_stream_translates_to_anthropic_events() {
        let mut translator = OpenAiToAnthropicStream::new();
        let mut events = Vec::new();
        for chunk in [
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"role": "assistant", "content": "he"}, "finish_reason": null}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"content": "y"}, "finish_reason": null}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {}, "finish_reason": "stop"}]}),
            json!({"model": "gpt-x", "choices": [], "usage": {
                "prompt_tokens": 9, "completion_tokens": 2,
                "prompt_tokens_details": {"cached_tokens": 1}}}),
            Value::String("[DONE]".into()),
        ] {
            let payload = if chunk.is_string() {
                "[DONE]".to_string()
            } else {
                chunk.to_string()
            };
            events.extend(translator.feed(&payload));
        }
        assert!(translator.finish().is_empty(), "DONE 已冲刷");

        let kinds: Vec<String> = events
            .iter()
            .filter_map(|e| serde_json::from_str::<Value>(&payload(e)).ok())
            .map(|e| e["type"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let delta = serde_json::from_str::<Value>(&payload(&events[2])).unwrap();
        assert_eq!(delta["index"], 0);
        let usage_delta = serde_json::from_str::<Value>(&payload(&events[5])).unwrap();
        assert_eq!(usage_delta["usage"]["input_tokens"], 9);
        assert_eq!(usage_delta["usage"]["output_tokens"], 2);
    }

    #[test]
    fn usage_scanner_covers_both_protocols() {
        let mut anthropic = UsageScanner::new(Protocol::Anthropic);
        anthropic.feed(
            &json!({"type": "message_start", "message": {"usage": {
                "input_tokens": 7, "cache_read_input_tokens": 2,
                "cache_creation_input_tokens": 1}}})
            .to_string(),
        );
        anthropic.feed(
            &json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "x"}})
                .to_string(),
        );
        anthropic
            .feed(&json!({"type": "message_delta", "usage": {"output_tokens": 5}}).to_string());
        let usage = anthropic.take().unwrap();
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cache_read_tokens, 2);
        assert_eq!(usage.cache_write_tokens, 1);

        let mut openai = UsageScanner::new(Protocol::OpenAI);
        openai.feed(&json!({"choices": [{"delta": {"content": "x"}}]}).to_string());
        openai.feed(
            &json!({"choices": [], "usage": {"prompt_tokens": 4, "completion_tokens": 2,
                "prompt_tokens_details": {"cached_tokens": 1},
                "completion_tokens_details": {"reasoning_tokens": 1}}})
            .to_string(),
        );
        openai.feed("[DONE]");
        let usage = openai.take().unwrap();
        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cache_read_tokens, 1);
        assert_eq!(usage.reasoning_tokens, Some(1));

        // 上游没发 usage：返回 None，计量行保持 token 全 0 但不含编造数字。
        let mut none = UsageScanner::new(Protocol::OpenAI);
        none.feed(&json!({"choices": [{"delta": {"content": "x"}}]}).to_string());
        assert!(none.take().is_none());
    }

    #[test]
    fn nonstream_usage_extraction_works() {
        let message = json!({"usage": {"input_tokens": 3, "output_tokens": 2}});
        let usage = extract_usage_json(Protocol::Anthropic, &message).unwrap();
        assert_eq!(usage.input_tokens, 3);
        let completion = json!({"usage": {"prompt_tokens": 3, "completion_tokens": 2}});
        let usage = extract_usage_json(Protocol::OpenAI, &completion).unwrap();
        assert_eq!(usage.output_tokens, 2);
    }

    #[test]
    fn include_usage_is_injected_for_openai_streaming() {
        let mut body = oai_request();
        inject_openai_include_usage(&mut body);
        assert_eq!(body["stream_options"]["include_usage"], true);
        // 非流式不动。
        let mut body = json!({"model": "gpt-x", "messages": []});
        inject_openai_include_usage(&mut body);
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn unsupported_request_fields_are_dropped_silently() {
        let body = json!({
            "model": "gpt-x",
            "messages": [{"role": "user", "content": "hi"}],
            "presence_penalty": 0.5,
            "frequency_penalty": 0.5,
            "logit_bias": {"50256": -100},
            "response_format": {"type": "json_object"},
            "n": 2,
            "seed": 42,
        });
        let out = openai_request_to_anthropic(body).unwrap();
        for dropped in [
            "presence_penalty",
            "frequency_penalty",
            "logit_bias",
            "response_format",
            "n",
            "seed",
        ] {
            assert!(out.get(dropped).is_none(), "{dropped} 应被忽略");
        }
    }

    #[test]
    fn parallel_tool_blocks_get_distinct_openai_indexes() {
        let mut translator = AnthropicToOpenAiStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "message_start", "message": {"model": "claude-x", "usage": {}}}),
            json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "tool_use", "id": "t1", "name": "get", "input": {}}}),
            json!({"type": "content_block_start", "index": 2,
                "content_block": {"type": "tool_use", "id": "t2", "name": "put", "input": {}}}),
            // 片段交错到达：第二个工具的参数先来一段。
            json!({"type": "content_block_delta", "index": 2,
                "delta": {"type": "input_json_delta", "partial_json": "{\"b\""}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"a\""}}),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        assert_eq!(
            lines.len(),
            5,
            "start_message + 两次 block_start + 两个参数片段"
        );

        let mut tool_chunks = Vec::new();
        for line in &lines[1..] {
            tool_chunks.push(serde_json::from_str::<Value>(&payload(line)).unwrap());
        }
        assert_eq!(
            tool_chunks[0]["choices"][0]["delta"]["tool_calls"][0]["index"],
            0
        );
        assert_eq!(
            tool_chunks[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
            "t1"
        );
        assert_eq!(
            tool_chunks[1]["choices"][0]["delta"]["tool_calls"][0]["index"],
            1
        );
        assert_eq!(
            tool_chunks[1]["choices"][0]["delta"]["tool_calls"][0]["id"],
            "t2"
        );
        // 参数片段按块号归位，交错也不串。
        assert_eq!(
            tool_chunks[2]["choices"][0]["delta"]["tool_calls"][0]["index"], 1,
            "块 2 的参数归到 tool index 1"
        );
        assert_eq!(
            tool_chunks[3]["choices"][0]["delta"]["tool_calls"][0]["index"], 0,
            "块 1 的参数归到 tool index 0"
        );
    }

    #[test]
    fn openai_index_only_fragments_keep_their_own_blocks() {
        let mut translator = OpenAiToAnthropicStream::new();
        let mut events = Vec::new();
        for chunk in [
            json!({"model": "gpt-x", "choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "id": "t1", "type": "function",
                    "function": {"name": "get", "arguments": ""}}]}}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 1, "id": "t2", "type": "function",
                    "function": {"name": "put", "arguments": ""}}]}}]}),
            // 后续片段只带 index 不带 id：按 index 归位，交错到达也不串块。
            json!({"model": "gpt-x", "choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 1, "function": {"arguments": "{\"b\""}}]}}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": "{\"a\""}}]}}]}),
            Value::String("[DONE]".into()),
        ] {
            let payload = if chunk.is_string() {
                "[DONE]".to_string()
            } else {
                chunk.to_string()
            };
            events.extend(translator.feed(&payload));
        }

        let parsed: Vec<Value> = events
            .iter()
            .filter_map(|e| serde_json::from_str(&payload(e)).ok())
            .collect();
        let starts: Vec<&Value> = parsed
            .iter()
            .filter(|e| e["type"] == "content_block_start")
            .collect();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[0]["index"], 0);
        assert_eq!(starts[0]["content_block"]["id"], "t1");
        assert_eq!(starts[1]["index"], 1);
        assert_eq!(starts[1]["content_block"]["id"], "t2");

        let deltas: Vec<&Value> = parsed
            .iter()
            .filter(|e| e["type"] == "content_block_delta")
            .collect();
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0]["index"], 1, "index 1 的参数片段归块 1");
        assert_eq!(deltas[1]["index"], 0, "index 0 的参数片段归块 0");

        // finish 统一补齐每个 tool 块的 stop。
        let stops: Vec<&Value> = parsed
            .iter()
            .filter(|e| e["type"] == "content_block_stop")
            .collect();
        assert_eq!(
            stops
                .iter()
                .map(|s| s["index"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn reasoning_content_flows_both_directions() {
        // 非流式：Anthropic thinking 块 → reasoning_content。
        let message = json!({
            "model": "claude-x", "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "let me think"},
                {"type": "text", "text": "answer"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let out = anthropic_message_to_openai_completion(message).unwrap();
        assert_eq!(
            out["choices"][0]["message"]["reasoning_content"],
            "let me think"
        );
        assert_eq!(out["choices"][0]["message"]["content"], "answer");

        // 非流式：reasoning_content → thinking 块（在正文之前）。
        let completion = json!({
            "model": "gpt-x",
            "choices": [{"index": 0, "message": {
                "role": "assistant", "content": "answer", "reasoning_content": "hmm"},
                "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        });
        let back = openai_completion_to_anthropic_message(completion).unwrap();
        assert_eq!(back["content"][0]["type"], "thinking");
        assert_eq!(back["content"][0]["thinking"], "hmm");
        assert_eq!(back["content"][1]["text"], "answer");
    }

    #[test]
    fn reasoning_streams_through_thinking_deltas() {
        // Anthropic 上游 thinking_delta → reasoning_content chunk。
        let mut translator = AnthropicToOpenAiStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "message_start", "message": {"model": "claude-x", "usage": {}}}),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "text_delta", "text": "ok"}}),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        let reasoning: Value = serde_json::from_str(&payload(&lines[1])).unwrap();
        assert_eq!(reasoning["choices"][0]["delta"]["reasoning_content"], "hmm");
        // signature_delta 只产出 [空]——lines[2] 不存在，text 在 lines[2]。
        let text: Value = serde_json::from_str(&payload(&lines[2])).unwrap();
        assert_eq!(text["choices"][0]["delta"]["content"], "ok");

        // OpenAI 上游 reasoning_content → thinking 块。
        let mut translator = OpenAiToAnthropicStream::new();
        let mut events = Vec::new();
        for chunk in [
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"reasoning_content": "hmm"}, "finish_reason": null}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"content": "ok"}, "finish_reason": null}]}),
            Value::String("[DONE]".into()),
        ] {
            let payload = if chunk.is_string() {
                "[DONE]".to_string()
            } else {
                chunk.to_string()
            };
            events.extend(translator.feed(&payload));
        }
        let parsed: Vec<Value> = events
            .iter()
            .filter_map(|e| serde_json::from_str(&payload(e)).ok())
            .collect();
        let kinds: Vec<&str> = parsed
            .iter()
            .map(|e| e["type"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message_start",
                "content_block_start", // thinking
                "content_block_delta",
                "content_block_stop",  // thinking 在正文开块前关闭
                "content_block_start", // text
                "content_block_delta",
                "content_block_stop", // text 在 finish 关闭
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(parsed[1]["content_block"]["type"], "thinking");
        assert_eq!(parsed[2]["delta"]["thinking"], "hmm");
        assert_eq!(parsed[4]["content_block"]["type"], "text");
    }

    #[test]
    fn reasoning_effort_maps_to_thinking_budget_and_back() {
        // OpenAI → Anthropic：effort 档位映射到预算，max_tokens 过小时抬升。
        let body = json!({
            "model": "gpt-x",
            "messages": [{"role": "user", "content": "hi"}],
            "reasoning_effort": "high",
            "max_tokens": 2000,
        });
        let out = openai_request_to_anthropic(body).unwrap();
        assert_eq!(out["thinking"]["type"], "enabled");
        assert_eq!(out["thinking"]["budget_tokens"], 16384);
        assert_eq!(
            out["max_tokens"], 17408,
            "budget >= max_tokens 时抬升 max_tokens，避免每个请求都 400"
        );

        // Anthropic → OpenAI：预算映射回档位。
        let body = json!({
            "model": "claude-x",
            "messages": [],
            "thinking": {"type": "enabled", "budget_tokens": 4096},
        });
        let out = anthropic_request_to_openai(body).unwrap();
        assert_eq!(out["reasoning_effort"], "medium");
        // thinking.type != enabled 时不设置。
        let body = json!({
            "model": "claude-x",
            "messages": [],
            "thinking": {"type": "disabled", "budget_tokens": 4096},
        });
        let out = anthropic_request_to_openai(body).unwrap();
        assert!(out.get("reasoning_effort").is_none());
    }
}
