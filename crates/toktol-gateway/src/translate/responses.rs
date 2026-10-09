//! OpenAI Responses API（`POST /v1/responses`）双向翻译：入站方向（本地应用说
//! Responses，上游说 OpenAI Chat / Anthropic）与上游方向（上游说 Responses，
//! 入站说 OpenAI Chat / Anthropic）各一套请求、非流式响应、流式转换。
//!
//! 对齐官方格式中 SDK 依赖的骨架：请求的 input items（message / function_call /
//! function_call_output）、flat tools、instructions、reasoning.effort；响应的
//! output 数组（message / function_call / reasoning）与 usage；流式的
//! created → output_item.added → *_delta → output_item.done → completed 序列。
//! 已知取舍：推理内容在 Responses 格式里是 reasoning item 的 summary 结构——
//! 入站方向（上游 thinking / reasoning_content → Responses）做有损丢弃；上游
//! 方向（Responses reasoning → thinking / reasoning_content）尽力回放 summary
//! 文本，summary 缺失时客户端少看到一段推理。

use serde_json::{Value, json};
use toktol_core::error::{Error, Result};
use toktol_core::model::TokenUsage;

use super::{budget_to_effort, effort_to_budget};

fn bad(message: impl Into<String>) -> Error {
    Error::Internal(format!("translate/responses: {}", message.into()))
}

/// 把 input（字符串或 items 数组）统一成 items 数组。
fn input_items(body: &Value) -> Vec<Value> {
    match body.get("input") {
        Some(Value::String(text)) => {
            vec![json!({"type": "message", "role": "user", "content": text})]
        }
        Some(Value::Array(items)) => items.clone(),
        _ => vec![],
    }
}

/// message item 的 content（字符串或 parts）拼成纯文本。
fn item_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                // input_text / output_text / text：三种命名并存，取任意 text 字段。
                part.get("text").and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Responses message content → openai chat content：纯文本回 string（与主流
/// SDK 兼容），含 input_image 时回 parts 数组（image_url part）。
fn message_content_to_openai(content: &Value) -> Value {
    match content {
        Value::String(text) => json!(text),
        Value::Array(parts) => {
            let mut text = String::new();
            let mut images: Vec<Value> = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_image") => {
                        if let Some(url) = part.get("image_url").and_then(Value::as_str) {
                            images.push(json!({"type": "image_url", "image_url": {"url": url}}));
                        }
                    }
                    _ => text.push_str(part.get("text").and_then(Value::as_str).unwrap_or("")),
                }
            }
            if images.is_empty() {
                json!(text)
            } else {
                let mut arr = Vec::new();
                if !text.is_empty() {
                    arr.push(json!({"type": "text", "text": text}));
                }
                arr.extend(images);
                Value::Array(arr)
            }
        }
        _ => json!(""),
    }
}

/// Responses message content → anthropic blocks：input_image 转 base64 image
/// 块；http URL 无对等表达，明确报错而不是静默丢图。
fn message_content_to_anthropic(content: &Value) -> Result<Vec<Value>> {
    match content {
        Value::String(text) => Ok(vec![json!({"type": "text", "text": text})]),
        Value::Array(parts) => {
            let mut blocks = Vec::new();
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_image") => {
                        let url = part.get("image_url").and_then(Value::as_str).unwrap_or("");
                        blocks.push(super::image_url_to_anthropic_block(url)?);
                    }
                    _ => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                    }
                }
            }
            if blocks.is_empty() {
                blocks.push(json!({"type": "text", "text": ""}));
            }
            Ok(blocks)
        }
        _ => Ok(vec![json!({"type": "text", "text": ""})]),
    }
}

// ── 请求：Responses → 上游 ──────────────────────────────────

/// 转成 OpenAI Chat Completions 请求体。
pub fn request_to_openai_chat(body: Value) -> Result<Value> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("请求缺少 model"))?
        .to_string();
    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str)
        && !instructions.is_empty()
    {
        messages.push(json!({"role": "system", "content": instructions}));
    }
    for item in input_items(&body) {
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
                let role = match role {
                    "developer" | "system" => "system",
                    other => other,
                };
                messages.push(json!({"role": role, "content": message_content_to_openai(item.get("content").unwrap_or(&Value::Null))}));
            }
            "function_call" => messages.push(json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": item.get("call_id"),
                    "type": "function",
                    "function": {"name": item.get("name"), "arguments": item.get("arguments").cloned().unwrap_or(json!("{}"))},
                }],
            })),
            "function_call_output" => messages.push(json!({
                "role": "tool",
                "tool_call_id": item.get("call_id"),
                "content": item.get("output").cloned().unwrap_or(json!("")),
            })),
            // reasoning / item_reference 等无法回放的 item 类型跳过。
            _ => {}
        }
    }

    let mut out = json!({"model": model, "messages": messages});
    for (from, to) in [
        ("max_output_tokens", "max_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stream", "stream"),
    ] {
        if let Some(v) = body.get(from) {
            out[to] = v.clone();
        }
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        // flat 格式（type/name/description/parameters）→ 包一层 function。
        out["tools"] = Value::Array(
            tools
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
    if let Some(choice) = body.get("tool_choice") {
        match choice {
            Value::String(s) => out["tool_choice"] = json!(s),
            Value::Object(_) => {
                let name = choice.get("name").cloned().unwrap_or_default();
                out["tool_choice"] = json!({"type": "function", "function": {"name": name}});
            }
            _ => {}
        }
    }
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(Value::as_str) {
        out["reasoning_effort"] = json!(effort);
    }
    Ok(out)
}

/// 转成 Anthropic Messages 请求体。
pub fn request_to_anthropic(body: Value) -> Result<Value> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("请求缺少 model"))?
        .to_string();
    let mut system = String::new();
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        system = instructions.to_string();
    }
    let mut messages: Vec<Value> = Vec::new();
    for item in input_items(&body) {
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
                let text = item_text(item.get("content").unwrap_or(&Value::Null));
                match role {
                    "system" | "developer" => {
                        if !system.is_empty() {
                            system.push_str("\n\n");
                        }
                        system.push_str(&text);
                    }
                    "assistant" => messages.push(
                        json!({"role": "assistant", "content": [{"type": "text", "text": text}]}),
                    ),
                    _ => messages.push(json!({
                        "role": "user",
                        "content": message_content_to_anthropic(item.get("content").unwrap_or(&Value::Null))?,
                    })),
                }
            }
            "function_call" => messages.push(json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": item.get("call_id"),
                    "name": item.get("name"),
                    "input": item
                        .get("arguments")
                        .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                        .unwrap_or(json!({})),
                }],
            })),
            "function_call_output" => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": item.get("call_id"),
                    "content": item.get("output").cloned().unwrap_or(json!("")),
                }],
            })),
            _ => {}
        }
    }

    let mut out = json!({
        "model": model,
        "messages": messages,
        // Anthropic 必填；Responses 侧缺省时给一个保守上限。
        "max_tokens": body.get("max_output_tokens").cloned().unwrap_or(json!(4096)),
    });
    if !system.is_empty() {
        out["system"] = json!(system);
    }
    for field in ["temperature", "top_p", "stream"] {
        if let Some(v) = body.get(field) {
            out[field] = v.clone();
        }
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        out["tools"] = Value::Array(
            tools
                .iter()
                .filter_map(|tool| {
                    Some(json!({
                        "name": tool.get("name")?,
                        "description": tool.get("description").cloned().unwrap_or(json!("")),
                        "input_schema": tool.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                    }))
                })
                .collect(),
        );
    }
    match body.get("tool_choice") {
        Some(Value::String(s)) if s == "auto" || s == "none" => {
            out["tool_choice"] = json!({"type": s})
        }
        Some(Value::String(s)) if s == "required" => out["tool_choice"] = json!({"type": "any"}),
        Some(choice) if choice.get("name").is_some() => {
            out["tool_choice"] = json!({"type": "tool", "name": choice.get("name")});
        }
        _ => {}
    }
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(Value::as_str)
        && let Some(budget) = effort_to_budget(effort)
    {
        out["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
        let max_tokens = out["max_tokens"].as_i64().unwrap_or(0);
        if max_tokens <= budget {
            out["max_tokens"] = json!(budget + 1024);
        }
    }
    Ok(out)
}

// ── 非流式响应：上游 → Responses ────────────────────────────

fn response_envelope(id: String, model: String, output: Vec<Value>, usage: Value) -> Value {
    json!({
        "id": id,
        "object": "response",
        "created_at": super::unix_now(),
        "status": "completed",
        "model": model,
        "output": output,
        "usage": usage,
    })
}

/// OpenAI Chat completion → Responses response 对象。
pub fn openai_chat_to_response(completion: Value) -> Result<Value> {
    let model = completion
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let choice = completion
        .pointer("/choices/0")
        .ok_or_else(|| bad("上游响应缺少 choices[0]"))?;
    let mut output: Vec<Value> = Vec::new();
    if let Some(text) = choice.pointer("/message/content").and_then(Value::as_str)
        && !text.is_empty()
    {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{}", super::unix_now()),
            "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        }));
    }
    if let Some(calls) = choice
        .pointer("/message/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            output.push(json!({
                "type": "function_call",
                "id": call.get("id"),
                "call_id": call.get("id"),
                "name": call.pointer("/function/name"),
                "arguments": call.pointer("/function/arguments").cloned().unwrap_or(json!("{}")),
            }));
        }
    }
    let usage = super::extract_openai_usage(&completion).unwrap_or_default();
    Ok(response_envelope(
        completion
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("resp_toktol")
            .to_string(),
        model,
        output,
        json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.input_tokens + usage.output_tokens,
            "input_tokens_details": {"cached_tokens": usage.cache_read_tokens},
            "output_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
        }),
    ))
}

/// Anthropic message → Responses response 对象。
pub fn anthropic_message_to_response(message: Value) -> Result<Value> {
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut output: Vec<Value> = Vec::new();
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                    if !text.is_empty() {
                        output.push(json!({
                            "type": "message",
                            "id": format!("msg_{}", super::unix_now()),
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}],
                        }));
                    }
                }
                // thinking 块丢弃（见模块文档取舍）。
                Some("tool_use") => output.push(json!({
                    "type": "function_call",
                    "id": block.get("id"),
                    "call_id": block.get("id"),
                    "name": block.get("name"),
                    "arguments": block.get("input").map(|input| input.to_string()).unwrap_or_else(|| "{}".into()),
                })),
                _ => {}
            }
        }
    }
    let usage = super::extract_anthropic_usage(&message).unwrap_or_default();
    Ok(response_envelope(
        message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("resp_toktol")
            .to_string(),
        model,
        output,
        json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.input_tokens + usage.output_tokens,
            "input_tokens_details": {"cached_tokens": usage.cache_read_tokens},
            "output_tokens_details": {"reasoning_tokens": usage.reasoning_tokens},
        }),
    ))
}

// ── 流式：上游 SSE → Responses 事件 ─────────────────────────

/// 一条 Responses SSE 事件（含 `event:` 行）。
fn event(name: &str, payload: Value) -> String {
    format!("event: {name}\ndata: {}\n\n", payload)
}

/// 起手事件：response.created（status=in_progress）。
fn created_event(model: &str) -> String {
    event(
        "response.created",
        json!({
            "type": "response.created",
            "response": {
                "id": format!("resp_toktol_{}", super::unix_now()),
                "object": "response",
                "created_at": super::unix_now(),
                "status": "in_progress",
                "model": model,
                "output": [],
                "usage": Value::Null,
            },
            "sequence_number": 0,
        }),
    )
}

/// OpenAI Chat SSE → Responses 事件流。
pub struct OpenAiChatToResponsesStream {
    model: String,
    created: bool,
    /// 文本项：开块后累积正文，finish 时补 output_text.done + output_item.done。
    text_open: bool,
    text: String,
    /// 上游 tool_calls index → (output_index, call_id, name, 累积参数)。
    tools: std::collections::HashMap<usize, (usize, String, String, String)>,
    next_output: usize,
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    reasoning_tokens: Option<i64>,
}

impl OpenAiChatToResponsesStream {
    pub fn new() -> Self {
        Self {
            model: String::new(),
            created: false,
            text_open: false,
            text: String::new(),
            tools: std::collections::HashMap::new(),
            next_output: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            reasoning_tokens: None,
        }
    }

    fn ensure_created(&mut self, out: &mut Vec<String>) {
        if !self.created {
            self.created = true;
            out.push(created_event(&self.model));
        }
    }

    fn usage_json(&self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "total_tokens": self.input_tokens + self.output_tokens,
            "input_tokens_details": {"cached_tokens": self.cache_read},
            "output_tokens_details": {"reasoning_tokens": self.reasoning_tokens},
        })
    }
}

impl Default for OpenAiChatToResponsesStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for OpenAiChatToResponsesStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        if let Some(model) = chunk.get("model").and_then(Value::as_str)
            && self.model.is_empty()
        {
            self.model = model.to_string();
        }
        if let Some(usage) = chunk.get("usage").filter(|u| u.is_object())
            && let Some(parsed) = super::extract_openai_usage(&json!({"usage": usage}))
        {
            self.input_tokens = parsed.input_tokens;
            self.output_tokens = parsed.output_tokens;
            self.cache_read = parsed.cache_read_tokens;
            self.reasoning_tokens = parsed.reasoning_tokens;
        }
        let Some(choice) = chunk.pointer("/choices/0") else {
            return vec![];
        };
        let delta = choice.get("delta");

        let mut out = Vec::new();
        self.ensure_created(&mut out);

        if let Some(text) = delta.and_then(|d| d.get("content")).and_then(Value::as_str)
            && !text.is_empty()
        {
            if !self.text_open {
                self.text_open = true;
                out.push(event(
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
            out.push(event(
                "response.output_text.delta",
                json!({
                    "type": "response.output_text.delta",
                    "item_id": format!("msg_{}", super::unix_now()),
                    "output_index": self.next_output - 1,
                    "content_index": 0,
                    "delta": text,
                }),
            ));
        }

        if let Some(calls) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for call in calls {
                let upstream_index =
                    call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if !self.tools.contains_key(&upstream_index)
                    && (call.get("id").is_some() || call.pointer("/function/name").is_some())
                {
                    let output_index = self.next_output;
                    self.next_output += 1;
                    let call_id = call
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    out.push(event(
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "output_index": output_index,
                            "item": {"type": "function_call", "id": call_id, "call_id": call_id, "name": name, "arguments": ""},
                        }),
                    ));
                    self.tools
                        .insert(upstream_index, (output_index, call_id, name, String::new()));
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str)
                    && !args.is_empty()
                    && let Some((output_index, _, _, buffer)) = self.tools.get_mut(&upstream_index)
                {
                    buffer.push_str(args);
                    out.push(event(
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "output_index": output_index,
                            "delta": args,
                        }),
                    ));
                }
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        if !self.created {
            return vec![];
        }
        let mut out = Vec::new();
        if self.text_open {
            out.push(event(
                "response.output_text.done",
                json!({
                    "type": "response.output_text.done",
                    "output_index": 0,
                    "content_index": 0,
                    "text": self.text,
                }),
            ));
            out.push(event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": self.text}]},
                }),
            ));
        }
        for (output_index, call_id, name, arguments) in self.tools.values() {
            out.push(event(
                "response.function_call_arguments.done",
                json!({
                    "type": "response.function_call_arguments.done",
                    "output_index": output_index,
                    "arguments": arguments,
                }),
            ));
            out.push(event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": {"type": "function_call", "id": call_id, "call_id": call_id, "name": name, "arguments": arguments},
                }),
            ));
        }
        out.push(event(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": {
                    "id": format!("resp_toktol_{}", super::unix_now()),
                    "object": "response",
                    "status": "completed",
                    "model": self.model,
                    "usage": self.usage_json(),
                },
            }),
        ));
        out
    }
}

/// Anthropic SSE → Responses 事件流。
pub struct AnthropicToResponsesStream {
    model: String,
    created: bool,
    /// Anthropic 块号 → (output_index, 类型, call_id, name, 累积文本/参数)。
    blocks: std::collections::HashMap<usize, BlockState>,
    next_output: usize,
    input_tokens: i64,
    output_tokens: i64,
    cache_read: i64,
    cache_write: i64,
}

struct BlockState {
    output_index: usize,
    kind: String,
    call_id: String,
    name: String,
    text: String,
    arguments: String,
    done: bool,
}

impl AnthropicToResponsesStream {
    pub fn new() -> Self {
        Self {
            model: String::new(),
            created: false,
            blocks: std::collections::HashMap::new(),
            next_output: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read: 0,
            cache_write: 0,
        }
    }

    fn usage_json(&self) -> Value {
        json!({
            "input_tokens": self.input_tokens,
            "output_tokens": self.output_tokens,
            "total_tokens": self.input_tokens + self.output_tokens,
            "input_tokens_details": {"cached_tokens": self.cache_read},
            "output_tokens_details": {"reasoning_tokens": Value::Null},
        })
    }

    /// `response.completed` 载荷；feed 的 message_stop 与断流兜底 finish 共用一份。
    fn completed_json(&self) -> Value {
        json!({
            "type": "response.completed",
            "response": {
                "id": format!("resp_toktol_{}", super::unix_now()),
                "object": "response",
                "status": "completed",
                "model": self.model,
                "usage": self.usage_json(),
            },
        })
    }

    /// 起手：登记模型与输入侧用量后补发 created；重复 message_start 幂等忽略。
    fn feed_message_start(&mut self, event_value: &Value, out: &mut Vec<String>) {
        if self.created {
            return;
        }
        self.created = true;
        if let Some(model) = event_value
            .pointer("/message/model")
            .and_then(Value::as_str)
        {
            self.model = model.to_string();
        }
        if let Some(usage) = event_value.pointer("/message/usage") {
            self.input_tokens = usage
                .get("input_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.cache_read = usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            self.cache_write = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(0);
        }
        out.push(created_event(&self.model));
    }

    /// 开块：文本/工具调用各建一个 output item；thinking 块不产出事件也不占 output 位
    /// （模块文档取舍），但仍要在 `blocks` 里登记以吃掉后续 delta。
    fn feed_block_start(&mut self, event_value: &Value, out: &mut Vec<String>) {
        let block = event_value
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let block_type = event_value
            .pointer("/content_block/type")
            .and_then(Value::as_str)
            .unwrap_or("text");
        let output_index = self.next_output;
        self.next_output += 1;
        let (response_type, item) = match block_type {
            "tool_use" => (
                "function_call",
                json!({
                    "type": "function_call",
                    "id": event_value.pointer("/content_block/id"),
                    "call_id": event_value.pointer("/content_block/id"),
                    "name": event_value.pointer("/content_block/name"),
                    "arguments": "",
                }),
            ),
            _ => (
                "message",
                json!({
                    "type": "message",
                    "id": format!("msg_{}", super::unix_now()),
                    "role": "assistant",
                    "content": [],
                }),
            ),
        };
        self.blocks.insert(
            block,
            BlockState {
                output_index,
                kind: response_type.to_string(),
                call_id: item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                text: String::new(),
                arguments: String::new(),
                done: false,
            },
        );
        if response_type == "message" || block_type == "tool_use" {
            out.push(event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": item,
                }),
            ));
        }
    }

    /// 块内增量：文本走 output_text.delta，工具参数走 function_call_arguments.delta；
    /// thinking_delta / signature_delta 丢弃。
    fn feed_block_delta(&mut self, event_value: &Value, out: &mut Vec<String>) {
        let block = event_value
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let delta_type = event_value
            .pointer("/delta/type")
            .and_then(Value::as_str)
            .unwrap_or("");
        match delta_type {
            "text_delta" => {
                let text = event_value
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some(state) = self.blocks.get_mut(&block) {
                    state.text.push_str(text);
                    out.push(event(
                        "response.output_text.delta",
                        json!({
                            "type": "response.output_text.delta",
                            "output_index": state.output_index,
                            "content_index": 0,
                            "delta": text,
                        }),
                    ));
                }
            }
            "input_json_delta" => {
                let partial = event_value
                    .pointer("/delta/partial_json")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Some(state) = self.blocks.get_mut(&block) {
                    state.arguments.push_str(partial);
                    out.push(event(
                        "response.function_call_arguments.delta",
                        json!({
                            "type": "response.function_call_arguments.delta",
                            "output_index": state.output_index,
                            "delta": partial,
                        }),
                    ));
                }
            }
            _ => {}
        }
    }

    /// 收块：每个块只发一次 output_item.done（`done` 旗标幂等）。
    fn feed_block_stop(&mut self, event_value: &Value, out: &mut Vec<String>) {
        let block = event_value
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if let Some(state) = self.blocks.get_mut(&block)
            && !state.done
        {
            state.done = true;
            let item = if state.kind == "function_call" {
                json!({
                    "type": "function_call",
                    "id": state.call_id,
                    "call_id": state.call_id,
                    "name": state.name,
                    "arguments": state.arguments,
                })
            } else {
                json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": state.text}],
                })
            };
            out.push(event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": state.output_index,
                    "item": item,
                }),
            ));
        }
    }

    /// 收尾用量：只更新输出 token（输入侧在 message_start 已定）。
    fn feed_message_delta(&mut self, event_value: &Value) {
        if let Some(usage) = event_value.get("usage") {
            self.output_tokens = usage
                .get("output_tokens")
                .and_then(Value::as_i64)
                .unwrap_or(self.output_tokens);
        }
    }
}

impl Default for AnthropicToResponsesStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for AnthropicToResponsesStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        let Ok(event_value) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        let kind = event_value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let mut out = Vec::new();
        match kind {
            "message_start" => self.feed_message_start(&event_value, &mut out),
            "content_block_start" => self.feed_block_start(&event_value, &mut out),
            "content_block_delta" => self.feed_block_delta(&event_value, &mut out),
            "content_block_stop" => self.feed_block_stop(&event_value, &mut out),
            "message_delta" => self.feed_message_delta(&event_value),
            "message_stop" => out.push(event("response.completed", self.completed_json())),
            _ => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        // 上游断流未发 message_stop 的兜底：补 completed。
        if self.created {
            self.created = false;
            vec![event("response.completed", self.completed_json())]
        } else {
            vec![]
        }
    }
}

// ── 请求：入站 → Responses 上游 ─────────────────────────────

/// OpenAI Chat Completions 请求 → Responses 请求体。
pub fn openai_chat_to_request(body: Value) -> Result<Value> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("请求缺少 model"))?
        .to_string();
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 messages"))?;
    for message in raw_messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("message 缺少 role"))?
            .to_string();
        match role.as_str() {
            "system" | "developer" => {
                if let Some(text) = message_text(&message)
                    && !text.is_empty()
                {
                    instructions.push(text);
                }
            }
            "user" => input.push(message_item(&message, "user")),
            "assistant" => {
                let text = message_text(&message).unwrap_or_default();
                if !text.is_empty() {
                    input.push(message_item(&message, "assistant"));
                }
                if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.get("id"),
                            "name": call.pointer("/function/name"),
                            "arguments": call
                                .pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .unwrap_or("{}"),
                        }));
                    }
                }
            }
            "tool" => input.push(json!({
                "type": "function_call_output",
                "call_id": message.get("tool_call_id"),
                "output": message.get("content").cloned().unwrap_or(json!("")),
            })),
            other => return Err(bad(format!("message role 不支持: {other}"))),
        }
    }

    let mut out = json!({"model": model, "input": input});
    if !instructions.is_empty() {
        out["instructions"] = json!(instructions.join("\n\n"));
    }
    for (from, to) in [
        ("max_tokens", "max_output_tokens"),
        ("max_completion_tokens", "max_output_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
        ("stream", "stream"),
    ] {
        if let Some(v) = body.get(from) {
            out[to] = v.clone();
        }
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        // 包了一层 function → flat 格式。
        out["tools"] = Value::Array(
            tools
                .iter()
                .filter_map(|tool| {
                    let f = tool.get("function")?;
                    Some(json!({
                        "type": "function",
                        "name": f.get("name")?,
                        "description": f.get("description").cloned().unwrap_or(json!("")),
                        "parameters": f.get("parameters").cloned().unwrap_or(json!({"type": "object"})),
                    }))
                })
                .collect(),
        );
    }
    if let Some(choice) = body
        .get("tool_choice")
        .filter(|c| c.is_string() || c.is_object())
    {
        if let Some(name) = choice.pointer("/function/name") {
            out["tool_choice"] = json!({"type": "function", "name": name});
        } else {
            out["tool_choice"] = choice.clone();
        }
    }
    if let Some(effort) = body.get("reasoning_effort").and_then(Value::as_str) {
        out["reasoning"] = json!({"effort": effort});
    }
    Ok(out)
}

/// Anthropic Messages 请求 → Responses 请求体。
pub fn anthropic_to_request(body: Value) -> Result<Value> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("请求缺少 model"))?
        .to_string();
    let mut instructions = String::new();
    if let Some(system) = body.get("system")
        && let Some(text) = message_text(&json!({"content": system}))
    {
        instructions = text;
    }
    let mut input: Vec<Value> = Vec::new();
    let raw_messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| bad("请求缺少 messages"))?;
    for message in raw_messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("message 缺少 role"))?;
        let content = message.get("content").cloned().unwrap_or(json!(""));
        let blocks = content.as_array().cloned().unwrap_or_else(|| {
            vec![json!({"type": "text", "text": content.as_str().unwrap_or_default()})]
        });
        match role {
            "user" => {
                // text / image / tool_result 可混排在同一条 user 消息里：
                // 前两者合成一个 message item，tool_result 独立成 output item。
                let mut parts: Vec<Value> = Vec::new();
                for block in &blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.is_empty() {
                                parts.push(json!({"type": "input_text", "text": text}));
                            }
                        }
                        Some("image") => {
                            // base64 source 回包成 data URL；非 base64（web URL
                            // beta）无对等表达，跳过。
                            if let Some(part) = super::anthropic_image_to_openai_part(block) {
                                let url = part
                                    .pointer("/image_url/url")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                parts.push(json!({"type": "input_image", "image_url": url}));
                            }
                        }
                        Some("tool_result") => input.push(json!({
                            "type": "function_call_output",
                            "call_id": block.get("tool_use_id"),
                            "output": block.get("content").cloned().unwrap_or(json!("")),
                        })),
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    input.push(json!({"type": "message", "role": "user", "content": parts}));
                }
            }
            "assistant" => {
                let mut text = String::new();
                for block in &blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            text.push_str(block.get("text").and_then(Value::as_str).unwrap_or(""));
                        }
                        Some("tool_use") => input.push(json!({
                            "type": "function_call",
                            "call_id": block.get("id"),
                            "name": block.get("name"),
                            "arguments": block
                                .get("input")
                                .map(|input| input.to_string())
                                .unwrap_or_else(|| "{}".into()),
                        })),
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": text}],
                    }));
                }
            }
            other => return Err(bad(format!("message role 不支持: {other}"))),
        }
    }

    let mut out = json!({"model": model, "input": input});
    if !instructions.is_empty() {
        out["instructions"] = json!(instructions);
    }
    for field in ["temperature", "top_p", "stream"] {
        if let Some(v) = body.get(field) {
            out[field] = v.clone();
        }
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        out["max_output_tokens"] = max_tokens.clone();
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        out["tools"] = Value::Array(
            tools
                .iter()
                .filter_map(|tool| {
                    Some(json!({
                        "type": "function",
                        "name": tool.get("name")?,
                        "description": tool.get("description").cloned().unwrap_or(json!("")),
                        "parameters": tool.get("input_schema").cloned().unwrap_or(json!({"type": "object"})),
                    }))
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
            out["tool_choice"] = json!({"type": "function", "name": name});
        }
        _ => {}
    }
    // 推理配置：thinking 预算 → effort 档位（与 anthropic→openai 同一张表）。
    if let Some(budget) = body
        .pointer("/thinking/budget_tokens")
        .and_then(Value::as_i64)
        && body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled")
    {
        out["reasoning"] = json!({"effort": budget_to_effort(budget)});
    }
    Ok(out)
}

/// 把一条 openai chat message 包成 Responses 的 message item；content parts
/// 的类型按角色取 input_text / output_text。image_url part 转成 input_image
/// （URL 原样，data URL 与 http URL 都是 Responses 的合法形态）。
fn message_item(message: &Value, role: &str) -> Value {
    let part_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let text = message_text(message).unwrap_or_default();
    let images: Vec<Value> = message
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
                .filter_map(|p| {
                    p.pointer("/image_url/url")
                        .and_then(Value::as_str)
                        .map(|url| json!({"type": "input_image", "image_url": url}))
                })
                .collect()
        })
        .unwrap_or_default();
    let content = if images.is_empty() {
        json!([{"type": part_type, "text": text}])
    } else {
        let mut arr = Vec::new();
        if !text.is_empty() {
            arr.push(json!({"type": part_type, "text": text}));
        }
        arr.extend(images);
        Value::Array(arr)
    };
    json!({
        "type": "message",
        "role": role,
        "content": content,
    })
}

/// message content（string 或多段）拼成纯文本；复用入站方向的取 text 字段逻辑。
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

// ── 非流式响应：Responses 上游 → 入站 ───────────────────────

/// Responses 响应体的 usage 抽取（非流式与 response.completed 事件共用）。
pub(super) fn extract_responses_usage(response: &Value) -> Option<TokenUsage> {
    let usage = response.get("usage")?;
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
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cache_write_tokens: 0,
        reasoning_tokens: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_i64),
    })
}

/// output 数组里的信息一次取齐：正文、推理、工具调用。
struct OutputParts {
    text: String,
    reasoning: String,
    tool_calls: Vec<Value>,
}

fn split_output(response: &Value) -> OutputParts {
    let mut parts = OutputParts {
        text: String::new(),
        reasoning: String::new(),
        tool_calls: Vec::new(),
    };
    if let Some(items) = response.get("output").and_then(Value::as_array) {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("message") => {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            // output_text / text：两种命名并存。
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                parts.text.push_str(text);
                            }
                        }
                    }
                }
                // reasoning summary 回放给不挑格式的推理通道；缺失时客户端只是
                // 少看到一段推理（见模块文档取舍）。
                Some("reasoning") => {
                    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                        for part in summary {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                parts.reasoning.push_str(text);
                            }
                        }
                    }
                }
                Some("function_call") => parts.tool_calls.push(item.clone()),
                _ => {}
            }
        }
    }
    parts
}

/// 推理 item 存在即视为"上游产生了推理"——回放进推理通道，没有就丢弃。
fn reasoning_item_json(parts: &OutputParts) -> Option<String> {
    if parts.reasoning.is_empty() {
        None
    } else {
        Some(parts.reasoning.clone())
    }
}

fn responses_stop_to_openai(response: &Value, has_tools: bool) -> &'static str {
    if has_tools {
        return "tool_calls";
    }
    match (
        response.get("status").and_then(Value::as_str),
        response.get("incomplete_reason").and_then(Value::as_str),
    ) {
        (_, Some("max_output_tokens")) => "length",
        (Some("incomplete"), _) => "length",
        _ => "stop",
    }
}

fn responses_stop_to_anthropic(response: &Value, has_tools: bool) -> &'static str {
    if has_tools {
        return "tool_use";
    }
    match (
        response.get("status").and_then(Value::as_str),
        response.get("incomplete_reason").and_then(Value::as_str),
    ) {
        (_, Some("max_output_tokens")) => "max_tokens",
        (Some("incomplete"), _) => "max_tokens",
        _ => "end_turn",
    }
}

/// Responses response 对象 → OpenAI Chat completion。
pub fn response_to_openai_chat(response: Value) -> Result<Value> {
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parts = split_output(&response);
    let mut message_out = json!({"role": "assistant", "content": parts.text});
    if let Some(reasoning) = reasoning_item_json(&parts) {
        message_out["reasoning_content"] = json!(reasoning);
    }
    if !parts.tool_calls.is_empty() {
        message_out["tool_calls"] = Value::Array(
            parts
                .tool_calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    json!({
                        "index": index,
                        "id": call.get("call_id"),
                        "type": "function",
                        "function": {
                            "name": call.get("name"),
                            "arguments": call.get("arguments").cloned().unwrap_or(json!("{}")),
                        },
                    })
                })
                .collect(),
        );
    }
    let usage = extract_responses_usage(&response).unwrap_or_default();
    Ok(json!({
        "id": response
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("resp_toktol")
            .to_string(),
        "object": "chat.completion",
        "created": super::unix_now(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message_out,
            "finish_reason": responses_stop_to_openai(&response, !parts.tool_calls.is_empty()),
        }],
        "usage": super::openai_usage_json(&usage),
    }))
}

/// Responses response 对象 → Anthropic message。
pub fn response_to_anthropic(response: Value) -> Result<Value> {
    let model = response
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let parts = split_output(&response);
    let mut blocks: Vec<Value> = Vec::new();
    if let Some(reasoning) = reasoning_item_json(&parts) {
        blocks.push(json!({"type": "thinking", "thinking": reasoning}));
    }
    if !parts.text.is_empty() {
        blocks.push(json!({"type": "text", "text": parts.text}));
    }
    for call in &parts.tool_calls {
        blocks.push(json!({
            "type": "tool_use",
            "id": call.get("call_id"),
            "name": call.get("name"),
            "input": call
                .get("arguments")
                .and_then(|args| serde_json::from_str::<Value>(args.as_str()?).ok())
                .unwrap_or(json!({})),
        }));
    }
    let usage = extract_responses_usage(&response).unwrap_or_default();
    Ok(json!({
        "id": response
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("resp_toktol")
            .to_string(),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": blocks,
        "stop_reason": responses_stop_to_anthropic(&response, !parts.tool_calls.is_empty()),
        "usage": super::anthropic_usage_json(&usage),
    }))
}

// ── 流式：Responses 上游 SSE → 入站 ─────────────────────────

/// Responses 上游流的收尾事件名：completed / incomplete / failed 都带 usage。
pub(super) fn is_response_done(kind: &str) -> bool {
    matches!(
        kind,
        "response.completed" | "response.incomplete" | "response.failed"
    )
}

/// Responses SSE → 客户端 OpenAI chunks。
pub struct ResponsesToOpenAiStream {
    id: String,
    model: String,
    started: bool,
    /// 上游 output_index → OpenAI tool_calls index（function_call item 顺序分配）。
    tool_indexes: std::collections::HashMap<u64, usize>,
    next_tool: usize,
    seen_tool: bool,
    /// completed/incomplete/failed 已收：不再产出事件。
    terminated: bool,
    usage: TokenUsage,
}

impl ResponsesToOpenAiStream {
    pub fn new() -> Self {
        Self {
            id: format!("chatcmpl-toktol-{}", super::unix_now()),
            model: String::new(),
            started: false,
            tool_indexes: std::collections::HashMap::new(),
            next_tool: 0,
            seen_tool: false,
            terminated: false,
            usage: TokenUsage::default(),
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> String {
        super::data_block(
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": super::unix_now(),
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

impl Default for ResponsesToOpenAiStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for ResponsesToOpenAiStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if self.terminated {
            return vec![];
        }
        match kind {
            "response.created" | "response.in_progress" => {
                let mut out = Vec::new();
                if let Some(model) = event.pointer("/response/model").and_then(Value::as_str)
                    && self.model.is_empty()
                {
                    self.model = model.to_string();
                }
                if !self.started {
                    self.started = true;
                    out.push(self.chunk(json!({"role": "assistant", "content": ""}), None));
                }
                out
            }
            "response.output_item.added" => {
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let output_index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let tool = self.next_tool;
                    self.next_tool += 1;
                    self.tool_indexes.insert(output_index, tool);
                    self.seen_tool = true;
                    vec![self.chunk(
                        json!({"tool_calls": [{
                            "index": tool,
                            "id": event.pointer("/item/call_id"),
                            "type": "function",
                            "function": {
                                "name": event.pointer("/item/name"),
                                "arguments": "",
                            },
                        }]}),
                        None,
                    )]
                } else {
                    vec![]
                }
            }
            "response.output_text.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    vec![]
                } else {
                    vec![self.chunk(json!({"content": text}), None)]
                }
            }
            // 推理通道：Responses reasoning summary 增量 → reasoning_content。
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    vec![]
                } else {
                    vec![self.chunk(json!({"reasoning_content": text}), None)]
                }
            }
            "response.function_call_arguments.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let Some(tool) = self.tool_indexes.get(&output_index).copied() else {
                    return vec![];
                };
                let partial = event.get("delta").and_then(Value::as_str).unwrap_or("");
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
            kind if is_response_done(kind) => {
                self.terminated = true;
                if let Some(usage) = event.pointer("/response/usage")
                    && let Some(parsed) = extract_responses_usage(&json!({"usage": usage}))
                {
                    self.usage = parsed;
                }
                let finish = match kind {
                    "response.incomplete" => "length",
                    _ if self.seen_tool => "tool_calls",
                    _ => "stop",
                };
                vec![
                    self.chunk(json!({}), Some(finish)),
                    // usage 走独立末包（choices 为空），与 OpenAI 原生行为一致。
                    self.chunk0_usage(),
                    super::data_block("[DONE]"),
                ]
            }
            _ => vec![],
        }
    }

    fn finish(&mut self) -> Vec<String> {
        // 正常收尾在 completed 事件处已完成；这里兜底上游断流未发 completed。
        if self.started && !self.terminated {
            self.terminated = true;
            let finish = if self.seen_tool { "tool_calls" } else { "stop" };
            vec![
                self.chunk(json!({}), Some(finish)),
                self.chunk0_usage(),
                super::data_block("[DONE]"),
            ]
        } else {
            vec![]
        }
    }
}

impl ResponsesToOpenAiStream {
    fn chunk0_usage(&self) -> String {
        let usage = json!({
            "prompt_tokens": self.usage.input_tokens,
            "completion_tokens": self.usage.output_tokens,
            "total_tokens": self.usage.input_tokens + self.usage.output_tokens,
            "prompt_tokens_details": {"cached_tokens": self.usage.cache_read_tokens},
            "completion_tokens_details": {"reasoning_tokens": self.usage.reasoning_tokens},
        });
        super::data_block(
            &json!({
                "id": self.id,
                "object": "chat.completion.chunk",
                "created": super::unix_now(),
                "model": self.model,
                "choices": [],
                "usage": usage,
            })
            .to_string(),
        )
    }
}

/// Responses SSE → 客户端 Anthropic 事件。
pub struct ResponsesToAnthropicStream {
    model: String,
    message_started: bool,
    terminated: bool,
    /// 下一个待开 block 的索引（Anthropic 块号按 start 顺序递增）。
    next_block: usize,
    /// 上游 output_index → Anthropic 块号。function_call 在 added 时开块；
    /// text/thinking 块懒开（首个 delta 才占块号），两类共用一张表。
    output_to_block: std::collections::HashMap<u64, usize>,
    /// 已开未关的块号；收尾统一补 stop。
    open_blocks: Vec<usize>,
    /// 懒开的 text / thinking 块（Anthropic 块不重叠，各至多一个开着）。
    open_text_block: Option<(u64, usize)>,
    open_thinking_block: Option<(u64, usize)>,
    seen_tool: bool,
    input_usage: TokenUsage,
    output_usage: TokenUsage,
}

impl ResponsesToAnthropicStream {
    pub fn new() -> Self {
        Self {
            model: String::new(),
            message_started: false,
            terminated: false,
            next_block: 0,
            output_to_block: std::collections::HashMap::new(),
            open_blocks: Vec::new(),
            open_text_block: None,
            open_thinking_block: None,
            seen_tool: false,
            input_usage: TokenUsage::default(),
            output_usage: TokenUsage::default(),
        }
    }

    fn event(&self, value: Value) -> String {
        super::data_block(&value.to_string())
    }

    fn start_message(&mut self) -> String {
        self.message_started = true;
        json!({
            "type": "message_start",
            "message": {
                "id": format!("msg-toktol-{}", super::unix_now()),
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": Value::Null,
                // usage 在 Responses 里到 completed 才出现，先置 0，message_delta 补。
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        })
        .to_string()
    }

    /// 懒开 text / thinking 块：同 output_index 复用已开的同类块；Anthropic 的
    /// 块不重叠，开新块前先关掉还开着的另一块。返回块号。
    fn lazy_start(
        &mut self,
        out: &mut Vec<String>,
        output_index: u64,
        kind: &'static str,
    ) -> usize {
        // Option<(u64, usize)> 是 Copy：先快照两个槽位再操作，避免持借用调方法。
        let is_thinking = kind == "thinking";
        let (open_same, open_other) = if is_thinking {
            (self.open_thinking_block, self.open_text_block)
        } else {
            (self.open_text_block, self.open_thinking_block)
        };
        if let Some((_, index)) = open_other {
            if is_thinking {
                self.open_text_block = None;
            } else {
                self.open_thinking_block = None;
            }
            self.stop_event(out, index);
        }
        if let Some((open_index, index)) = open_same {
            if open_index == output_index {
                return index;
            }
            if is_thinking {
                self.open_thinking_block = None;
            } else {
                self.open_text_block = None;
            }
            self.stop_event(out, index);
        }
        let index = self.next_block;
        self.next_block += 1;
        self.open_blocks.push(index);
        let block = if is_thinking {
            json!({"type": "thinking", "thinking": ""})
        } else {
            json!({"type": "text", "text": ""})
        };
        out.push(self.event(json!({
            "type": "content_block_start",
            "index": index,
            "content_block": block,
        })));
        if is_thinking {
            self.open_thinking_block = Some((output_index, index));
        } else {
            self.open_text_block = Some((output_index, index));
        }
        index
    }

    /// 发 content_block_stop 并从开块集合摘除。
    fn stop_event(&mut self, out: &mut Vec<String>, index: usize) {
        if let Some(pos) = self.open_blocks.iter().position(|b| *b == index) {
            self.open_blocks.remove(pos);
        }
        out.push(self.event(json!({
            "type": "content_block_stop",
            "index": index,
        })));
    }

    fn finish_stream(&mut self, stop_reason: &'static str) -> Vec<String> {
        let mut out = Vec::new();
        for index in std::mem::take(&mut self.open_blocks) {
            out.push(self.event(json!({
                "type": "content_block_stop",
                "index": index,
            })));
        }
        out.push(self.event(json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": stop_reason,
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

impl Default for ResponsesToAnthropicStream {
    fn default() -> Self {
        Self::new()
    }
}

impl super::StreamTranslator for ResponsesToAnthropicStream {
    fn feed(&mut self, payload: &str) -> Vec<String> {
        if payload.trim() == "[DONE]" {
            return self.finish();
        }
        let Ok(event) = serde_json::from_str::<Value>(payload) else {
            return vec![];
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        if self.terminated {
            return vec![];
        }
        match kind {
            "response.created" | "response.in_progress" => {
                let mut out = Vec::new();
                if let Some(model) = event.pointer("/response/model").and_then(Value::as_str)
                    && self.model.is_empty()
                {
                    self.model = model.to_string();
                }
                if !self.message_started {
                    out.push(self.start_message());
                }
                out
            }
            "response.output_item.added" => {
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") {
                    let output_index = event
                        .get("output_index")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let index = self.next_block;
                    self.next_block += 1;
                    self.output_to_block.insert(output_index, index);
                    self.open_blocks.push(index);
                    self.seen_tool = true;
                    vec![self.event(json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": event.pointer("/item/call_id"),
                            "name": event.pointer("/item/name"),
                            "input": {},
                        },
                    }))]
                } else {
                    vec![]
                }
            }
            "response.output_text.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    return vec![];
                }
                let mut out = Vec::new();
                let index = self.lazy_start(&mut out, output_index, "text");
                out.push(self.event(json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "text_delta", "text": text},
                })));
                out
            }
            // 推理通道：Responses reasoning summary 增量 → thinking 块。
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let text = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if text.is_empty() {
                    return vec![];
                }
                let mut out = Vec::new();
                let index = self.lazy_start(&mut out, output_index, "thinking");
                out.push(self.event(json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "thinking_delta", "thinking": text},
                })));
                out
            }
            "response.function_call_arguments.delta" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let Some(index) = self.output_to_block.get(&output_index).copied() else {
                    return vec![];
                };
                let partial = event.get("delta").and_then(Value::as_str).unwrap_or("");
                if partial.is_empty() {
                    return vec![];
                }
                vec![self.event(json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": {"type": "input_json_delta", "partial_json": partial},
                }))]
            }
            "response.output_item.done" => {
                let output_index = event
                    .get("output_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let mut out = Vec::new();
                if let Some(index) = self.output_to_block.get(&output_index).copied() {
                    self.stop_event(&mut out, index);
                }
                let mut lazy_closed: Vec<usize> = Vec::new();
                for holder in [&mut self.open_text_block, &mut self.open_thinking_block] {
                    if holder.is_some_and(|(oidx, _)| oidx == output_index)
                        && let Some((_, index)) = holder.take()
                    {
                        lazy_closed.push(index);
                    }
                }
                for index in lazy_closed {
                    self.stop_event(&mut out, index);
                }
                out
            }
            kind if is_response_done(kind) => {
                self.terminated = true;
                if let Some(usage) = event.pointer("/response/usage")
                    && let Some(parsed) = extract_responses_usage(&json!({"usage": usage}))
                {
                    self.input_usage.input_tokens = parsed.input_tokens;
                    self.input_usage.cache_read_tokens = parsed.cache_read_tokens;
                    self.output_usage.output_tokens = parsed.output_tokens;
                    self.output_usage.reasoning_tokens = parsed.reasoning_tokens;
                }
                let stop_reason = match kind {
                    "response.incomplete" => "max_tokens",
                    _ if self.seen_tool => "tool_use",
                    _ => "end_turn",
                };
                self.finish_stream(stop_reason)
            }
            _ => vec![],
        }
    }

    fn finish(&mut self) -> Vec<String> {
        // 上游断流未发 completed 的兜底。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Protocol;
    use crate::translate::{StreamTranslator, UsageScanner, extract_usage_json};

    /// 流式翻译器返回完整 SSE 块；测试里剥出裸载荷方便断言。
    fn payload(line: &str) -> String {
        line.trim_start_matches("data: ").trim_end().to_string()
    }

    fn responses_request() -> Value {
        json!({
            "model": "gpt-x",
            "instructions": "be brief",
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {"type": "function_call", "call_id": "fc_1", "name": "get", "arguments": "{\"q\":1}"},
                {"type": "function_call_output", "call_id": "fc_1", "output": "ok"}
            ],
            "max_output_tokens": 100,
            "tools": [{"type": "function", "name": "get", "description": "w", "parameters": {"type": "object"}}],
            "tool_choice": "auto",
            "reasoning": {"effort": "low"}
        })
    }

    #[test]
    fn request_maps_to_openai_chat() {
        let out = request_to_openai_chat(responses_request()).unwrap();
        assert_eq!(
            out["messages"][0],
            json!({"role": "system", "content": "be brief"})
        );
        assert_eq!(out["messages"][1]["content"], "hi");
        assert_eq!(out["messages"][2]["tool_calls"][0]["id"], "fc_1");
        assert_eq!(out["messages"][3]["role"], "tool");
        assert_eq!(out["max_tokens"], 100);
        assert_eq!(out["tools"][0]["function"]["name"], "get");
        assert_eq!(out["tool_choice"], "auto");
        assert_eq!(out["reasoning_effort"], "low");
    }

    #[test]
    fn request_maps_to_anthropic() {
        let out = request_to_anthropic(responses_request()).unwrap();
        assert_eq!(out["system"], "be brief");
        assert_eq!(out["messages"][0]["role"], "user");
        assert_eq!(out["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(out["messages"][1]["content"][0]["id"], "fc_1");
        assert_eq!(out["messages"][2]["content"][0]["type"], "tool_result");
        // thinking 预算 1024 >= max_tokens 100：按约定抬升为 budget + 1024。
        assert_eq!(out["max_tokens"], 2048);
        assert_eq!(out["tools"][0]["name"], "get");
        assert_eq!(out["thinking"]["budget_tokens"], 1024, "low → 1024");
    }

    #[test]
    fn string_input_is_treated_as_user_message() {
        let out = request_to_openai_chat(json!({
            "model": "gpt-x", "input": "hi", "max_output_tokens": 10
        }))
        .unwrap();
        assert_eq!(out["messages"][0]["content"], "hi");
    }

    #[test]
    fn openai_completion_maps_to_response_object() {
        let completion = json!({
            "id": "chatcmpl-1", "model": "gpt-x",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "hello"},
                "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 2,
                "prompt_tokens_details": {"cached_tokens": 3}}
        });
        let out = openai_chat_to_response(completion).unwrap();
        assert_eq!(out["object"], "response");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["output"][0]["type"], "message");
        assert_eq!(out["output"][0]["content"][0]["text"], "hello");
        assert_eq!(out["usage"]["input_tokens"], 9);
        assert_eq!(out["usage"]["input_tokens_details"]["cached_tokens"], 3);
    }

    #[test]
    fn anthropic_message_maps_to_response_object() {
        let message = json!({
            "id": "msg_1", "model": "claude-x", "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "hello"},
                {"type": "tool_use", "id": "t1", "name": "get", "input": {"q": 1}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 7, "output_tokens": 4}
        });
        let out = anthropic_message_to_response(message).unwrap();
        assert_eq!(out["output"].as_array().unwrap().len(), 2, "thinking 丢弃");
        assert_eq!(out["output"][0]["content"][0]["text"], "hello");
        assert_eq!(out["output"][1]["type"], "function_call");
        assert_eq!(out["output"][1]["call_id"], "t1");
        assert_eq!(out["output"][1]["arguments"], r#"{"q":1}"#);
    }

    #[test]
    fn openai_stream_becomes_responses_events() {
        let mut translator = OpenAiChatToResponsesStream::new();
        let mut events = Vec::new();
        for chunk in [
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"role": "assistant", "content": "he"}, "finish_reason": null}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"tool_calls": [{"index": 0, "id": "fc_1", "type": "function",
                    "function": {"name": "get", "arguments": ""}}]}}]}),
            json!({"model": "gpt-x", "choices": [{"index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{\"q\":1}"}}]}}]}),
            json!({"model": "gpt-x", "choices": [], "usage": {
                "prompt_tokens": 5, "completion_tokens": 2}}),
            Value::String("[DONE]".into()),
        ] {
            let payload = if chunk.is_string() {
                "[DONE]".to_string()
            } else {
                chunk.to_string()
            };
            events.extend(translator.feed(&payload));
        }

        let names: Vec<String> = events
            .iter()
            .filter_map(|block: &String| {
                block
                    .split('\n')
                    .next()
                    .and_then(|line| line.strip_prefix("event: "))
            })
            .map(str::to_string)
            .collect();
        assert_eq!(
            names,
            vec![
                "response.created",
                "response.output_item.added", // message
                "response.output_text.delta",
                "response.output_item.added", // function_call
                "response.function_call_arguments.delta",
                "response.output_text.done",
                "response.output_item.done", // message
                "response.function_call_arguments.done",
                "response.output_item.done", // function_call
                "response.completed",
            ]
        );
        let completed = events.last().unwrap();
        let data = completed
            .split('\n')
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        let parsed: Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed["response"]["status"], "completed");
        assert_eq!(parsed["response"]["usage"]["input_tokens"], 5);
    }

    #[test]
    fn anthropic_stream_becomes_responses_events() {
        let mut translator = AnthropicToResponsesStream::new();
        let mut events = Vec::new();
        for event_value in [
            json!({"type": "message_start", "message": {"model": "claude-x",
                "usage": {"input_tokens": 7}}}),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "hi"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "usage": {"output_tokens": 2}}),
            json!({"type": "message_stop"}),
        ] {
            events.extend(translator.feed(&event_value.to_string()));
        }

        let names: Vec<String> = events
            .iter()
            .filter_map(|block: &String| {
                block
                    .split('\n')
                    .next()
                    .and_then(|line| line.strip_prefix("event: "))
            })
            .map(str::to_string)
            .collect();
        assert_eq!(
            names,
            vec![
                "response.created",
                "response.output_item.added",
                "response.output_text.delta",
                "response.output_item.done",
                "response.completed",
            ]
        );
    }

    // ── 上游方向（入站 → Responses 上游）────────────────────

    #[test]
    fn openai_chat_request_maps_to_responses() {
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
            "tools": [{"type": "function", "function": {
                "name": "get", "description": "w", "parameters": {"type": "object"}}}],
            "tool_choice": {"type": "function", "function": {"name": "get"}},
            "reasoning_effort": "low"
        });
        let out = openai_chat_to_request(body).unwrap();
        assert_eq!(out["instructions"], "be brief");
        assert_eq!(out["input"][0]["role"], "user");
        assert_eq!(out["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(out["input"][1]["type"], "function_call");
        assert_eq!(out["input"][1]["call_id"], "fc_1");
        assert_eq!(out["input"][2]["type"], "function_call_output");
        assert_eq!(out["max_output_tokens"], 100);
        assert_eq!(out["tools"][0]["name"], "get", "tools 摊平");
        assert_eq!(out["tool_choice"]["name"], "get");
        assert_eq!(out["reasoning"]["effort"], "low");
    }

    #[test]
    fn anthropic_request_maps_to_responses() {
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
            "tools": [{"name": "get", "description": "w", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "any"},
            "thinking": {"type": "enabled", "budget_tokens": 4096}
        });
        let out = anthropic_to_request(body).unwrap();
        assert_eq!(out["instructions"], "be brief");
        assert_eq!(out["input"][0]["content"][0]["text"], "hi");
        assert_eq!(out["input"][1]["type"], "function_call");
        assert_eq!(out["input"][2]["type"], "function_call_output");
        assert_eq!(out["max_output_tokens"], 50);
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tool_choice"], "required");
        assert_eq!(out["reasoning"]["effort"], "medium", "4096 → medium");
    }

    #[test]
    fn responses_object_maps_to_openai_completion() {
        let response = json!({
            "id": "resp_1", "model": "gpt-x", "status": "completed",
            "output": [
                {"type": "reasoning", "summary": [{"type": "summary_text", "text": "hmm"}]},
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "call_id": "fc_1", "name": "get",
                 "arguments": "{\"q\":1}"}
            ],
            "usage": {"input_tokens": 9, "output_tokens": 4,
                "input_tokens_details": {"cached_tokens": 3},
                "output_tokens_details": {"reasoning_tokens": 2}}
        });
        let out = response_to_openai_chat(response).unwrap();
        assert_eq!(out["object"], "chat.completion");
        assert_eq!(out["choices"][0]["message"]["content"], "hello");
        assert_eq!(
            out["choices"][0]["message"]["reasoning_content"], "hmm",
            "reasoning summary 回放进推理通道"
        );
        assert_eq!(out["choices"][0]["message"]["tool_calls"][0]["id"], "fc_1");
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"]["prompt_tokens"], 9);
        assert_eq!(out["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
        assert_eq!(
            out["usage"]["completion_tokens_details"]["reasoning_tokens"],
            2
        );

        // incomplete 语义映射。
        let out = response_to_openai_chat(json!({
            "model": "gpt-x", "status": "incomplete",
            "incomplete_reason": "max_output_tokens", "output": [], "usage": {}
        }))
        .unwrap();
        assert_eq!(out["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn responses_object_maps_to_anthropic_message() {
        let response = json!({
            "id": "resp_1", "model": "gpt-x", "status": "completed",
            "output": [
                {"type": "message", "role": "assistant",
                 "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "call_id": "fc_1", "name": "get",
                 "arguments": "{\"q\":1}"}
            ],
            "usage": {"input_tokens": 7, "output_tokens": 4,
                "input_tokens_details": {"cached_tokens": 2}}
        });
        let out = response_to_anthropic(response).unwrap();
        assert_eq!(out["type"], "message");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "hello");
        assert_eq!(out["content"][1]["type"], "tool_use");
        assert_eq!(out["content"][1]["id"], "fc_1");
        assert_eq!(out["content"][1]["input"]["q"], 1);
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"]["cache_read_input_tokens"], 2);
    }

    #[test]
    fn responses_stream_translates_to_openai_chunks() {
        let mut translator = ResponsesToOpenAiStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "response.created", "response": {"model": "gpt-x"}}),
            json!({"type": "response.output_item.added", "output_index": 0,
                "item": {"type": "message", "role": "assistant", "content": []}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "he"}),
            json!({"type": "response.output_item.added", "output_index": 1,
                "item": {"type": "function_call", "call_id": "fc_1", "name": "get", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta",
                "output_index": 1, "delta": "{\"q\":1}"}),
            json!({"type": "response.completed", "response": {
                "model": "gpt-x",
                "usage": {"input_tokens": 5, "output_tokens": 2,
                    "input_tokens_details": {"cached_tokens": 1}}}}),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        assert!(translator.finish().is_empty(), "completed 已收尾");

        let parsed: Vec<Value> = lines
            .iter()
            .filter_map(|line| serde_json::from_str(&payload(line)).ok())
            .collect();
        let first = &parsed[0];
        assert_eq!(first["object"], "chat.completion.chunk");
        assert_eq!(first["choices"][0]["delta"]["role"], "assistant");
        let text = &parsed[1];
        assert_eq!(text["choices"][0]["delta"]["content"], "he");
        let tool = &parsed[2];
        assert_eq!(tool["choices"][0]["delta"]["tool_calls"][0]["id"], "fc_1");
        let args = &parsed[3];
        assert_eq!(
            args["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"q\":1}"
        );
        let finish = &parsed[4];
        assert_eq!(finish["choices"][0]["finish_reason"], "tool_calls");
        let usage = &parsed[5];
        assert_eq!(
            usage["choices"].as_array().unwrap().len(),
            0,
            "usage 走空 choices 末包"
        );
        assert_eq!(usage["usage"]["prompt_tokens"], 5);
        assert_eq!(usage["usage"]["prompt_tokens_details"]["cached_tokens"], 1);
        assert_eq!(lines[6], "data: [DONE]\n\n");
    }

    #[test]
    fn responses_stream_translates_to_anthropic_events() {
        let mut translator = ResponsesToAnthropicStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "response.created", "response": {"model": "gpt-x"}}),
            json!({"type": "response.output_text.delta", "output_index": 0, "delta": "hi"}),
            json!({"type": "response.output_item.done", "output_index": 0,
                "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.completed", "response": {
                "model": "gpt-x",
                "usage": {"input_tokens": 7, "output_tokens": 2}}}),
        ] {
            lines.extend(translator.feed(&event.to_string()));
        }
        assert!(translator.finish().is_empty());

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
                "content_block_start", // text 懒开
                "content_block_delta",
                "content_block_stop", // output_item.done 关块
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(parsed[2]["delta"]["text"], "hi");
        assert_eq!(parsed[4]["usage"]["input_tokens"], 7);
        assert_eq!(parsed[4]["usage"]["output_tokens"], 2);
    }

    #[test]
    fn images_flow_through_responses_in_both_directions() {
        const PNG: &str = "iVBORw0KGgo=";
        let data_url = format!("data:image/png;base64,{PNG}");

        // 入站 openai → Responses 上游：image_url part 原样成 input_image。
        let out = openai_chat_to_request(json!({
            "model": "gpt-x",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": data_url}},
            ]}],
        }))
        .unwrap();
        assert_eq!(out["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(out["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(out["input"][0]["content"][1]["image_url"], data_url);

        // 入站 anthropic → Responses 上游：base64 块回包成 data URL。
        let out = anthropic_to_request(json!({
            "model": "claude-x",
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image", "source": {"type": "base64",
                    "media_type": "image/png", "data": PNG}},
            ]}],
        }))
        .unwrap();
        assert_eq!(out["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(out["input"][0]["content"][1]["image_url"], data_url);

        // Responses 入站 → openai 上游：input_image 原样成 image_url part。
        let out = request_to_openai_chat(json!({
            "model": "gpt-x",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": data_url},
            ]}],
        }))
        .unwrap();
        assert_eq!(out["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(
            out["messages"][0]["content"][1]["image_url"]["url"],
            data_url
        );

        // Responses 入站 → anthropic 上游：data URL 拆回 base64 块；http URL 报错。
        let out = request_to_anthropic(json!({
            "model": "claude-x",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "look"},
                {"type": "input_image", "image_url": data_url},
            ]}],
        }))
        .unwrap();
        assert_eq!(
            out["messages"][0]["content"][1],
            json!({"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": PNG}}),
        );
        let http = request_to_anthropic(json!({
            "model": "claude-x",
            "input": [{"type": "message", "role": "user", "content": [
                {"type": "input_image", "image_url": "https://example.com/a.png"},
            ]}],
        }))
        .unwrap_err();
        assert!(http.to_string().contains("base64"), "http URL 明确报错");
    }

    #[test]
    fn responses_reasoning_streams_to_anthropic_thinking() {
        let mut translator = ResponsesToAnthropicStream::new();
        let mut lines = Vec::new();
        for event in [
            json!({"type": "response.created", "response": {"model": "gpt-x"}}),
            // reasoning item 的 summary 增量：懒开 thinking 块（output_index 0）。
            json!({"type": "response.reasoning_summary_text.delta",
                "output_index": 0, "delta": "hmm"}),
            // 正文换了个 output_index：thinking 块让位给新开的 text 块。
            json!({"type": "response.output_text.delta", "output_index": 1, "delta": "ok"}),
            json!({"type": "response.output_item.done", "output_index": 1,
                "item": {"type": "message", "role": "assistant"}}),
            json!({"type": "response.completed", "response": {"model": "gpt-x", "usage": {}}}),
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
                "content_block_start", // thinking
                "content_block_delta",
                "content_block_stop",  // thinking 在 text 开块前关闭
                "content_block_start", // text
                "content_block_delta",
                "content_block_stop", // text 在 output_item.done 关闭
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(parsed[1]["content_block"]["type"], "thinking");
        assert_eq!(parsed[2]["delta"]["thinking"], "hmm");
        assert_eq!(parsed[4]["content_block"]["type"], "text");
    }

    #[test]
    fn usage_scanner_reads_responses_completion_events() {
        let mut scanner = UsageScanner::new(Protocol::Responses);
        scanner
            .feed(&json!({"type": "response.created", "response": {"model": "gpt-x"}}).to_string());
        scanner.feed(
            &json!({"type": "response.completed", "response": {
                "usage": {"input_tokens": 6, "output_tokens": 3,
                    "input_tokens_details": {"cached_tokens": 2},
                    "output_tokens_details": {"reasoning_tokens": 1}}}})
            .to_string(),
        );
        let usage = scanner.take().unwrap();
        assert_eq!(usage.input_tokens, 6);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cache_read_tokens, 2);
        assert_eq!(usage.reasoning_tokens, Some(1));

        // 非流式响应体同路径。
        let usage = extract_usage_json(
            Protocol::Responses,
            &json!({"usage": {"input_tokens": 4, "output_tokens": 1}}),
        )
        .unwrap();
        assert_eq!(usage.input_tokens, 4);
        assert_eq!(usage.output_tokens, 1);
    }
}
