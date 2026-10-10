//! Google Gemini（generativelanguage REST）双向翻译：作为上游（入站 openai /
//! anthropic / responses → `:generateContent`）与作为入站（Gemini 应用 → 三种
//! 上游）各一套请求、非流式响应、流式转换。
//!
//! 结构性差异的处理约定：model 在 URL 路径不在请求体、流式由端点
//! （`:streamGenerateContent?alt=sse`）决定、鉴权走 `x-goog-api-key`——三者都
//! 由 proxy 层负责，本模块的请求体不含 model/stream 字段。角色 user/model；
//! 内容一律 parts 数组；functionCall.args 是对象而 openai arguments 是字符串，
//! 互转时序列化/解析。Gemini 的 function call **没有 call id**：转出到
//! openai/anthropic/responses 时按 `call_{name}` 合成 id（同名并行调用会撞
//! id，已知取舍）；转入时工具结果的名字从同一请求的历史 assistant 块反查。
//! 推理：thought part（text 带 `thought:true`）↔ thinking / reasoning_content /
//! reasoning summary；thinkingConfig.thinkingBudget 与 anthropic 预算同语义直传，
//! 与 openai effort 档位共用 effort_to_budget 映射表。usage 走 usageMetadata
//! （promptTokenCount / candidatesTokenCount / cachedContentTokenCount /
//! thoughtsTokenCount）。

use serde_json::{Value, json};
use toktol_core::error::{Error, Result};
use toktol_core::model::TokenUsage;

use super::{budget_to_effort, effort_to_budget};

fn bad(message: impl Into<String>) -> Error {
    Error::Internal(format!("translate/gemini: {}", message.into()))
}

/// Gemini function call 无 id：转出时按名字合成（见模块文档取舍）。
fn synthetic_call_id(name: &str) -> String {
    format!("call_{name}")
}

// ── 共享小件 ────────────────────────────────────────────────

/// parts 里的纯文本（text part 拼接，忽略 thought 与 functionCall）。
fn parts_text(parts: &Value) -> String {
    parts
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

/// parts 一次拆齐：正文、推理（thought parts）、functionCall 列表。
struct GeminiParts {
    text: String,
    reasoning: String,
    calls: Vec<Value>,
}

fn split_gemini_parts(parts: &Value) -> GeminiParts {
    let mut out = GeminiParts {
        text: String::new(),
        reasoning: String::new(),
        calls: Vec::new(),
    };
    if let Some(arr) = parts.as_array() {
        for part in arr {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                out.reasoning
                    .push_str(part.get("text").and_then(Value::as_str).unwrap_or(""));
            } else if let Some(call) = part.get("functionCall") {
                out.calls.push(call.clone());
            } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                out.text.push_str(text);
            }
        }
    }
    out
}

/// usageMetadata → TokenUsage；缺字段按 0。
pub(super) fn extract_gemini_usage(body: &Value) -> Option<TokenUsage> {
    let usage = body.get("usageMetadata")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("promptTokenCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        output_tokens: usage
            .get("candidatesTokenCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_read_tokens: usage
            .get("cachedContentTokenCount")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: usage.get("thoughtsTokenCount").and_then(Value::as_i64),
    })
}

/// TokenUsage → usageMetadata（入站 Gemini 方向的响应体重建）。
pub(super) fn gemini_usage_json(usage: &TokenUsage) -> Value {
    let mut out = json!({
        "promptTokenCount": usage.input_tokens,
        "candidatesTokenCount": usage.output_tokens,
        "totalTokenCount": usage.input_tokens + usage.output_tokens,
    });
    if usage.cache_read_tokens > 0 {
        out["cachedContentTokenCount"] = json!(usage.cache_read_tokens);
    }
    if let Some(reasoning) = usage.reasoning_tokens {
        out["thoughtsTokenCount"] = json!(reasoning);
    }
    out
}

fn finish_to_openai(finish: Option<&Value>, has_tools: bool) -> &'static str {
    if has_tools {
        return "tool_calls";
    }
    match finish.and_then(Value::as_str) {
        Some("MAX_TOKENS") => "length",
        Some("SAFETY") | Some("PROHIBITED_CONTENT") | Some("BLOCKLIST") => "content_filter",
        _ => "stop",
    }
}

fn finish_to_anthropic(finish: Option<&Value>, has_tools: bool) -> &'static str {
    if has_tools {
        return "tool_use";
    }
    match finish.and_then(Value::as_str) {
        Some("MAX_TOKENS") => "max_tokens",
        Some("SAFETY") | Some("PROHIBITED_CONTENT") | Some("BLOCKLIST") => "refusal",
        _ => "end_turn",
    }
}

fn openai_finish_to_gemini(finish: Option<&Value>) -> &'static str {
    match finish.and_then(Value::as_str) {
        Some("length") => "MAX_TOKENS",
        Some("content_filter") => "SAFETY",
        _ => "STOP",
    }
}

fn anthropic_stop_to_gemini(stop: Option<&Value>) -> &'static str {
    match stop.and_then(Value::as_str) {
        Some("max_tokens") => "MAX_TOKENS",
        Some("refusal") => "SAFETY",
        _ => "STOP",
    }
}

/// openai image_url part 的 URL → Gemini inlineData part。http URL 无对等
/// 表达（Gemini REST 只收 base64 内联），明确报错而不是静默丢图。
fn image_url_to_inline_data(url: &str) -> Result<Value> {
    let (media_type, data) = super::parse_data_url(url)
        .ok_or_else(|| bad("gemini 上游只支持 base64 图片：http URL 无法转写，请改用 data URL"))?;
    Ok(json!({"inlineData": {"mimeType": media_type, "data": data}}))
}

/// Gemini inlineData part → openai image_url part（回包成 data URL）。
fn inline_data_to_image_url(part: &Value) -> Option<Value> {
    let inline = part.get("inlineData")?;
    let mime = inline.get("mimeType").and_then(Value::as_str)?;
    let data = inline.get("data").and_then(Value::as_str)?;
    Some(json!({
        "type": "image_url",
        "image_url": {"url": format!("data:{mime};base64,{data}")},
    }))
}

/// 从 openai messages 里反查 tool_call_id → 工具名（functionResponse 需要
/// 名字而 tool 消息只带 id）。
fn openai_tool_names(messages: &[Value]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(Value::as_str),
                    call.pointer("/function/name").and_then(Value::as_str),
                ) {
                    map.insert(id.to_string(), name.to_string());
                }
            }
        }
    }
    map
}

/// 从 anthropic blocks 里反查 tool_use id → 名字。
fn anthropic_tool_names(messages: &[Value]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for message in messages {
        if let Some(blocks) = message.get("content").and_then(Value::as_array) {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && let (Some(id), Some(name)) = (
                        block.get("id").and_then(Value::as_str),
                        block.get("name").and_then(Value::as_str),
                    )
                {
                    map.insert(id.to_string(), name.to_string());
                }
            }
        }
    }
    map
}

/// 从 responses input items 里反查 call_id → 名字。
fn responses_tool_names(items: &[Value]) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for item in items {
        if item.get("type").and_then(Value::as_str) == Some("function_call")
            && let (Some(id), Some(name)) = (
                item.get("call_id").and_then(Value::as_str),
                item.get("name").and_then(Value::as_str),
            )
        {
            map.insert(id.to_string(), name.to_string());
        }
    }
    map
}

// ── 上游方向：入站请求 → Gemini 请求 ────────────────────────

fn openai_part_to_gemini(part: &Value) -> Result<Option<Value>> {
    match part.get("type").and_then(Value::as_str) {
        Some("image_url") => {
            let url = part
                .pointer("/image_url/url")
                .and_then(Value::as_str)
                .unwrap_or("");
            Ok(Some(image_url_to_inline_data(url)?))
        }
        _ => {
            let text = part.get("text").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                Ok(None)
            } else {
                Ok(Some(json!({"text": text})))
            }
        }
    }
}

/// openai message content（string 或多段）→ Gemini parts 数组。
fn openai_content_to_gemini_parts(message: &Value) -> Result<Vec<Value>> {
    match message.get("content") {
        Some(Value::String(text)) if !text.is_empty() => Ok(vec![json!({"text": text})]),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for part in parts {
                if let Some(part) = openai_part_to_gemini(part)? {
                    out.push(part);
                }
            }
            Ok(out)
        }
        _ => Ok(vec![]),
    }
}

fn gemini_tools_from_openai(body: &Value) -> Option<Value> {
    let tools = body.get("tools")?.as_array()?;
    let mapped: Vec<Value> = tools
        .iter()
        .filter_map(|tool| {
            let f = tool.get("function")?;
            Some(json!({
                "name": f.get("name")?,
                "description": f.get("description").cloned().unwrap_or(json!("")),
                "parameters": f.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
            }))
        })
        .collect();
    if mapped.is_empty() {
        None
    } else {
        Some(json!([{"functionDeclarations": mapped}]))
    }
}

fn openai_tool_choice_to_gemini(body: &Value, out: &mut Value) {
    match body.get("tool_choice") {
        Some(Value::String(s)) if s == "auto" => {
            out["toolConfig"] = json!({"functionCallingConfig": {"mode": "AUTO"}});
        }
        Some(Value::String(s)) if s == "none" => {
            out["toolConfig"] = json!({"functionCallingConfig": {"mode": "NONE"}});
        }
        Some(Value::String(s)) if s == "required" => {
            out["toolConfig"] = json!({"functionCallingConfig": {"mode": "ANY"}});
        }
        Some(choice) if choice.pointer("/function/name").is_some() => {
            out["toolConfig"] = json!({"functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [choice.pointer("/function/name")],
            }});
        }
        _ => {}
    }
}

/// OpenAI Chat Completions 请求 → Gemini 请求体。
pub fn openai_chat_to_gemini(body: Value) -> Result<Value> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 messages"))?;
    let tool_names = openai_tool_names(&messages);

    let mut system: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    for message in &messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("message 缺少 role"))?;
        match role {
            "system" | "developer" => {
                if let Some(text) = super::message_text(message)
                    && !text.is_empty()
                {
                    system.push(json!({"text": text}));
                }
            }
            "user" => {
                contents.push(json!({
                    "role": "user",
                    "parts": openai_content_to_gemini_parts(message)?,
                }));
            }
            "assistant" => {
                let mut parts = openai_content_to_gemini_parts(message)?;
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        parts.push(json!({"functionCall": {
                            "name": call.pointer("/function/name"),
                            "args": call
                                .pointer("/function/arguments")
                                .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                                .unwrap_or(json!({})),
                        }}));
                    }
                }
                contents.push(json!({"role": "model", "parts": parts}));
            }
            "tool" => {
                let id = message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = tool_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_string());
                contents.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": {
                        "name": name,
                        "response": {"result": message.get("content").cloned().unwrap_or(json!(""))},
                    }}],
                }));
            }
            other => return Err(bad(format!("message role 不支持: {other}"))),
        }
    }

    let mut generation = json!({
        "maxOutputTokens": body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .cloned()
            .unwrap_or(json!(4096)),
    });
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(v) = body.get(from) {
            generation[to] = v.clone();
        }
    }
    if let Some(stop) = body.get("stop") {
        generation["stopSequences"] = match stop {
            Value::String(s) => json!([s]),
            Value::Array(items) => Value::Array(items.clone()),
            _ => return Err(bad("openai stop 只支持字符串或数组")),
        };
    }
    // effort 档位 ↔ thinkingBudget 共用约定映射表。
    if let Some(effort) = body.get("reasoning_effort").and_then(Value::as_str)
        && let Some(budget) = effort_to_budget(effort)
    {
        generation["thinkingConfig"] = json!({"thinkingBudget": budget});
    }

    let mut out = json!({"contents": contents, "generationConfig": generation});
    if !system.is_empty() {
        out["systemInstruction"] = json!({"parts": system});
    }
    if let Some(tools) = gemini_tools_from_openai(&body) {
        out["tools"] = tools;
    }
    openai_tool_choice_to_gemini(&body, &mut out);
    Ok(out)
}

/// Anthropic Messages 请求 → Gemini 请求体。
pub fn anthropic_to_gemini(body: Value) -> Result<Value> {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 messages"))?;
    let tool_names = anthropic_tool_names(&messages);

    let mut system: Vec<Value> = Vec::new();
    if let Some(system_value) = body.get("system")
        && let Some(text) = super::message_text(&json!({"content": system_value}))
        && !text.is_empty()
    {
        system.push(json!({"text": text}));
    }

    let mut contents: Vec<Value> = Vec::new();
    for message in &messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("message 缺少 role"))?;
        let content = message.get("content").cloned().unwrap_or(json!(""));
        let blocks = content.as_array().cloned().unwrap_or_else(|| {
            vec![json!({"type": "text", "text": content.as_str().unwrap_or_default()})]
        });
        let mut parts: Vec<Value> = Vec::new();
        for block in &blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
                        parts.push(json!({"text": text}));
                    }
                }
                Some("image") => {
                    // base64 source → inlineData；web URL beta 形态无对等表达，跳过。
                    if let Some(source) = block.get("source")
                        && source.get("type").and_then(Value::as_str) == Some("base64")
                    {
                        let media = source
                            .get("media_type")
                            .and_then(Value::as_str)
                            .unwrap_or("image/png");
                        let data = source.get("data").and_then(Value::as_str).unwrap_or("");
                        parts.push(json!({"inlineData": {"mimeType": media, "data": data}}));
                    }
                }
                Some("tool_use") => parts.push(json!({"functionCall": {
                    "name": block.get("name"),
                    "args": block.get("input").cloned().unwrap_or(json!({})),
                }})),
                Some("tool_result") => {
                    let id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let name = tool_names
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| id.to_string());
                    parts.push(json!({"functionResponse": {
                        "name": name,
                        "response": {"result": block.get("content").cloned().unwrap_or(json!(""))},
                    }}));
                }
                // thinking 历史块丢弃（与 anthropic→openai 同约定）。
                _ => {}
            }
        }
        let gemini_role = if role == "assistant" { "model" } else { "user" };
        contents.push(json!({"role": gemini_role, "parts": parts}));
    }

    let mut generation = json!({
        "maxOutputTokens": body.get("max_tokens").cloned().unwrap_or(json!(4096)),
    });
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(v) = body.get(from) {
            generation[to] = v.clone();
        }
    }
    if let Some(stops) = body.get("stop_sequences").and_then(Value::as_array) {
        generation["stopSequences"] = Value::Array(stops.clone());
    }
    // Anthropic 预算与 Gemini thinkingBudget 同语义（token 数），直传。
    if body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled")
        && let Some(budget) = body
            .pointer("/thinking/budget_tokens")
            .and_then(Value::as_i64)
    {
        generation["thinkingConfig"] = json!({"thinkingBudget": budget});
    }

    let mut out = json!({"contents": contents, "generationConfig": generation});
    if !system.is_empty() {
        out["systemInstruction"] = json!({"parts": system});
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                Some(json!({
                    "name": tool.get("name")?,
                    "description": tool.get("description").cloned().unwrap_or(json!("")),
                    "parameters": tool.get("input_schema").cloned().unwrap_or(json!({"type": "object"})),
                }))
            })
            .collect();
        if !mapped.is_empty() {
            out["tools"] = json!([{"functionDeclarations": mapped}]);
        }
    }
    match body
        .get("tool_choice")
        .and_then(|c| c.get("type"))
        .and_then(Value::as_str)
    {
        Some("auto") => out["toolConfig"] = json!({"functionCallingConfig": {"mode": "AUTO"}}),
        Some("none") => out["toolConfig"] = json!({"functionCallingConfig": {"mode": "NONE"}}),
        Some("any") => out["toolConfig"] = json!({"functionCallingConfig": {"mode": "ANY"}}),
        Some("tool") => {
            out["toolConfig"] = json!({"functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [body.pointer("/tool_choice/name")],
            }});
        }
        _ => {}
    }
    Ok(out)
}

/// Responses 请求 → Gemini 请求体。
pub fn responses_to_gemini(body: Value) -> Result<Value> {
    let items = body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tool_names = responses_tool_names(&items);

    let mut system: Vec<Value> = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        system.push(json!({"text": instructions}));
    }

    let mut contents: Vec<Value> = Vec::new();
    for item in &items {
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        match kind {
            "message" => {
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("message item 缺少 role"))?;
                let mut parts: Vec<Value> = Vec::new();
                match item.get("content") {
                    Some(Value::String(text)) if !text.is_empty() => {
                        parts.push(json!({"text": text}));
                    }
                    Some(Value::Array(content_parts)) => {
                        for part in content_parts {
                            match part.get("type").and_then(Value::as_str) {
                                Some("input_image") => {
                                    let url =
                                        part.get("image_url").and_then(Value::as_str).unwrap_or("");
                                    parts.push(image_url_to_inline_data(url)?);
                                }
                                _ => {
                                    let text =
                                        part.get("text").and_then(Value::as_str).unwrap_or("");
                                    if !text.is_empty() {
                                        parts.push(json!({"text": text}));
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let gemini_role = if role == "assistant" || role == "model" {
                    "model"
                } else {
                    "user"
                };
                contents.push(json!({"role": gemini_role, "parts": parts}));
            }
            "function_call" => contents.push(json!({
                "role": "model",
                "parts": [{"functionCall": {
                    "name": item.get("name"),
                    "args": item
                        .get("arguments")
                        .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                        .unwrap_or(json!({})),
                }}],
            })),
            "function_call_output" => {
                let id = item.get("call_id").and_then(Value::as_str).unwrap_or("");
                let name = tool_names
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| id.to_string());
                contents.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": {
                        "name": name,
                        "response": {"result": item.get("output").cloned().unwrap_or(json!(""))},
                    }}],
                }));
            }
            _ => {}
        }
    }

    let mut generation = json!({
        "maxOutputTokens": body.get("max_output_tokens").cloned().unwrap_or(json!(4096)),
    });
    for (from, to) in [("temperature", "temperature"), ("top_p", "topP")] {
        if let Some(v) = body.get(from) {
            generation[to] = v.clone();
        }
    }
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(Value::as_str)
        && let Some(budget) = effort_to_budget(effort)
    {
        generation["thinkingConfig"] = json!({"thinkingBudget": budget});
    }

    let mut out = json!({"contents": contents, "generationConfig": generation});
    if !system.is_empty() {
        out["systemInstruction"] = json!({"parts": system});
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let mapped: Vec<Value> = tools
            .iter()
            .filter_map(|tool| {
                Some(json!({
                    "name": tool.get("name")?,
                    "description": tool.get("description").cloned().unwrap_or(json!("")),
                    "parameters": tool.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                }))
            })
            .collect();
        if !mapped.is_empty() {
            out["tools"] = json!([{"functionDeclarations": mapped}]);
        }
    }
    match body.get("tool_choice") {
        Some(Value::String(s)) if s == "auto" || s == "none" || s == "required" => {
            let mode = s.to_uppercase();
            out["toolConfig"] = json!({"functionCallingConfig": {"mode": mode}});
        }
        Some(choice) if choice.get("name").is_some() => {
            out["toolConfig"] = json!({"functionCallingConfig": {
                "mode": "ANY",
                "allowedFunctionNames": [choice.get("name")],
            }});
        }
        _ => {}
    }
    Ok(out)
}

// ── 上游方向：Gemini 响应 → 入站 ────────────────────────────

/// Gemini 响应 → OpenAI Chat completion。
pub fn gemini_to_openai_chat(body: Value) -> Result<Value> {
    let candidate = body
        .pointer("/candidates/0")
        .ok_or_else(|| bad("响应缺少 candidates[0]"))?;
    let parts = split_gemini_parts(candidate.pointer("/content/parts").unwrap_or(&Value::Null));
    let mut message = json!({"role": "assistant", "content": parts.text});
    if !parts.reasoning.is_empty() {
        message["reasoning_content"] = json!(parts.reasoning);
    }
    if !parts.calls.is_empty() {
        message["tool_calls"] = Value::Array(
            parts
                .calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    json!({
                        "index": index,
                        "id": synthetic_call_id(call.get("name").and_then(Value::as_str).unwrap_or("")),
                        "type": "function",
                        "function": {
                            "name": call.get("name"),
                            "arguments": call.get("args").map(|args| args.to_string()).unwrap_or_else(|| "{}".into()),
                        },
                    })
                })
                .collect(),
        );
    }
    let usage = extract_gemini_usage(&body).unwrap_or_default();
    Ok(json!({
        "id": format!("chatcmpl-toktol-{}", super::unix_now()),
        "object": "chat.completion",
        "created": super::unix_now(),
        "model": body.get("modelVersion").and_then(Value::as_str).unwrap_or_default(),
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": finish_to_openai(candidate.get("finishReason"), !parts.calls.is_empty()),
        }],
        "usage": super::openai_usage_json(&usage),
    }))
}

/// Gemini 响应 → Anthropic message。
pub fn gemini_to_anthropic(body: Value) -> Result<Value> {
    let candidate = body
        .pointer("/candidates/0")
        .ok_or_else(|| bad("响应缺少 candidates[0]"))?;
    let parts = split_gemini_parts(candidate.pointer("/content/parts").unwrap_or(&Value::Null));
    let mut blocks: Vec<Value> = Vec::new();
    if !parts.reasoning.is_empty() {
        blocks.push(json!({"type": "thinking", "thinking": parts.reasoning}));
    }
    if !parts.text.is_empty() {
        blocks.push(json!({"type": "text", "text": parts.text}));
    }
    for call in &parts.calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": synthetic_call_id(call.get("name").and_then(Value::as_str).unwrap_or("")),
            "name": call.get("name"),
            "input": call.get("args").cloned().unwrap_or(json!({})),
        }));
    }
    let usage = extract_gemini_usage(&body).unwrap_or_default();
    Ok(json!({
        "id": format!("msg-toktol-{}", super::unix_now()),
        "type": "message",
        "role": "assistant",
        "model": body.get("modelVersion").and_then(Value::as_str).unwrap_or_default(),
        "content": blocks,
        "stop_reason": finish_to_anthropic(candidate.get("finishReason"), !parts.calls.is_empty()),
        "usage": super::anthropic_usage_json(&usage),
    }))
}

/// Gemini 响应 → Responses response 对象。
pub fn gemini_to_responses(body: Value) -> Result<Value> {
    let candidate = body
        .pointer("/candidates/0")
        .ok_or_else(|| bad("响应缺少 candidates[0]"))?;
    let parts = split_gemini_parts(candidate.pointer("/content/parts").unwrap_or(&Value::Null));
    let mut output: Vec<Value> = Vec::new();
    if !parts.reasoning.is_empty() {
        output.push(json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": parts.reasoning}],
        }));
    }
    if !parts.text.is_empty() {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", super::unix_now()),
            "role": "assistant",
            "content": [{"type": "output_text", "text": parts.text}],
        }));
    }
    for call in &parts.calls {
        let name = call.get("name").and_then(Value::as_str).unwrap_or("");
        output.push(json!({
            "type": "function_call",
            "id": synthetic_call_id(name),
            "call_id": synthetic_call_id(name),
            "name": call.get("name"),
            "arguments": call.get("args").map(|args| args.to_string()).unwrap_or_else(|| "{}".into()),
        }));
    }
    let usage = extract_gemini_usage(&body).unwrap_or_default();
    let incomplete = candidate.get("finishReason").and_then(Value::as_str) == Some("MAX_TOKENS");
    Ok(json!({
        "id": format!("resp_toktol_{}", super::unix_now()),
        "object": "response",
        "created_at": super::unix_now(),
        "status": if incomplete { "incomplete" } else { "completed" },
        "model": body.get("modelVersion").and_then(Value::as_str).unwrap_or_default(),
        "output": output,
        "usage": {
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.input_tokens + usage.output_tokens,
            "input_tokens_details": {"cached_tokens": usage.cache_read_tokens},
            "output_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
        },
    }))
}

// ── 入站方向：Gemini 请求 → 上游请求 ────────────────────────

// functionResponse 自带 name，无需反查；只有转出 openai/anthropic/responses
// 时才需要合成 id，同名调用共用一个合成 id（见模块文档取舍）。

fn gemini_content_parts(content: &Value) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts.clone(),
        Value::String(text) => vec![json!({"text": text})],
        _ => vec![],
    }
}

/// Gemini 请求 → OpenAI Chat Completions 请求体。
pub fn gemini_to_openai_request(body: Value) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::new();
    if let Some(text) = parts_text_opt(body.pointer("/systemInstruction/parts"))
        && !text.is_empty()
    {
        messages.push(json!({"role": "system", "content": text}));
    }
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 contents"))?;
    for content in &contents {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let role = if role == "model" { "assistant" } else { "user" };
        let parts = gemini_content_parts(content.get("parts").unwrap_or(&Value::Null));
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut images: Vec<Value> = Vec::new();
        for part in &parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                reasoning.push_str(part.get("text").and_then(Value::as_str).unwrap_or(""));
            } else if let Some(call) = part.get("functionCall") {
                let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                tool_calls.push(json!({
                    "id": synthetic_call_id(name),
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": call.get("args").map(|args| args.to_string()).unwrap_or_else(|| "{}".into()),
                    },
                }));
            } else if let Some(part) = inline_data_to_image_url(part) {
                images.push(part);
            } else if let Some(part_text) = part.get("text").and_then(Value::as_str) {
                text.push_str(part_text);
            }
        }
        if role == "assistant" {
            let mut message = json!({"role": "assistant", "content": text});
            if !reasoning.is_empty() {
                message["reasoning_content"] = json!(reasoning);
            }
            if !tool_calls.is_empty() {
                message["tool_calls"] = Value::Array(tool_calls);
            }
            messages.push(message);
        } else {
            // user：有图时 content 必须是数组；纯 functionResponse 的内容不产生
            // 空 user 消息，只落 tool 消息。
            if !text.is_empty() || !images.is_empty() {
                if images.is_empty() {
                    messages.push(json!({"role": "user", "content": text}));
                } else {
                    let mut content_parts = Vec::new();
                    if !text.is_empty() {
                        content_parts.push(json!({"type": "text", "text": text}));
                    }
                    content_parts.extend(images);
                    messages.push(json!({"role": "user", "content": content_parts}));
                }
            }
            // functionResponse 的工具结果按 openai 语义独立成 tool 消息。
            for part in &parts {
                if let Some(response) = part.get("functionResponse") {
                    let name = response.get("name").and_then(Value::as_str).unwrap_or("");
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": synthetic_call_id(name),
                        "content": response
                            .pointer("/response/result")
                            .cloned()
                            .unwrap_or(json!("")),
                    }));
                }
            }
        }
    }

    let generation = body.get("generationConfig").cloned().unwrap_or(json!({}));
    let mut out = json!({"model": "", "messages": messages});
    for (from, to) in [
        ("/maxOutputTokens", "max_tokens"),
        ("/temperature", "temperature"),
        ("/topP", "top_p"),
    ] {
        if let Some(v) = generation.pointer(from) {
            out[to] = v.clone();
        }
    }
    if let Some(stops) = generation.get("stopSequences").and_then(Value::as_array) {
        out["stop"] = Value::Array(stops.clone());
    }
    // 预算 → effort 档位（与 anthropic→openai 同一张表）。
    if let Some(budget) = generation
        .pointer("/thinkingConfig/thinkingBudget")
        .and_then(Value::as_i64)
    {
        out["reasoning_effort"] = json!(budget_to_effort(budget));
    }
    if let Some(decls) = body
        .pointer("/tools/0/functionDeclarations")
        .and_then(Value::as_array)
    {
        out["tools"] = Value::Array(
            decls
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.get("name"),
                            "description": tool.get("description").cloned().unwrap_or(json!("")),
                            "parameters": tool.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                        },
                    })
                })
                .collect(),
        );
    }
    match body
        .pointer("/toolConfig/functionCallingConfig/mode")
        .and_then(Value::as_str)
    {
        Some("AUTO") => out["tool_choice"] = json!("auto"),
        Some("NONE") => out["tool_choice"] = json!("none"),
        Some("ANY") => out["tool_choice"] = json!("required"),
        _ => {}
    }
    Ok(out)
}

/// Gemini 请求 → Anthropic Messages 请求体。
pub fn gemini_to_anthropic_request(body: Value) -> Result<Value> {
    let mut messages: Vec<Value> = Vec::new();
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 contents"))?;
    for content in &contents {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let is_model = role == "model";
        let parts = gemini_content_parts(content.get("parts").unwrap_or(&Value::Null));
        let mut blocks: Vec<Value> = Vec::new();
        for part in &parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    blocks.push(json!({"type": "thinking", "thinking": text}));
                }
            } else if let Some(call) = part.get("functionCall") {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": synthetic_call_id(call.get("name").and_then(Value::as_str).unwrap_or("")),
                    "name": call.get("name"),
                    "input": call.get("args").cloned().unwrap_or(json!({})),
                }));
            } else if let Some(inline) = part.get("inlineData") {
                if let (Some(mime), Some(data)) = (
                    inline.get("mimeType").and_then(Value::as_str),
                    inline.get("data").and_then(Value::as_str),
                ) {
                    blocks.push(json!({"type": "image", "source": {
                        "type": "base64", "media_type": mime, "data": data}}));
                }
            } else if let Some(response) = part.get("functionResponse") {
                blocks.push(json!({
                    "type": "tool_result",
                    "tool_use_id": synthetic_call_id(
                        response.get("name").and_then(Value::as_str).unwrap_or("")),
                    "content": response.pointer("/response/result").cloned().unwrap_or(json!("")),
                }));
            } else if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                blocks.push(json!({"type": "text", "text": text}));
            }
        }
        let anthropic_role = if is_model { "assistant" } else { "user" };
        messages.push(json!({"role": anthropic_role, "content": blocks}));
    }

    let generation = body.get("generationConfig").cloned().unwrap_or(json!({}));
    let mut out = json!({
        "model": "",
        "messages": messages,
        "max_tokens": generation.get("maxOutputTokens").cloned().unwrap_or(json!(4096)),
    });
    // systemInstruction 对应 anthropic 顶层 system 字段，不是 user 消息。
    if let Some(text) = parts_text_opt(body.pointer("/systemInstruction/parts")) {
        out["system"] = json!(text);
    }
    for (from, to) in [("/temperature", "temperature"), ("/topP", "top_p")] {
        if let Some(v) = generation.pointer(from) {
            out[to] = v.clone();
        }
    }
    if let Some(stops) = generation.get("stopSequences").and_then(Value::as_array) {
        out["stop_sequences"] = Value::Array(stops.clone());
    }
    if let Some(budget) = generation
        .pointer("/thinkingConfig/thinkingBudget")
        .and_then(Value::as_i64)
    {
        out["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
    }
    if let Some(decls) = body
        .pointer("/tools/0/functionDeclarations")
        .and_then(Value::as_array)
    {
        out["tools"] = Value::Array(
            decls
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.get("name"),
                        "description": tool.get("description").cloned().unwrap_or(json!("")),
                        "input_schema": tool.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                    })
                })
                .collect(),
        );
    }
    match body
        .pointer("/toolConfig/functionCallingConfig/mode")
        .and_then(Value::as_str)
    {
        Some("AUTO") => out["tool_choice"] = json!({"type": "auto"}),
        Some("NONE") => out["tool_choice"] = json!({"type": "none"}),
        Some("ANY") => out["tool_choice"] = json!({"type": "any"}),
        _ => {}
    }
    Ok(out)
}

/// Gemini 请求 → Responses 请求体。
pub fn gemini_to_responses_request(body: Value) -> Result<Value> {
    let mut input: Vec<Value> = Vec::new();
    let contents = body
        .get("contents")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 contents"))?;
    for content in &contents {
        let role = content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let is_model = role == "model";
        let parts = gemini_content_parts(content.get("parts").unwrap_or(&Value::Null));
        let mut text_parts: Vec<Value> = Vec::new();
        for part in &parts {
            if part.get("thought").and_then(Value::as_bool) == Some(true) {
                continue; // 推理历史不入 Responses input（与 responses.rs 同取舍）。
            }
            if let Some(call) = part.get("functionCall") {
                let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                input.push(json!({
                    "type": "function_call",
                    "call_id": synthetic_call_id(name),
                    "name": name,
                    "arguments": call.get("args").map(|args| args.to_string()).unwrap_or_else(|| "{}".into()),
                }));
            } else if let Some(response) = part.get("functionResponse") {
                let name = response.get("name").and_then(Value::as_str).unwrap_or("");
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": synthetic_call_id(name),
                    "output": response.pointer("/response/result").cloned().unwrap_or(json!("")),
                }));
            } else if let Some(inline) = part.get("inlineData") {
                if let (Some(mime), Some(data)) = (
                    inline.get("mimeType").and_then(Value::as_str),
                    inline.get("data").and_then(Value::as_str),
                ) {
                    text_parts.push(json!({
                        "type": "input_image",
                        "image_url": format!("data:{mime};base64,{data}"),
                    }));
                }
            } else if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                let part_type = if is_model {
                    "output_text"
                } else {
                    "input_text"
                };
                text_parts.push(json!({"type": part_type, "text": text}));
            }
        }
        if !text_parts.is_empty() {
            let item_role = if is_model { "assistant" } else { "user" };
            input.push(json!({
                "type": "message",
                "role": item_role,
                "content": text_parts,
            }));
        }
    }

    let generation = body.get("generationConfig").cloned().unwrap_or(json!({}));
    let mut out = json!({"model": "", "input": input});
    if let Some(text) = parts_text_opt(body.pointer("/systemInstruction/parts"))
        && !text.is_empty()
    {
        out["instructions"] = json!(text);
    }
    if let Some(v) = generation.get("maxOutputTokens") {
        out["max_output_tokens"] = v.clone();
    }
    for (from, to) in [("/temperature", "temperature"), ("/topP", "top_p")] {
        if let Some(v) = generation.pointer(from) {
            out[to] = v.clone();
        }
    }
    if let Some(budget) = generation
        .pointer("/thinkingConfig/thinkingBudget")
        .and_then(Value::as_i64)
    {
        out["reasoning"] = json!({"effort": budget_to_effort(budget)});
    }
    if let Some(decls) = body
        .pointer("/tools/0/functionDeclarations")
        .and_then(Value::as_array)
    {
        out["tools"] = Value::Array(
            decls
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.get("name"),
                        "description": tool.get("description").cloned().unwrap_or(json!("")),
                        "parameters": tool.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                    })
                })
                .collect(),
        );
    }
    if let Some(mode) = body
        .pointer("/toolConfig/functionCallingConfig/mode")
        .and_then(Value::as_str)
        && matches!(mode, "AUTO" | "NONE" | "ANY")
    {
        out["tool_choice"] = json!(mode.to_lowercase());
    }
    Ok(out)
}

fn parts_text_opt(parts: Option<&Value>) -> Option<String> {
    let text = parts.map(parts_text).unwrap_or_default();
    if text.is_empty() { None } else { Some(text) }
}

// ── 入站方向：上游响应 → Gemini 响应 ────────────────────────

fn openai_message_to_gemini_parts(message: &Value) -> Result<Vec<Value>> {
    let mut parts: Vec<Value> = Vec::new();
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str)
        && !reasoning.is_empty()
    {
        parts.push(json!({"text": reasoning, "thought": true}));
    }
    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        parts.push(json!({"text": text}));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            parts.push(json!({"functionCall": {
                "name": call.pointer("/function/name"),
                "args": call
                    .pointer("/function/arguments")
                    .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                    .unwrap_or(json!({})),
            }}));
        }
    }
    Ok(parts)
}

/// OpenAI Chat completion → Gemini 响应体。
pub fn openai_completion_to_gemini(completion: Value) -> Result<Value> {
    let choice = completion
        .pointer("/choices/0")
        .ok_or_else(|| bad("响应缺少 choices[0]"))?;
    let parts = openai_message_to_gemini_parts(choice.get("message").unwrap_or(&Value::Null))?;
    let usage = super::extract_openai_usage(&completion).unwrap_or_default();
    Ok(json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": openai_finish_to_gemini(choice.get("finish_reason")),
        }],
        "modelVersion": completion.get("model").cloned().unwrap_or(json!("")),
        "usageMetadata": gemini_usage_json(&usage),
    }))
}

/// Anthropic message → Gemini 响应体。
pub fn anthropic_message_to_gemini(message: Value) -> Result<Value> {
    let mut parts: Vec<Value> = Vec::new();
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("thinking") => parts.push(json!({
                    "text": block.get("thinking").and_then(Value::as_str).unwrap_or(""),
                    "thought": true,
                })),
                Some("text") => parts.push(json!({
                    "text": block.get("text").and_then(Value::as_str).unwrap_or(""),
                })),
                Some("tool_use") => parts.push(json!({"functionCall": {
                    "name": block.get("name"),
                    "args": block.get("input").cloned().unwrap_or(json!({})),
                }})),
                _ => {}
            }
        }
    }
    let usage = super::extract_anthropic_usage(&message).unwrap_or_default();
    Ok(json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": anthropic_stop_to_gemini(message.get("stop_reason")),
        }],
        "modelVersion": message.get("model").cloned().unwrap_or(json!("")),
        "usageMetadata": gemini_usage_json(&usage),
    }))
}

/// Responses response 对象 → Gemini 响应体。
pub fn responses_object_to_gemini(response: Value) -> Result<Value> {
    let mut parts: Vec<Value> = Vec::new();
    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("reasoning") => {
                    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                        for entry in summary {
                            if let Some(text) = entry.get("text").and_then(Value::as_str) {
                                parts.push(json!({"text": text, "thought": true}));
                            }
                        }
                    }
                }
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                parts.push(json!({"text": text}));
                            }
                        }
                    }
                }
                Some("function_call") => parts.push(json!({"functionCall": {
                    "name": item.get("name"),
                    "args": item
                        .get("arguments")
                        .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                        .unwrap_or(json!({})),
                }})),
                _ => {}
            }
        }
    }
    let usage = super::responses::extract_responses_usage(&response).unwrap_or_default();
    let finish = if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        "MAX_TOKENS"
    } else {
        "STOP"
    };
    Ok(json!({
        "candidates": [{
            "content": {"role": "model", "parts": parts},
            "finishReason": finish,
        }],
        "modelVersion": response.get("model").cloned().unwrap_or(json!("")),
        "usageMetadata": gemini_usage_json(&usage),
    }))
}

// ── 流式：Gemini 上游 SSE → 入站 ────────────────────────────

/// 从一条 Gemini SSE 载荷里取 (parts, finishReason, usageMetadata)。
fn gemini_chunk(event: &Value) -> (Value, Option<Value>, Option<Value>) {
    let parts = event
        .pointer("/candidates/0/content/parts")
        .cloned()
        .unwrap_or(Value::Null);
    let finish = event
        .pointer("/candidates/0/finishReason")
        .filter(|f| !f.is_null())
        .cloned();
    let usage = event.get("usageMetadata").cloned();
    (parts, finish, usage)
}

/// Gemini SSE → 客户端 OpenAI chunks。
pub struct GeminiToOpenAiStream {
    id: String,
    started: bool,
    terminated: bool,
    next_tool: usize,
    seen_tool: bool,
    usage: TokenUsage,
}

impl GeminiToOpenAiStream {
    pub fn new() -> Self {
        Self {
            id: format!("chatcmpl-toktol-{}", super::unix_now()),
            started: false,
            terminated: false,
            next_tool: 0,
            seen_tool: false,
            usage: TokenUsage::default(),
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        super::data_block(
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": super::unix_now(),
                "model": "",
                "choices": [{
                    "index": 0,
                    "delta": delta,
                    "finish_reason": finish,
                }],
            })
            .to_string(),
        )
    }

    fn usage_chunk(&self) -> String {
        super::data_block(
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": super::unix_now(),
                "model": "",
                "choices": [],
                "usage": super::openai_usage_json(&self.usage),
            })
            .to_string(),
        )
    }
}

impl Default for GeminiToOpenAiStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for GeminiToOpenAiStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if self.terminated {
            return vec![];
        }
        let (parts, finish, usage) = gemini_chunk(&event);
        if let Some(usage) = usage.filter(|u| u.is_object())
            && let Some(parsed) = extract_gemini_usage(&json!({"usageMetadata": usage}))
        {
            self.usage = parsed;
        }
        let mut out = Vec::new();
        if let Some(arr) = parts.as_array() {
            for part in arr {
                if let Some(call) = part.get("functionCall") {
                    let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                    let tool = self.next_tool;
                    self.next_tool += 1;
                    self.seen_tool = true;
                    out.push(self.chunk(
                        json!({"tool_calls": [{
                            "index": tool,
                            "id": synthetic_call_id(name),
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": call.get("args").map(|args| args.to_string()).unwrap_or_else(|| "{}".into()),
                            },
                        }]}),
                        None,
                    ));
                } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if text.is_empty() {
                        continue;
                    }
                    if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        out.push(self.chunk(json!({"reasoning_content": text}), None));
                    } else {
                        if !self.started {
                            self.started = true;
                            out.push(self.chunk(json!({"role": "assistant", "content": ""}), None));
                        }
                        out.push(self.chunk(json!({"content": text}), None));
                    }
                }
            }
        }
        if let Some(finish) = finish {
            self.terminated = true;
            let finish = finish_to_openai(Some(&finish), self.seen_tool);
            out.push(self.chunk(json!({}), Some(finish)));
            out.push(self.usage_chunk());
            out.push(super::data_block("[DONE]"));
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        if self.started && !self.terminated {
            self.terminated = true;
            let finish = if self.seen_tool { "tool_calls" } else { "stop" };
            vec![
                self.chunk(json!({}), Some(finish)),
                self.usage_chunk(),
                super::data_block("[DONE]"),
            ]
        } else {
            vec![]
        }
    }
}

/// Gemini SSE → 客户端 Anthropic 事件。
pub struct GeminiToAnthropicStream {
    message_started: bool,
    terminated: bool,
    next_block: usize,
    open_text_block: Option<usize>,
    open_thinking_block: Option<usize>,
    seen_tool: bool,
    usage: TokenUsage,
}

impl GeminiToAnthropicStream {
    pub fn new() -> Self {
        Self {
            message_started: false,
            terminated: false,
            next_block: 0,
            open_text_block: None,
            open_thinking_block: None,
            seen_tool: false,
            usage: TokenUsage::default(),
        }
    }

    fn event(&self, value: Value) -> String {
        super::data_block(&value.to_string())
    }

    fn stop_event(&mut self, out: &mut Vec<String>, index: usize) {
        out.push(self.event(json!({"type": "content_block_stop", "index": index})));
    }

    /// 懒开 text / thinking 块：Anthropic 块不重叠，开新块前关掉另一块。
    fn lazy_start(&mut self, out: &mut Vec<String>, kind: &'static str) -> usize {
        let is_thinking = kind == "thinking";
        let open_other = if is_thinking {
            self.open_text_block.take()
        } else {
            self.open_thinking_block.take()
        };
        if let Some(index) = open_other {
            self.stop_event(out, index);
        }
        let index = self.next_block;
        self.next_block += 1;
        let block = if is_thinking {
            self.open_thinking_block = Some(index);
            json!({"type": "thinking", "thinking": ""})
        } else {
            self.open_text_block = Some(index);
            json!({"type": "text", "text": ""})
        };
        out.push(self.event(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": block,
        })));
        index
    }

    fn finish_stream(&mut self, stop_reason: &'static str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(index) = self.open_text_block.take() {
            self.stop_event(&mut out, index);
        }
        if let Some(index) = self.open_thinking_block.take() {
            self.stop_event(&mut out, index);
        }
        out.push(self.event(json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason, "stop_sequence": Value::Null},
            "usage": {
                "input_tokens": self.usage.input_tokens,
                "output_tokens": self.usage.output_tokens,
            },
        })));
        out.push(self.event(json!({"type": "message_stop"})));
        self.message_started = false;
        out
    }
}

impl Default for GeminiToAnthropicStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for GeminiToAnthropicStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if self.terminated {
            return vec![];
        }
        let (parts, finish, usage) = gemini_chunk(&event);
        if let Some(usage) = usage.filter(|u| u.is_object())
            && let Some(parsed) = extract_gemini_usage(&json!({"usageMetadata": usage}))
        {
            self.usage = parsed;
        }
        let mut out = Vec::new();
        if !self.message_started {
            self.message_started = true;
            out.push(self.event(json!({
                "type": "message_start",
                "message": {
                    "id": format!("msg-toktol-{}", super::unix_now()),
                    "type": "message",
                    "role": "assistant",
                    "model": event.get("modelVersion").and_then(Value::as_str).unwrap_or(""),
                    "content": [],
                    "stop_reason": Value::Null,
                    "usage": {"input_tokens": 0, "output_tokens": 0},
                },
            })));
        }
        if let Some(arr) = parts.as_array() {
            for part in arr {
                if let Some(call) = part.get("functionCall") {
                    // functionCall 是完整对象：先关掉开着的文本块，再
                    // 开块 → 参数一次到位 → 关块。
                    if let Some(index) = self.open_text_block.take() {
                        self.stop_event(&mut out, index);
                    }
                    let index = self.next_block;
                    self.next_block += 1;
                    self.seen_tool = true;
                    out.push(self.event(json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": synthetic_call_id(call.get("name").and_then(Value::as_str).unwrap_or("")),
                            "name": call.get("name"),
                            "input": {},
                        },
                    })));
                    let args = call
                        .get("args")
                        .map(|args| args.to_string())
                        .unwrap_or_else(|| "{}".into());
                    out.push(self.event(json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": {"type": "input_json_delta", "partial_json": args},
                    })));
                    self.stop_event(&mut out, index);
                } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if text.is_empty() {
                        continue;
                    }
                    let thinking = part.get("thought").and_then(Value::as_bool) == Some(true);
                    let mut pre = Vec::new();
                    let index =
                        self.lazy_start(&mut pre, if thinking { "thinking" } else { "text" });
                    out.extend(pre);
                    out.push(self.event(json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": if thinking {
                            json!({"type": "thinking_delta", "thinking": text})
                        } else {
                            json!({"type": "text_delta", "text": text})
                        },
                    })));
                }
            }
        }
        if let Some(finish) = finish {
            self.terminated = true;
            let stop_reason = finish_to_anthropic(Some(&finish), self.seen_tool);
            out.extend(self.finish_stream(stop_reason));
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        if self.message_started && !self.terminated {
            self.terminated = true;
            let stop_reason = if self.seen_tool {
                "tool_use"
            } else {
                "end_turn"
            };
            self.finish_stream(stop_reason)
        } else {
            vec![]
        }
    }
}

/// Gemini SSE → 客户端 Responses 事件。
pub struct GeminiToResponsesStream {
    created: bool,
    terminated: bool,
    /// 文本项：开块后累积正文，finish 时补 done 事件。
    text_open: bool,
    text: String,
    next_output: usize,
    usage: TokenUsage,
}

impl GeminiToResponsesStream {
    pub fn new() -> Self {
        Self {
            created: false,
            terminated: false,
            text_open: false,
            text: String::new(),
            next_output: 0,
            usage: TokenUsage::default(),
        }
    }

    fn event(name: &str, payload: Value) -> String {
        format!("event: {name}\ndata: {}\n\n", payload)
    }

    fn ensure_created(&mut self, out: &mut Vec<String>) {
        if !self.created {
            self.created = true;
            out.push(Self::event(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": {
                        "id": format!("resp_toktol_{}", super::unix_now()),
                        "object": "response",
                        "created_at": super::unix_now(),
                        "status": "in_progress",
                        "model": "",
                        "output": [],
                        "usage": Value::Null,
                    },
                    "sequence_number": 0,
                }),
            ));
        }
    }

    fn usage_json(&self) -> Value {
        json!({
            "input_tokens": self.usage.input_tokens,
            "output_tokens": self.usage.output_tokens,
            "total_tokens": self.usage.input_tokens + self.usage.output_tokens,
            "input_tokens_details": {"cached_tokens": self.usage.cache_read_tokens},
            "output_tokens_details": {"reasoning_tokens": self.usage.reasoning_tokens},
        })
    }
}

impl Default for GeminiToResponsesStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for GeminiToResponsesStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if self.terminated {
            return vec![];
        }
        let (parts, finish, usage) = gemini_chunk(&event);
        if let Some(usage) = usage.filter(|u| u.is_object())
            && let Some(parsed) = extract_gemini_usage(&json!({"usageMetadata": usage}))
        {
            self.usage = parsed;
        }
        let mut out = Vec::new();
        self.ensure_created(&mut out);
        if let Some(arr) = parts.as_array() {
            for part in arr {
                // thought part 丢弃（入站方向推理不回放的既有取舍）。
                if let Some(call) = part.get("functionCall") {
                    let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                    let call_id = synthetic_call_id(name);
                    let output_index = self.next_output;
                    self.next_output += 1;
                    let arguments = call
                        .get("args")
                        .map(|args| args.to_string())
                        .unwrap_or_else(|| "{}".into());
                    out.push(Self::event(
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {"type": "function_call", "id": call_id, "call_id": call_id, "name": name, "arguments": ""},
                        }),
                    ));
                    out.push(Self::event(
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "output_index": output_index,
                            "delta": arguments,
                        }),
                    ));
                    out.push(Self::event(
                        "response.function_call_arguments.done",
                        json!({
                            "type": "response.function_call_arguments.done",
                            "output_index": output_index,
                            "arguments": arguments,
                        }),
                    ));
                    out.push(Self::event(
                        "response.output_item.done",
                        json!({
                            "type": "response.output_item.done",
                            "output_index": output_index,
                            "item": {"type": "function_call", "id": call_id, "call_id": call_id, "name": name, "arguments": arguments},
                        }),
                    ));
                } else if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if text.is_empty() || part.get("thought").and_then(Value::as_bool) == Some(true)
                    {
                        continue;
                    }
                    if !self.text_open {
                        self.text_open = true;
                        out.push(Self::event(
                            "response.output_item.added",
                            json!({
                                "type": "response.output_item.added",
                                "output_index": self.next_output,
                                "item": {"type": "message", "id": format!("msg_{}", super::unix_now()), "role": "assistant", "content": []},
                            }),
                        ));
                        self.next_output += 1;
                    }
                    self.text.push_str(text);
                    out.push(Self::event(
                        "response.output_text.delta",
                        json!({
                            "type": "response.output_text.delta",
                            "output_index": self.next_output - 1,
                            "content_index": 0,
                            "delta": text,
                        }),
                    ));
                }
            }
        }
        if let Some(finish) = finish {
            self.terminated = true;
            if self.text_open {
                out.push(Self::event(
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "output_index": 0,
                        "content_index": 0,
                        "text": self.text,
                    }),
                ));
                out.push(Self::event(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": self.text}]},
                    }),
                ));
            }
            out.push(Self::event(
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": format!("resp_toktol_{}", super::unix_now()),
                        "object": "response",
                        "status": if finish.as_str() == Some("MAX_TOKENS") { "incomplete" } else { "completed" },
                        "model": "",
                        "usage": self.usage_json(),
                    },
                }),
            ));
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        // 上游断流未发 finishReason 的兜底：补 done + completed。
        if self.created && !self.terminated {
            self.terminated = true;
            let mut out = Vec::new();
            if self.text_open {
                out.push(Self::event(
                    "response.output_text.done",
                    json!({
                        "type": "response.output_text.done",
                        "output_index": 0,
                        "content_index": 0,
                        "text": self.text,
                    }),
                ));
                out.push(Self::event(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": self.text}]},
                    }),
                ));
            }
            out.push(Self::event(
                "response.completed",
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": format!("resp_toktol_{}", super::unix_now()),
                        "object": "response",
                        "status": "completed",
                        "model": "",
                        "usage": self.usage_json(),
                    },
                }),
            ));
            out
        } else {
            vec![]
        }
    }
}

// ── 流式：上游 SSE → Gemini 入站 ────────────────────────────

/// 构造一条 Gemini 入站 SSE 块（data 载荷；Gemini 流没有 event: 行与 [DONE]）。
fn gemini_chunk_block(parts: Value, finish: Option<&str>, usage: Option<&Value>) -> String {
    let mut candidate = json!({"content": {"role": "model", "parts": parts}});
    if let Some(finish) = finish {
        candidate["finishReason"] = json!(finish);
    }
    let mut out = json!({"candidates": [candidate]});
    if let Some(usage) = usage {
        out["usageMetadata"] = usage.clone();
    }
    super::data_block(&out.to_string())
}

/// 缓冲 functionCall 参数直到能整块发出的共用状态。
#[derive(Default)]
struct CallBuffer {
    names: std::collections::HashMap<usize, String>,
    args: std::collections::HashMap<usize, String>,
}

impl CallBuffer {
    fn push_args(&mut self, index: usize, args: &str) {
        self.args.entry(index).or_default().push_str(args);
    }
}

/// 上游 OpenAI SSE → Gemini 入站 chunks。tool_calls 参数分片到达，整块
/// 缓冲到 finish 一次性发出（Gemini 的 functionCall 是完整对象）。
pub struct OpenAiToGeminiStream {
    terminated: bool,
    calls: CallBuffer,
    tool_order: Vec<usize>,
    usage: TokenUsage,
    finish: Option<Value>,
}

impl OpenAiToGeminiStream {
    pub fn new() -> Self {
        Self {
            terminated: false,
            calls: CallBuffer::default(),
            tool_order: Vec::new(),
            usage: TokenUsage::default(),
            finish: None,
        }
    }
}

impl Default for OpenAiToGeminiStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for OpenAiToGeminiStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if self.terminated {
            return vec![];
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object())
            && let Some(parsed) = super::extract_openai_usage(&json!({"usage": usage}))
        {
            self.usage = parsed;
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return vec![];
        };
        let delta = choice.get("delta");
        let mut out = Vec::new();
        if let Some(text) = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(gemini_chunk_block(
                json!([{"text": text, "thought": true}]),
                None,
                None,
            ));
        }
        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str)
            && !text.is_empty()
        {
            out.push(gemini_chunk_block(json!([{"text": text}]), None, None));
        }
        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    self.calls.names.insert(index, name.to_string());
                    self.tool_order.push(index);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    self.calls.push_args(index, args);
                }
            }
        }
        if let Some(finish) = choice.get("finish_reason").filter(|f| !f.is_null()) {
            self.finish = Some(finish.clone());
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        if self.terminated {
            return vec![];
        }
        self.terminated = true;
        // functionCall 整块补发：Gemini 无参数分片形态。
        let mut parts = Vec::new();
        for index in std::mem::take(&mut self.tool_order) {
            let name = self.calls.names.get(&index).cloned().unwrap_or_default();
            let args_text = self
                .calls
                .args
                .get(&index)
                .cloned()
                .unwrap_or_else(|| "{}".into());
            let args = serde_json::from_str::<Value>(&args_text).unwrap_or(json!({}));
            parts.push(json!({"functionCall": {"name": name, "args": args}}));
        }
        // 最后一块把 finishReason 与 usageMetadata 一并带上。
        let finish = openai_finish_to_gemini(self.finish.as_ref());
        let usage = gemini_usage_json(&self.usage);
        vec![gemini_chunk_block(
            Value::Array(parts),
            Some(finish),
            Some(&usage),
        )]
    }
}

/// 上游 Anthropic SSE → Gemini 入站 chunks。tool_use 块的参数分片缓冲到
/// content_block_stop 整块发出。
pub struct AnthropicToGeminiStream {
    terminated: bool,
    /// Anthropic 块号 → (工具名, 累积参数)。非工具块不进表。
    tools: std::collections::HashMap<usize, (String, String)>,
    usage: TokenUsage,
    stop_reason: Option<Value>,
}

impl AnthropicToGeminiStream {
    pub fn new() -> Self {
        Self {
            terminated: false,
            tools: std::collections::HashMap::new(),
            usage: TokenUsage::default(),
            stop_reason: None,
        }
    }
}

impl Default for AnthropicToGeminiStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for AnthropicToGeminiStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if self.terminated {
            return vec![];
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "message_start" => {
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
                vec![]
            }
            "content_block_start" => {
                if event.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use")
                {
                    let block = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let name = event
                        .pointer("/content_block/name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.tools.insert(block, (name, String::new()));
                }
                vec![]
            }
            "content_block_delta" => {
                let delta = event.get("delta");
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
                            vec![gemini_chunk_block(json!([{"text": text}]), None, None)]
                        }
                    }
                    Some("thinking_delta") => {
                        let text = delta
                            .and_then(|d| d.get("thinking"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![gemini_chunk_block(
                                json!([{"text": text, "thought": true}]),
                                None,
                                None,
                            )]
                        }
                    }
                    Some("input_json_delta") => {
                        let block =
                            event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let partial = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some((_, buffer)) = self.tools.get_mut(&block) {
                            buffer.push_str(partial);
                        }
                        vec![]
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                let block = event.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if let Some((name, buffer)) = self.tools.remove(&block) {
                    let args = serde_json::from_str::<Value>(&buffer).unwrap_or(json!({}));
                    vec![gemini_chunk_block(
                        json!([{"functionCall": {"name": name, "args": args}}]),
                        None,
                        None,
                    )]
                } else {
                    vec![]
                }
            }
            "message_delta" => {
                if let Some(output) = event
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_i64)
                {
                    self.usage.output_tokens = output;
                }
                self.stop_reason = event.pointer("/delta/stop_reason").cloned();
                vec![]
            }
            "message_stop" => self.finish(),
            _ => vec![],
        }
    }

    fn finish(&mut self) -> Vec<String> {
        if self.terminated {
            return vec![];
        }
        self.terminated = true;
        let finish = anthropic_stop_to_gemini(self.stop_reason.as_ref());
        let usage = gemini_usage_json(&self.usage);
        vec![gemini_chunk_block(json!([]), Some(finish), Some(&usage))]
    }
}

/// 上游 Responses SSE → Gemini 入站 chunks。function_call 参数缓冲到
/// output_item.done 整块发出。
pub struct ResponsesToGeminiStream {
    terminated: bool,
    calls: std::collections::HashMap<u64, (String, String)>,
    usage: TokenUsage,
}

impl ResponsesToGeminiStream {
    pub fn new() -> Self {
        Self {
            terminated: false,
            calls: std::collections::HashMap::new(),
            usage: TokenUsage::default(),
        }
    }
}

impl Default for ResponsesToGeminiStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for ResponsesToGeminiStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if self.terminated {
            return vec![];
        }
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "response.output_text.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    vec![]
                } else {
                    vec![gemini_chunk_block(json!([{"text": text}]), None, None)]
                }
            }
            // 推理 summary 增量 → thought part（Gemini 有原生表达，不丢）。
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    vec![]
                } else {
                    vec![gemini_chunk_block(
                        json!([{"text": text, "thought": true}]),
                        None,
                        None,
                    )]
                }
            }
            "response.function_call_arguments.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let partial = event.get("delta").and_then(Value::as_str).unwrap_or("");
                self.calls
                    .entry(output_index)
                    .or_insert_with(|| (String::new(), String::new()))
                    .1
                    .push_str(partial);
                vec![]
            }
            "response.output_item.added" => {
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let output_index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let name = event
                        .pointer("/item/name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.calls
                        .entry(output_index)
                        .or_insert_with(|| (String::new(), String::new()))
                        .0 = name;
                }
                vec![]
            }
            "response.output_item.done" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if let Some((name, buffer)) = self.calls.remove(&output_index) {
                    let args = serde_json::from_str::<Value>(&buffer).unwrap_or(json!({}));
                    vec![gemini_chunk_block(
                        json!([{"functionCall": {"name": name, "args": args}}]),
                        None,
                        None,
                    )]
                } else {
                    vec![]
                }
            }
            kind if super::responses::is_response_done(kind) => {
                self.terminated = true;
                if let Some(usage) = event.pointer("/response/usage")
                    && let Some(parsed) =
                        super::responses::extract_responses_usage(&json!({"usage": usage}))
                {
                    self.usage = parsed;
                }
                let finish = if kind == "response.incomplete" {
                    "MAX_TOKENS"
                } else {
                    "STOP"
                };
                let usage = gemini_usage_json(&self.usage);
                vec![gemini_chunk_block(json!([]), Some(finish), Some(&usage))]
            }
            _ => vec![],
        }
    }

    fn finish(&mut self) -> Vec<String> {
        if self.terminated {
            return vec![];
        }
        self.terminated = true;
        let usage = gemini_usage_json(&self.usage);
        vec![gemini_chunk_block(json!([]), Some("STOP"), Some(&usage))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::{Protocol, StreamTranslator, UsageScanner, extract_usage_json};

    /// 流式翻译器返回完整 SSE 块；测试里剥出裸载荷方便断言。
    fn payload(line: &str) -> String {
        line.trim_start_matches("data: ").trim_end().to_string()
    }

    #[test]
    fn openai_chat_request_maps_to_gemini() {
        let body = json!({
            "model": "gpt-x",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "fc_1", "type": "function",
                     "function": {"name": "get", "arguments": "{\"q\":1}"}}
                ]},
                {"role": "tool", "tool_call_id": "fc_1", "content": "ok"}
            ],
            "max_tokens": 100,
            "stop": "END",
            "tools": [{"type": "function", "function": {
                "name": "get", "description": "w", "parameters": {"type": "object"}}}],
            "tool_choice": {"type": "function", "function": {"name": "get"}},
            "reasoning_effort": "low"
        });
        let out = openai_chat_to_gemini(body).unwrap();
        assert_eq!(out["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(out["contents"][0]["role"], "user");
        assert_eq!(out["contents"][0]["parts"][0]["text"], "hi");
        // tool_call → functionCall（args 是对象），工具结果 → functionResponse
        // （名字从历史 assistant 块反查）。
        assert_eq!(out["contents"][1]["role"], "model");
        assert_eq!(
            out["contents"][1]["parts"][0]["functionCall"]["name"],
            "get"
        );
        assert_eq!(
            out["contents"][1]["parts"][0]["functionCall"]["args"]["q"],
            1
        );
        assert_eq!(
            out["contents"][2]["parts"][0]["functionResponse"]["name"],
            "get"
        );
        assert_eq!(
            out["contents"][2]["parts"][0]["functionResponse"]["response"]["result"],
            "ok"
        );
        assert_eq!(out["generationConfig"]["maxOutputTokens"], 100);
        assert_eq!(out["generationConfig"]["stopSequences"], json!(["END"]));
        assert_eq!(
            out["generationConfig"]["thinkingConfig"]["thinkingBudget"], 1024,
            "low → 1024"
        );
        assert_eq!(out["tools"][0]["functionDeclarations"][0]["name"], "get");
        assert_eq!(
            out["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"][0],
            "get"
        );
        // model 在路径不在体（proxy 负责），体里不能出现。
        assert!(out.get("model").is_none());
        assert!(out.get("stream").is_none());
    }

    #[test]
    fn gemini_request_maps_to_openai_chat() {
        let body = json!({
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [
                    {"text": "thinking", "thought": true},
                    {"text": "calling"},
                    {"functionCall": {"name": "get", "args": {"q": 1}}}
                ]},
                {"role": "user", "parts": [{"functionResponse": {
                    "name": "get", "response": {"result": "ok"}}}]}
            ],
            "generationConfig": {
                "maxOutputTokens": 100,
                "topP": 0.9,
                "stopSequences": ["END"],
                "thinkingConfig": {"thinkingBudget": 4096},
            },
            "tools": [{"functionDeclarations": [
                {"name": "get", "description": "w", "parameters": {"type": "object"}}]}],
            "toolConfig": {"functionCallingConfig": {"mode": "AUTO"}}
        });
        let out = gemini_to_openai_request(body).unwrap();
        assert_eq!(
            out["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(out["messages"][1]["content"], "hi");
        // thought part → reasoning_content；functionCall → 合成 id 的 tool_calls。
        assert_eq!(out["messages"][2]["reasoning_content"], "thinking");
        assert_eq!(out["messages"][2]["tool_calls"][0]["id"], "call_get");
        assert_eq!(
            out["messages"][2]["tool_calls"][0]["function"]["arguments"],
            r#"{"q":1}"#
        );
        assert_eq!(out["messages"][3]["role"], "tool");
        assert_eq!(out["messages"][3]["tool_call_id"], "call_get");
        assert_eq!(out["max_tokens"], 100);
        assert_eq!(out["stop"], json!(["END"]));
        assert_eq!(out["reasoning_effort"], "medium", "4096 → medium");
        assert_eq!(out["tools"][0]["function"]["name"], "get");
        assert_eq!(out["tool_choice"], "auto");
    }

    #[test]
    fn anthropic_request_maps_to_gemini_and_back() {
        let body = json!({
            "model": "claude-x",
            "system": "be brief",
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "get", "input": {"q": "x"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "ok"}
                ]}
            ],
            "max_tokens": 50,
            "thinking": {"type": "enabled", "budget_tokens": 2048}
        });
        let out = anthropic_to_gemini(body).unwrap();
        assert_eq!(out["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(out["contents"][1]["role"], "model");
        assert_eq!(
            out["contents"][1]["parts"][0]["functionCall"]["args"]["q"],
            "x"
        );
        assert_eq!(
            out["contents"][2]["parts"][0]["functionResponse"]["name"],
            "get"
        );
        // 预算直传（同为 token 数语义）。
        assert_eq!(
            out["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            2048
        );

        let back = gemini_to_anthropic_request(out).unwrap();
        assert_eq!(back["max_tokens"], 50);
        assert_eq!(back["thinking"]["budget_tokens"], 2048);
        assert_eq!(back["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(back["messages"][1]["content"][0]["id"], "call_get");
        assert_eq!(back["messages"][2]["content"][0]["tool_use_id"], "call_get");
    }

    #[test]
    fn responses_request_maps_to_gemini() {
        let body = json!({
            "model": "gpt-x",
            "instructions": "be brief",
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {"type": "function_call", "call_id": "fc_1", "name": "get", "arguments": "{\"q\":1}"},
                {"type": "function_call_output", "call_id": "fc_1", "output": "ok"}
            ],
            "reasoning": {"effort": "high"}
        });
        let out = responses_to_gemini(body).unwrap();
        assert_eq!(out["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(
            out["contents"][1]["parts"][0]["functionCall"]["args"]["q"],
            1
        );
        assert_eq!(
            out["contents"][2]["parts"][0]["functionResponse"]["name"],
            "get"
        );
        assert_eq!(
            out["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            16384
        );
    }

    #[test]
    fn gemini_request_maps_to_responses() {
        let body = json!({
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"functionCall": {"name": "get", "args": {"q": 1}}}]},
                {"role": "user", "parts": [{"functionResponse": {
                    "name": "get", "response": {"result": "ok"}}}]}
            ],
            "generationConfig": {"maxOutputTokens": 80}
        });
        let out = gemini_to_responses_request(body).unwrap();
        assert_eq!(out["instructions"], "be brief");
        assert_eq!(out["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(out["input"][1]["type"], "function_call");
        assert_eq!(out["input"][1]["call_id"], "call_get");
        assert_eq!(out["input"][2]["type"], "function_call_output");
        assert_eq!(out["max_output_tokens"], 80);
    }

    #[test]
    fn gemini_response_maps_to_all_inbound_formats() {
        let response = json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "hmm", "thought": true},
                    {"text": "hello"},
                    {"functionCall": {"name": "get", "args": {"q": 1}}}
                ]},
                "finishReason": "STOP"
            }],
            "modelVersion": "gemini-x",
            "usageMetadata": {"promptTokenCount": 9, "candidatesTokenCount": 4,
                "cachedContentTokenCount": 3, "thoughtsTokenCount": 2}
        });

        let out = gemini_to_openai_chat(response.clone()).unwrap();
        assert_eq!(out["object"], "chat.completion");
        assert_eq!(out["choices"][0]["message"]["content"], "hello");
        assert_eq!(out["choices"][0]["message"]["reasoning_content"], "hmm");
        assert_eq!(
            out["choices"][0]["message"]["tool_calls"][0]["id"],
            "call_get"
        );
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"]["prompt_tokens"], 9);
        assert_eq!(out["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
        assert_eq!(
            out["usage"]["completion_tokens_details"]["reasoning_tokens"],
            2
        );

        let out = gemini_to_anthropic(response.clone()).unwrap();
        assert_eq!(out["content"][0]["type"], "thinking");
        assert_eq!(out["content"][1]["type"], "text");
        assert_eq!(out["content"][2]["type"], "tool_use");
        assert_eq!(out["content"][2]["input"]["q"], 1);
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"]["cache_read_input_tokens"], 3);

        let out = gemini_to_responses(response).unwrap();
        assert_eq!(out["output"][0]["type"], "reasoning");
        assert_eq!(out["output"][1]["type"], "message");
        assert_eq!(out["output"][2]["type"], "function_call");
        assert_eq!(out["output"][2]["call_id"], "call_get");
        assert_eq!(out["usage"]["input_tokens_details"]["cached_tokens"], 3);
    }

    #[test]
    fn inbound_responses_map_to_gemini_shape() {
        let completion = json!({
            "id": "chatcmpl-1", "model": "gpt-x",
            "choices": [{"index": 0, "message": {"role": "assistant",
                "content": "hello", "reasoning_content": "hmm"},
                "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 4}
        });
        let out = openai_completion_to_gemini(completion).unwrap();
        let parts = out["candidates"][0]["content"]["parts"].as_array().unwrap();
        assert_eq!(
            parts[0]["thought"], true,
            "reasoning_content → thought part"
        );
        assert_eq!(parts[1]["text"], "hello");
        assert_eq!(out["candidates"][0]["finishReason"], "STOP");
        assert_eq!(out["usageMetadata"]["promptTokenCount"], 9);

        let response = json!({
            "id": "resp_1", "model": "gpt-x", "status": "completed",
            "output": [
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "call_id": "fc_1", "name": "get",
                 "arguments": "{\"q\":1}"}
            ],
            "usage": {"input_tokens": 7, "output_tokens": 4}
        });
        let out = responses_object_to_gemini(response).unwrap();
        let parts = out["candidates"][0]["content"]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "hello");
        assert_eq!(parts[1]["functionCall"]["args"]["q"], 1);
        assert_eq!(out["usageMetadata"]["candidatesTokenCount"], 4);
    }

    #[test]
    fn gemini_stream_translates_to_openai_chunks() {
        let mut translator = GeminiToOpenAiStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "hmm", "thought": true}]}}]}),
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "hi"}]}}]}),
            json!({
                "candidates": [{
                    "content": {"role": "model", "parts": [
                        {"functionCall": {"name": "get", "args": {"q": 1}}},
                    ]},
                    "finishReason": "STOP",
                }],
                "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 2},
            }),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        assert!(translator.finish().is_empty(), "finishReason 已收尾");

        let parsed: Vec<Value> = lines
            .iter()
            .filter_map(|line| serde_json::from_str(&payload(line)).ok())
            .collect();
        assert_eq!(parsed[0]["choices"][0]["delta"]["reasoning_content"], "hmm");
        assert_eq!(parsed[1]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(parsed[2]["choices"][0]["delta"]["content"], "hi");
        assert_eq!(
            parsed[3]["choices"][0]["delta"]["tool_calls"][0]["id"],
            "call_get"
        );
        assert_eq!(parsed[4]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(parsed[5]["usage"]["prompt_tokens"], 5);
        assert_eq!(lines[6], "data: [DONE]\n\n");
    }

    #[test]
    fn gemini_stream_translates_to_anthropic_events() {
        let mut translator = GeminiToAnthropicStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "hi"}]}}], "modelVersion": "gemini-x"}),
            json!({
                "candidates": [{
                    "content": {"role": "model", "parts": [
                        {"functionCall": {"name": "get", "args": {"q": 1}}},
                    ]},
                    "finishReason": "STOP",
                }],
                "usageMetadata": {"promptTokenCount": 7, "candidatesTokenCount": 2},
            }),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        let parsed: Vec<Value> = lines
            .iter()
            .filter_map(|line| serde_json::from_str(&payload(line)).ok())
            .collect();
        let kinds: Vec<&str> = parsed
            .iter()
            .map(|e| e["type"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            kinds,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop", // 文本块在 tool 块开块前关闭
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(parsed[0]["message"]["model"], "gemini-x");
        assert_eq!(parsed[4]["content_block"]["name"], "get");
        assert_eq!(parsed[7]["usage"]["input_tokens"], 7);
    }

    #[test]
    fn openai_stream_buffers_tool_calls_into_gemini_parts() {
        let mut translator = OpenAiToGeminiStream::new();
        let mut lines = Vec::new();
        for chunk in [
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"content": "hi"}, "finish_reason": null}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"tool_calls": [{"index": 0, "id": "fc_1",
                    "function": {"name": "get", "arguments": ""}}]}}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"tool_calls": [{"index": 0,
                    "function": {"arguments": "{\"q\":1}"}}]}}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0, "delta": {},
                "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 5, "completion_tokens": 2}}),
            Value::String("[DONE]".into()),
        ] {
            let payload = if chunk.is_string() {
                "[DONE]".to_string()
            } else {
                chunk.to_string()
            };
            lines.extend(translator.feed(&payload));
        }
        // 文本 chunk 即时透出；functionCall 缓冲到 finish 整块发出。
        assert_eq!(lines.len(), 2);
        let final_chunk: Value = serde_json::from_str(&payload(&lines[1])).unwrap();
        let parts = final_chunk["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        assert_eq!(parts[0]["functionCall"]["name"], "get");
        assert_eq!(parts[0]["functionCall"]["args"]["q"], 1);
        assert_eq!(final_chunk["candidates"][0]["finishReason"], "STOP");
        assert_eq!(final_chunk["usageMetadata"]["promptTokenCount"], 5);
    }

    #[test]
    fn anthropic_stream_maps_to_gemini_chunks() {
        let mut translator = AnthropicToGeminiStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "message_start", "message": {"model": "claude-x",
                "usage": {"input_tokens": 7}}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "hmm"}}),
            json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "tool_use", "id": "t1", "name": "get", "input": {}}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "input_json_delta", "partial_json": "{\"q\":1}"}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 2}}),
            json!({"type": "message_stop"}),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        assert_eq!(lines.len(), 3, "thought + functionCall + 收尾块");
        let thought: Value = serde_json::from_str(&payload(&lines[0])).unwrap();
        assert_eq!(
            thought["candidates"][0]["content"]["parts"][0]["thought"],
            true
        );
        let call: Value = serde_json::from_str(&payload(&lines[1])).unwrap();
        assert_eq!(
            call["candidates"][0]["content"]["parts"][0]["functionCall"]["name"],
            "get"
        );
        let finish: Value = serde_json::from_str(&payload(&lines[2])).unwrap();
        assert_eq!(finish["candidates"][0]["finishReason"], "STOP");
        assert_eq!(finish["usageMetadata"]["candidatesTokenCount"], 2);
    }

    #[test]
    fn usage_scanner_reads_gemini_metadata() {
        let mut scanner = UsageScanner::new(Protocol::Gemini);
        scanner.feed(
            &json!({"candidates": [{"content": {"role": "model", "parts": [{"text": "hi"}]}}]})
                .to_string(),
        );
        scanner.feed(
            &json!({"candidates": [{"content": {"parts": []}, "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 6, "candidatesTokenCount": 3,
                    "cachedContentTokenCount": 2, "thoughtsTokenCount": 1}})
            .to_string(),
        );
        let usage = scanner.take().unwrap();
        assert_eq!(usage.input_tokens, 6);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 2);
        assert_eq!(usage.reasoning_tokens, Some(1));

        let usage = extract_usage_json(
            Protocol::Gemini,
            &json!({"usageMetadata": {"promptTokenCount": 4, "candidatesTokenCount": 1}}),
        )
        .unwrap();
        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 1);
    }

    #[test]
    fn http_image_urls_are_rejected_for_gemini_upstream() {
        let body = json!({
            "model": "gpt-x",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": "https://example.com/a.png"}},
                ],
            }],
        });
        let err = openai_chat_to_gemini(body).unwrap_err();
        assert!(err.to_string().contains("base64"), "http URL 明确报错");
    }
}
