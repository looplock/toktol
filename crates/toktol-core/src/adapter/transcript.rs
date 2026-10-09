//! 会话转录的统一模型与跨适配器的提取辅助：会话详情视图把一次会话的
//! 用户/助手消息、工具调用按日志原顺序展示。内容只在用户点开时按需现读、
//! 绝不落库——与扫描管线"只入账用量事实"的口径互补。
//!
//! 各工具日志的内容块形状不一（text / tool_use / tool_result / thinking…，
//! 字段名也各自为政），[`blocks_from_content`] 做归一：认识的块映射成结构化
//! 类型，不认识的整块塞进 [`TranscriptBlock::Raw`] 原样透传给前端——内容
//! 绝不静默丢弃。

use std::collections::VecDeque;
use std::path::Path;

use serde::Serialize;

use crate::error::{Error, Result};

/// 转录里的一条消息。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptEntry {
    /// 发送方角色。
    pub role: TranscriptRole,
    /// 消息时间（epoch ms，UTC）；日志未携带则 `None`。
    pub ts_ms: Option<i64>,
    /// 产出该消息的模型（assistant 消息）；日志未携带则 `None`。
    pub model: Option<String>,
    /// 消息内容块，按日志原顺序。
    pub blocks: Vec<TranscriptBlock>,
}

/// 消息角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptRole {
    /// 用户输入。
    User,
    /// 助手回复。
    Assistant,
    /// 系统提示。
    System,
    /// 工具侧消息（工具结果通常作为用户侧内容块出现，此角色少见）。
    Tool,
}

/// 消息内容块。各工具的字段名差异在提取时归一，未识别的形状原样透传。
/// 注意：枚举容器上的 `rename_all` 只作用于变体名——变体**字段**的
/// camelCase 必须靠 `rename_all_fields`，否则线上是 snake_case
/// （`data_url` / `is_error` / `call_id`），前端读 camelCase 全是 undefined。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TranscriptBlock {
    /// 普通文本。
    Text {
        /// 文本内容。
        text: String,
    },
    /// 推理/思考内容。
    Thinking {
        /// 思考内容。
        text: String,
    },
    /// 工具调用（请求）。
    ToolCall {
        /// 调用 id；日志未携带则 `None`。
        id: Option<String>,
        /// 工具名。
        name: Option<String>,
        /// 参数原文（JSON 文本）；日志未携带则 `None`。
        arguments: Option<String>,
    },
    /// 工具结果。
    ToolResult {
        /// 对应的调用 id；日志未携带则 `None`。
        call_id: Option<String>,
        /// 结果内容（日志里可能是字符串或块数组，已摊平成文本）。
        content: String,
        /// 工具是否报错。
        is_error: bool,
    },
    /// 图片附件引用：WorkBuddy 粘贴图片的两种记法（`image_blob_ref` 块与
    /// `<image_local_path>` 文本）、zcode 的 `image` 块与 `[Image: source: …]`
    /// 引用文本，归一到这里。只带元数据，图片内容不读不传。
    /// opencode 的 `file` part 例外：图片以 data URL 内嵌在库里，走 `data_url`
    /// 原样传给前端渲染（转录现读现显，不入库）。
    Image {
        /// 图片文件的本地路径（data URL 型为空串）。
        path: String,
        /// 展示用文件名（附件原始名，引用块缺失时取路径尾段）。
        filename: String,
        /// 文件体积（字节）；日志未携带则 `None`。
        size: Option<i64>,
        /// 现读时文件是否存在（附件可能已被清理）。
        exists: bool,
        /// 内嵌 data URL（opencode）；本地路径型为 `None`。
        data_url: Option<String>,
    },
    /// 未识别的块：整块 JSON 原样透传，前端折叠展示。
    Raw {
        /// 原始 JSON 文本。
        json: String,
    },
    /// harness 注入上下文所在消息的**完整原文**（`<system-reminder>` 段、
    /// `<user_query>` 标签与提问内容一字不差）。与正文分开建模：前端默认
    /// 只渲染正文，完整口径渲染这段原文（提问内容就在标签内，不拆两截）；
    /// 目录不由此开启轮次。
    Injected {
        /// 整条消息的原始文本。
        text: String,
        /// 其中注入段（reminder 块等）的字符数：占比条"注入占比"的分子，
        /// 分母是它加上正文字符数。
        injected_chars: i64,
    },
    /// 后台任务完成通知（`<task-notification>` 包裹，user 角色、系统生成）。
    /// 字段从原文解析（实体已反转义便于阅读）；`text` 保留整条原文，含
    /// 尾部那段固定模板的后续指令。
    TaskNotification {
        /// 后台任务 id。
        task_id: String,
        /// 任务状态（completed 等，原文原样）。
        status: String,
        /// 一行摘要（实体已反转义）。
        summary: String,
        /// 整条原文（含尾部固定模板指令）。
        text: String,
    },
    /// 上下文压缩摘要（`<conversation_history_summary>` 包裹，user 角色、
    /// 系统生成）。`text` 为标签内摘要原文（不含包裹标签）。
    HistorySummary {
        /// 标签内摘要原文。
        text: String,
        /// 紧随的固定模板"继续对话"指令是否已并入本条（并入后不再单独
        /// 成条目）。
        has_continue: bool,
    },
    /// 压缩后的固定模板"继续对话"指令（"Please continue with the
    /// conversation…"，无标签壳）。组装期并入前一条摘要；未并上时单独
    /// 成条目，前端渲染为弱化说明条。
    ContinueNotice,
    /// 用户中断标记（codex 的 `<turn_aborted>` 包裹消息，user 角色）：记录
    /// "上一轮被用户主动打断"这一事件。原文是恒定的英文模板、没有随事件
    /// 变化的信息，前端渲染成红色分隔线徽章，不锚定轮次、不进目录。
    TurnAborted,
    /// 工具注入的会话上下文（codex 的 `<environment_context>` /
    /// `<skills_instructions>` / `<permissions instructions>` /
    /// `<user_instructions>` 等包裹消息，zcode 的 `<system-reminder>`
    /// 独立消息）：工具塞给模型的指令，不是用户说的话。整条原文保留，
    /// 前端折叠成一行说明条；不锚定轮次、不进目录。
    /// codex 按标签白名单逐块识别；zcode 只认纯注入消息（剥除后无正文），
    /// 混合了正文的仍走 [`TranscriptBlock::Injected`]。
    HarnessContext {
        /// 包裹标签名（不含尖括号），如 environment_context。
        tag: String,
        /// 整条原文（含包裹标签）。
        text: String,
    },
}

/// 读会话日志为文本（替换非 UTF8 序列；坏行由上层按行跳过）。
pub(crate) fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).map_err(|source| Error::DataFile {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 日志内容字段的归一入口：字符串、内容块数组、单个块对象都接受。
///
/// WorkBuddy 的图片附件按"blob 引用块 + 紧跟的本地路径文本"成对记在同一
/// 消息里（同一张图的两个副本）。两路都归一成 [`TranscriptBlock::Image`]，
/// 成对出现时只留 blob 引用——它带原始文件名与体积，信息更全。
pub(crate) fn blocks_from_content(content: &serde_json::Value) -> Vec<TranscriptBlock> {
    match content {
        serde_json::Value::String(text) => text_block_or_image(text).into_iter().collect(),
        serde_json::Value::Array(items) => collapse_image_pairs(items)
            .into_iter()
            .flat_map(block_from_value)
            .collect(),
        serde_json::Value::Object(_) => block_from_value(content),
        _ => vec![],
    }
}

/// 整段文本就是一个 `<image_local_path>…</image_local_path>` 标签时返回内层路径。
/// 混在长文本里的标签不动（那是消息正文的一部分，另有占位处理的空间）。
pub(crate) fn image_local_path_of(text: &str) -> Option<&str> {
    const HEAD: &str = "<image_local_path>";
    const TAIL: &str = "</image_local_path>";
    let inner = text.trim().strip_prefix(HEAD)?.strip_suffix(TAIL)?.trim();
    (!inner.is_empty()).then_some(inner)
}

/// 整段文本就是 zcode 的图片引用 `[Image: source: <path>]` 时返回内层路径。
/// 与 `<image_local_path>` 同类：图片块的第二份记法，通常紧跟图片块成对
/// 出现；混在长文本里的引用不动。
pub(crate) fn image_source_ref_of(text: &str) -> Option<&str> {
    const HEAD: &str = "[Image: source: ";
    let inner = text.trim().strip_prefix(HEAD)?.strip_suffix(']')?.trim();
    (!inner.is_empty() && !inner.contains('\n')).then_some(inner)
}

/// 把 user 文本里的注入段切出来：`tags` 是（开标签前缀, 闭标签）对——开
/// 标签不带 `>` 以兼容属性。返回（剥除后的剩余文本, 注入段原文列表，含包
/// 裹标签）。只认闭合的块；未闭合的开标签视为注入直到结尾，一并截掉（隐
/// 私红线宁可多剥：把标签写进正文当字面文本讨论的场合极罕见）。多类标签
/// 混用时按开标签在文中的出现位置依次切。
pub(crate) fn split_tag_spans(text: &str, tags: &[(&str, &str)]) -> (String, Vec<String>) {
    let mut cleaned = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut cursor = 0;
    while let Some((start, _, tail)) = tags
        .iter()
        .filter_map(|(head, tail)| {
            text[cursor..]
                .find(head)
                .map(|rel| (cursor + rel, *head, *tail))
        })
        .min_by_key(|(start, _, _)| *start)
    {
        match text[start..].find(tail) {
            Some(close_rel) => {
                let end = start + close_rel + tail.len();
                cleaned.push_str(&text[cursor..start]);
                spans.push(text[start..end].to_string());
                cursor = end;
            }
            // 未闭合：余下全是注入——不进剩余文本，整体当一段收集。
            None => {
                cleaned.push_str(&text[cursor..start]);
                spans.push(text[start..].to_string());
                cursor = text.len();
                break;
            }
        }
    }
    cleaned.push_str(&text[cursor..]);
    (cleaned, spans)
}

/// `<system-reminder…>…</system-reminder>` 注入段（workbuddy / zcode 共有）。
pub(crate) fn split_system_reminders(text: &str) -> (String, Vec<String>) {
    split_tag_spans(text, &[("<system-reminder", "</system-reminder>")])
}

/// `<user_query>…</user_query>` 标签里的真实提问（workbuddy / grok 共有，
/// harness 把注入上下文拼在提问前后）；多处出现取第一处。
pub(crate) fn user_query_of(text: &str) -> Option<String> {
    let head = "<user_query>";
    let start = text.find(head)? + head.len();
    let end = start + text[start..].find("</user_query>")?;
    Some(text[start..end].to_string())
}

/// [`top_level_pieces`] 切出的一段：自由文字，或成对 XML 信封（含包裹标签）。
pub(crate) enum TextPiece<'a> {
    Free(&'a str),
    Envelope { tag: &'a str, text: &'a str },
}

/// 把文本切成顶层信封与自由文字（按原文顺序）。开标签名限定小写字母开头
/// （harness 标签都是这个形态，`a < b` 这类散文不误伤）；嵌套由最外层配对
/// 兜住；未闭合的信封视为注入直到结尾——与 [`split_tag_spans`] 的未闭合
/// 语义一致。grok / codebuddy 的信封分段共用。
pub(crate) fn top_level_pieces(text: &str) -> Vec<TextPiece<'_>> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        if start > 0 {
            pieces.push(TextPiece::Free(&rest[..start]));
            rest = &rest[start..];
        }
        match open_tag_name(rest) {
            Some((tag, after_open)) => {
                let close = format!("</{tag}>");
                match rest[after_open..].find(&close) {
                    Some(end_rel) => {
                        let end = after_open + end_rel + close.len();
                        pieces.push(TextPiece::Envelope {
                            tag,
                            text: &rest[..end],
                        });
                        rest = &rest[end..];
                    }
                    None => {
                        pieces.push(TextPiece::Envelope { tag, text: rest });
                        rest = "";
                    }
                }
            }
            None => {
                // '<' 不是合法开标签：当字面文本，跳过一个字符继续扫。
                pieces.push(TextPiece::Free("<"));
                rest = &rest[1..];
            }
        }
    }
    if !rest.is_empty() {
        pieces.push(TextPiece::Free(rest));
    }
    pieces
}

/// `s` 以 `<` 开头时解析开标签名与 `>` 之后的位置；不是合法标签名返回 `None`。
fn open_tag_name(s: &str) -> Option<(&str, usize)> {
    let close_gt = s.find('>')?;
    let name = s[1..close_gt]
        .split([' ', '\t', '\n', '\r'])
        .next()
        .filter(|name| {
            !name.is_empty()
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })?;
    Some((name, close_gt + 1))
}

/// 折叠图片附件的成对记法：纯路径文本紧跟 blob 引用块（workbuddy）或
/// 图片块（zcode，引用文本是 `[Image: source: …]`）时是同一附件的第二份
/// 记法，跳过文本那份。非相邻或单独出现的引用一律保留。
fn collapse_image_pairs(items: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    let mut out = Vec::with_capacity(items.len());
    let mut prev_was_blob = false;
    for item in items {
        let obj = item.as_object();
        let kind = obj
            .and_then(|o| o.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_blob = kind == "image_blob_ref" || zcode_image_resolvable(obj);
        let is_tag_text = matches!(kind, "text" | "input_text" | "output_text")
            && obj
                .and_then(|o| o.get("text"))
                .and_then(|v| v.as_str())
                .map(|text| {
                    image_local_path_of(text).is_some() || image_source_ref_of(text).is_some()
                })
                .unwrap_or(false);
        if is_tag_text && prev_was_blob {
            prev_was_blob = false;
            continue;
        }
        prev_was_blob = is_blob;
        out.push(item);
    }
    out
}

/// zcode 的图片块（`type:"image"`）能否落到可渲染的内容：source.path 在
/// （附件落盘的 image-cache 路径），或 dataUrl 是真实 data URL——日志里
/// 它常被工具脱敏成占位串（"[dataUrl omitted …]"），那不算。
fn zcode_image_resolvable(obj: Option<&serde_json::Map<String, serde_json::Value>>) -> bool {
    obj.and_then(|o| o.get("source"))
        .and_then(|s| s.get("path"))
        .and_then(|v| v.as_str())
        .is_some()
        || obj
            .and_then(|o| o.get("dataUrl"))
            .and_then(|v| v.as_str())
            .is_some_and(|url| url.starts_with("data:image/"))
}

/// 正文里的图片提及 `@image#N:文件名`：扫描出 (起始字节, 结束字节, 文件名)。
/// 文件名吃到空白或 `<` 为止（提及常直接缀在 `</user_query>` 等标签前后）；
/// `#` 与 `:` 之间必须是数字，`@scene#4:"…"` 之类的其他提及不匹配。
fn image_mentions(text: &str) -> Vec<(usize, usize, &str)> {
    const HEAD: &str = "@image#";
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(HEAD) {
        let start = search_from + rel;
        let after_head = start + HEAD.len();
        let rest = &text[after_head..];
        let Some(colon_rel) = rest.find(':') else {
            break;
        };
        let ordinal = &rest[..colon_rel];
        if ordinal.is_empty() || !ordinal.bytes().all(|b| b.is_ascii_digit()) {
            search_from = after_head;
            continue;
        }
        let name_start = after_head + colon_rel + 1;
        let name_end = text[name_start..]
            .find(|c: char| c.is_whitespace() || c == '<')
            .map_or(text.len(), |rel| name_start + rel);
        if name_end > name_start {
            out.push((start, name_end, &text[name_start..name_end]));
        }
        search_from = name_end.max(after_head);
    }
    out
}

/// 把附件图片按正文里的 `@image#N:文件名` 提及插到**粘贴位置**：提及处切开
/// 文本、图片块插入其间——日志把附件统一记在消息尾部，直接渲染会全堆在
/// 文字后面，与用户书写的先后顺序不符。没有提及对应的图片按日志顺序追加
/// 在末尾（旧行为）。提及 → 图片的匹配：优先文件名相同者（多图同名时退回
/// 按出现顺序取下一个未用的）。
pub(crate) fn interleave_image_mentions(blocks: Vec<TranscriptBlock>) -> Vec<TranscriptBlock> {
    let has_image = blocks
        .iter()
        .any(|b| matches!(b, TranscriptBlock::Image { .. }));
    if !has_image {
        return blocks;
    }
    let mut queue: VecDeque<TranscriptBlock> = VecDeque::with_capacity(blocks.len());
    let mut rest: Vec<TranscriptBlock> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if matches!(block, TranscriptBlock::Image { .. }) {
            queue.push_back(block);
        } else {
            rest.push(block);
        }
    }
    let mut out = Vec::new();
    for block in rest {
        let TranscriptBlock::Text { text } = block else {
            out.push(block);
            continue;
        };
        let mentions = image_mentions(&text);
        if mentions.is_empty() {
            out.push(TranscriptBlock::Text { text });
            continue;
        }
        let mut cursor = 0;
        for (start, end, filename) in mentions {
            if start > cursor {
                out.push(TranscriptBlock::Text {
                    text: text[cursor..start].to_string(),
                });
            }
            let matched = queue
                .iter()
                .position(|b| matches!(b, TranscriptBlock::Image { filename: name, .. } if name == filename))
                .unwrap_or(0);
            if let Some(image) = queue.remove(matched) {
                out.push(image);
            }
            cursor = end;
        }
        if cursor < text.len() {
            out.push(TranscriptBlock::Text {
                text: text[cursor..].to_string(),
            });
        }
    }
    out.extend(queue);
    out
}

/// 参数字段 → JSON 文本：字符串原样（有的日志直接存序列化结果），其余值
/// 重新序列化。
pub(crate) fn arguments_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// 单个内容块的归一。认识的 `type` 映射成结构化块（一个输入块可拆出多个
/// 输出块，如 opencode 的 tool part 合记调用与结果）；结构标记（无内容）
/// 返回空，未识别但带 `text` 字段的按文本兜底，其余整块进
/// [`TranscriptBlock::Raw`]。
fn block_from_value(value: &serde_json::Value) -> Vec<TranscriptBlock> {
    let Some(obj) = value.as_object() else {
        // 无结构的散值（裸字符串等）：当文本透传。
        return match value {
            serde_json::Value::String(text) if !text.is_empty() => {
                vec![TranscriptBlock::Text { text: text.clone() }]
            }
            _ => vec![],
        };
    };
    let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
    match kind {
        "text" | "input_text" | "output_text" => {
            let blocks = obj
                .get("text")
                .and_then(|v| v.as_str())
                .and_then(text_block_or_image);
            blocks.into_iter().collect()
        }
        "image_blob_ref" => obj
            .get("blob_path")
            .and_then(|v| v.as_str())
            .map(|path| {
                image_block(
                    path,
                    obj.get("original_filename").and_then(|v| v.as_str()),
                    obj.get("size").and_then(|v| v.as_i64()),
                )
            })
            .into_iter()
            .collect(),
        // zcode 的图片块：附件落盘在 image-cache，路径在 source.path；日志
        // 里的 dataUrl 被工具脱敏成占位串（"[dataUrl omitted …]"），不可
        // 渲染——真实 data URL 才走内嵌，否则取本地路径。两者都无（MCP
        // 工具图，source 只有占位名）不猜，Raw 兜底。
        "image" => image_blocks(value, obj),
        "thinking" | "reasoning" | "summary_text" => {
            let blocks = ["thinking", "text", "reasoning"]
                .iter()
                .find_map(|key| obj.get(*key).and_then(|v| v.as_str()))
                .filter(|text| !text.is_empty())
                .map(|text| TranscriptBlock::Thinking {
                    text: text.to_string(),
                });
            blocks.into_iter().collect()
        }
        "tool_use" | "toolCall" | "function_call" => tool_call_blocks(obj),
        "tool_result" | "toolCallResult" | "function_call_result" => tool_result_blocks(obj),
        // opencode：调用与结果合记在一个 part（state.input / state.output），
        // 拆成 ToolCall + ToolResult 两块；没有 output（调用失败中断等）只留调用。
        "tool" => opencode_tool_blocks(obj),
        // opencode 粘贴/拖入的文件：图片以 data URL 内嵌，原样透传给前端渲染；
        // 非图片文件不猜内容，Raw 兜底。
        "file" => file_blocks(value, obj),
        // opencode 的流式结构标记：step-start / step-finish（计费已在
        // message 级 tokens 里）、compaction（压缩边界）——无内容可展示。
        "step-start" | "step-finish" | "compaction" => vec![],
        _ => fallback_blocks(value, obj),
    }
}

/// zcode 的图片块：附件落盘在 image-cache，路径在 source.path；日志
/// 里的 dataUrl 被工具脱敏成占位串（"[dataUrl omitted …]"），不可
/// 渲染——真实 data URL 才走内嵌，否则取本地路径。两者都无（MCP
/// 工具图，source 只有占位名）不猜，Raw 兜底。
fn image_blocks(
    value: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Vec<TranscriptBlock> {
    let source = obj.get("source").and_then(|v| v.as_object());
    let placeholder = source
        .and_then(|s| s.get("placeholder"))
        .and_then(|v| v.as_str());
    let url = obj.get("dataUrl").and_then(|v| v.as_str()).unwrap_or("");
    if url.starts_with("data:image/") {
        vec![image_data_block(url, placeholder)]
    } else if let Some(path) = source.and_then(|s| s.get("path")).and_then(|v| v.as_str()) {
        let size = source
            .and_then(|s| s.get("sizeBytes"))
            .and_then(|v| v.as_i64());
        vec![image_block(path, placeholder, size)]
    } else {
        let json = serde_json::to_string(value).ok();
        json.map(|json| TranscriptBlock::Raw { json })
            .into_iter()
            .collect()
    }
}

/// 工具调用块：`id`/`call_id`、`input`/`arguments` 字段名跨工具不同，都兜住。
fn tool_call_blocks(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<TranscriptBlock> {
    let id = obj
        .get("id")
        .or_else(|| obj.get("call_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let name = obj.get("name").and_then(|v| v.as_str()).map(str::to_string);
    let arguments = obj
        .get("input")
        .or_else(|| obj.get("arguments"))
        .map(arguments_text);
    vec![TranscriptBlock::ToolCall {
        id,
        name,
        arguments,
    }]
}

/// 工具结果块：call_id 在三种字段名里找；content/output/text 都可能是任意
/// JSON 值，统一走 [`value_text`]。
fn tool_result_blocks(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<TranscriptBlock> {
    let call_id = ["tool_use_id", "call_id", "id"]
        .iter()
        .find_map(|key| obj.get(*key))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let content = ["content", "output", "text"]
        .iter()
        .find_map(|key| obj.get(*key))
        .map(value_text)
        .unwrap_or_default();
    let is_error = obj
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    vec![TranscriptBlock::ToolResult {
        call_id,
        content,
        is_error,
    }]
}

/// opencode：调用与结果合记在一个 part（state.input / state.output），
/// 拆成 ToolCall + ToolResult 两块；没有 output（调用失败中断等）只留调用。
fn opencode_tool_blocks(obj: &serde_json::Map<String, serde_json::Value>) -> Vec<TranscriptBlock> {
    let state = obj.get("state");
    let mut blocks = vec![TranscriptBlock::ToolCall {
        id: obj
            .get("callID")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        name: obj.get("tool").and_then(|v| v.as_str()).map(str::to_string),
        arguments: state.and_then(|s| s.get("input")).map(arguments_text),
    }];
    if let Some(output) = state
        .and_then(|s| s.get("output"))
        .and_then(|v| v.as_str())
        .filter(|text| !text.is_empty())
    {
        blocks.push(TranscriptBlock::ToolResult {
            call_id: obj
                .get("callID")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            content: output.to_string(),
            is_error: state.and_then(|s| s.get("status")).and_then(|v| v.as_str()) == Some("error"),
        });
    }
    blocks
}

/// opencode 粘贴/拖入的文件：图片以 data URL 内嵌，原样透传给前端渲染；
/// 非图片文件不猜内容，Raw 兜底。
fn file_blocks(
    value: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Vec<TranscriptBlock> {
    let url = obj.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.starts_with("data:image/") {
        vec![image_data_block(
            url,
            obj.get("filename").and_then(|v| v.as_str()),
        )]
    } else {
        let json = serde_json::to_string(value).ok();
        json.map(|json| TranscriptBlock::Raw { json })
            .into_iter()
            .collect()
    }
}

/// 未识别 kind 的兜底：带非空 `text` 字段按文本透传，否则整块 Raw 留档——
/// 未认识的日志格式宁可原样展示也不丢内容。
fn fallback_blocks(
    value: &serde_json::Value,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Vec<TranscriptBlock> {
    if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
        return (!text.is_empty())
            .then(|| TranscriptBlock::Text {
                text: text.to_string(),
            })
            .into_iter()
            .collect();
    }
    let json = serde_json::to_string(value)
        .ok()
        .map(|json| TranscriptBlock::Raw { json });
    json.into_iter().collect()
}

/// 文本块归一：整块是图片路径标签 / zcode 图片引用 → Image；否则原样成 Text。
fn text_block_or_image(text: &str) -> Option<TranscriptBlock> {
    if let Some(path) = image_local_path_of(text) {
        return Some(image_block(path, None, None));
    }
    if let Some(path) = image_source_ref_of(text) {
        return Some(image_block(path, None, None));
    }
    (!text.is_empty()).then(|| TranscriptBlock::Text {
        text: text.to_string(),
    })
}

/// 图片引用块：路径必填；文件名缺省取路径尾段。`exists` 现场探测——
/// 附件可能已被工具清理，前端据此显示占位或缺失态。
fn image_block(path: &str, filename: Option<&str>, size: Option<i64>) -> TranscriptBlock {
    let path = path.trim().to_string();
    let filename = filename
        .map(str::to_string)
        .or_else(|| {
            // 工具日志里的路径是 Windows 形态（\ 分隔），宿主平台未必是
            // Windows——Path::file_name 在 Unix 不认 `\`，会整段返回。按两种
            // 分隔符手工取尾段，两端平台行为一致。
            path.rsplit(['/', '\\'])
                .find(|seg| !seg.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let exists = std::path::Path::new(&path).is_file();
    TranscriptBlock::Image {
        path,
        filename,
        size,
        exists,
        data_url: None,
    }
}

/// 内嵌图片块（opencode 的 data URL file part）：内容已在库里，现场
/// 可渲染；体积从 base64 长度反推（base64 约 4/3 倍原始字节）。
fn image_data_block(url: &str, filename: Option<&str>) -> TranscriptBlock {
    // "data:image/png;base64,XXXX" 的尾段是 base64 载荷。
    let payload = url.split_once(',').map(|(_, rest)| rest).unwrap_or("");
    let size = (payload.len() / 4 * 3).try_into().ok();
    TranscriptBlock::Image {
        path: String::new(),
        filename: filename.unwrap_or("image").to_string(),
        size,
        exists: true,
        data_url: Some(url.to_string()),
    }
}

/// 工具结果内容摊平成文本：字符串原样；块数组取各块的 text 按行拼接；
/// 其余值序列化。
pub(crate) fn value_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                serde_json::Value::String(text) => Some(text.clone()),
                other => other
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(str::to_string),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// pi / dsh 同源格式的转录提取：`type:"session"` 头行切换归属，消息行不带
/// 会话 id，只保留当前头行与目标会话一致的消息（compaction 换头后旧行自然
/// 排除）。见两适配器模块文档。
pub(crate) fn pi_style_transcript(lines: &[&str], external_id: &str) -> Vec<TranscriptEntry> {
    #[derive(serde::Deserialize)]
    struct RawLine {
        #[serde(rename = "type", default)]
        kind: String,
        #[serde(default)]
        id: String,
        /// 外层行时间戳（ISO 字符串）。
        #[serde(default)]
        timestamp: Option<String>,
        #[serde(default)]
        message: Option<RawMessage>,
    }
    #[derive(serde::Deserialize)]
    struct RawMessage {
        #[serde(default)]
        role: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        content: Option<serde_json::Value>,
        /// 内层时间戳：epoch ms 整数，优先于外层。
        #[serde(default)]
        timestamp: Option<i64>,
    }

    let mut current: Option<String> = None;
    let mut entries = Vec::new();
    for line in lines {
        let Ok(raw) = serde_json::from_str::<RawLine>(line) else {
            continue;
        };
        if raw.kind == "session" {
            if !raw.id.is_empty() {
                current = Some(raw.id);
            }
            continue;
        }
        if raw.kind != "message" || current.as_deref() != Some(external_id) {
            continue;
        }
        let Some(message) = raw.message else {
            continue;
        };
        let role = match message.role.as_str() {
            "user" => TranscriptRole::User,
            "assistant" => TranscriptRole::Assistant,
            "system" => TranscriptRole::System,
            _ => continue,
        };
        let blocks = message
            .content
            .as_ref()
            .map(blocks_from_content)
            .unwrap_or_default();
        if blocks.is_empty() {
            continue;
        }
        let ts_ms = message.timestamp.or_else(|| {
            chrono::DateTime::parse_from_rfc3339(raw.timestamp.as_deref()?)
                .ok()
                .map(|t| t.timestamp_millis())
        });
        let model = message.model.filter(|m| !m.trim().is_empty());
        entries.push(TranscriptEntry {
            role,
            ts_ms,
            model,
            blocks,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_content_becomes_single_text_block() {
        let blocks = blocks_from_content(&json!("帮我看看这个 bug"));
        assert_eq!(
            blocks,
            vec![TranscriptBlock::Text {
                text: "帮我看看这个 bug".into()
            }]
        );
        assert_eq!(blocks_from_content(&json!("")), vec![]);
    }

    /// 注入段切分：标签前后与中间的正文都保留在剩余文本里，注入段带标签
    /// 原样收集；未闭合的开标签截到结尾。
    #[test]
    fn split_system_reminders_separates_spans_keeps_surroundings() {
        let (cleaned, spans) = split_system_reminders(
            "前文<system-reminder>a</system-reminder>中<system-reminder data-role=\"x\">b\n</system-reminder>后文",
        );
        assert_eq!(cleaned, "前文中后文");
        assert_eq!(
            spans,
            vec![
                "<system-reminder>a</system-reminder>".to_string(),
                "<system-reminder data-role=\"x\">b\n</system-reminder>".to_string()
            ]
        );
        let (cleaned, spans) = split_system_reminders("没有标签");
        assert_eq!(cleaned, "没有标签");
        assert!(spans.is_empty());
        // 未闭合：开标签起视为注入直到结尾，整体收进注入段。
        let (cleaned, spans) = split_system_reminders("前文<system-reminder>截断");
        assert_eq!(cleaned, "前文");
        assert_eq!(spans, vec!["<system-reminder>截断".to_string()]);
        let (cleaned, spans) = split_system_reminders("");
        assert_eq!(cleaned, "");
        assert!(spans.is_empty());
    }

    /// 多类注入标签混排：按开标签在文中的出现位置依次切，各归其段。
    #[test]
    fn split_tag_spans_handles_mixed_tags() {
        let tags = &[
            ("<system-reminder", "</system-reminder>"),
            ("<in-app-browser-context", "</in-app-browser-context>"),
        ];
        let (cleaned, spans) = split_tag_spans(
            "<in-app-browser-context>a</in-app-browser-context>中<system-reminder>b</system-reminder>后",
            tags,
        );
        assert_eq!(cleaned, "中后");
        assert_eq!(
            spans,
            vec![
                "<in-app-browser-context>a</in-app-browser-context>".to_string(),
                "<system-reminder>b</system-reminder>".to_string()
            ]
        );
        // 未闭合的同类标签同样截到结尾。
        let (cleaned, spans) = split_tag_spans("前<in-app-browser-context>a中", tags);
        assert_eq!(cleaned, "前");
        assert_eq!(spans, vec!["<in-app-browser-context>a中".to_string()]);
    }

    #[test]
    fn claude_style_blocks_are_mapped() {
        let content = json!([
            { "type": "thinking", "thinking": "先分析" },
            { "type": "text", "text": "结论" },
            { "type": "tool_use", "id": "t1", "name": "bash", "input": { "cmd": "ls" } },
            { "type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "ok"}], "is_error": true }
        ]);
        let blocks = blocks_from_content(&content);
        assert_eq!(
            blocks,
            vec![
                TranscriptBlock::Thinking {
                    text: "先分析".into()
                },
                TranscriptBlock::Text {
                    text: "结论".into()
                },
                TranscriptBlock::ToolCall {
                    id: Some("t1".into()),
                    name: Some("bash".into()),
                    arguments: Some(r#"{"cmd":"ls"}"#.into()),
                },
                TranscriptBlock::ToolResult {
                    call_id: Some("t1".into()),
                    content: "ok".into(),
                    is_error: true,
                },
            ]
        );
    }

    #[test]
    fn codex_input_output_text_and_reasoning_kinds_are_mapped() {
        let blocks = blocks_from_content(&json!([
            { "type": "input_text", "text": "问题" },
            { "type": "output_text", "text": "回答" },
            { "type": "summary_text", "text": "推理摘要" }
        ]));
        assert_eq!(
            blocks,
            vec![
                TranscriptBlock::Text {
                    text: "问题".into()
                },
                TranscriptBlock::Text {
                    text: "回答".into()
                },
                TranscriptBlock::Thinking {
                    text: "推理摘要".into()
                },
            ]
        );
    }

    #[test]
    fn unknown_blocks_pass_through_as_raw() {
        let content = json!([
            { "type": "image", "source": { "kind": "base64" } },
            { "type": "weird" }
        ]);
        let blocks = blocks_from_content(&content);
        assert_eq!(blocks.len(), 2);
        for block in &blocks {
            assert!(
                matches!(block, TranscriptBlock::Raw { .. }),
                "未识别块应透传"
            );
        }
        let TranscriptBlock::Raw { json } = &blocks[0] else {
            panic!("应为 Raw");
        };
        assert!(json.contains("base64"), "原文必须保留");
    }

    /// WorkBuddy 粘贴图片的成对记法：blob 引用块 + 紧跟的路径文本。
    /// 归一成一个 Image 块，取 blob 引用的元数据；路径文本那份折叠掉。
    #[test]
    fn workbuddy_image_pair_collapses_into_single_image_block() {
        let path = "C:\\Users\\me\\.workbuddy\\blobs\\ab\\abc.png";
        let content = json!([
            { "type": "input_text", "text": "看看这张图" },
            { "type": "image_blob_ref", "blob_id": "abc", "blob_path": path,
              "mime": "image/png", "original_filename": "Clipboard_Screenshot.png", "size": 194_910 },
            { "type": "input_text", "text": format!("<image_local_path>{}</image_local_path>", r"C:\Users\me\.workbuddy\clipboard-images\x.png") }
        ]);
        let blocks = blocks_from_content(&content);
        assert_eq!(blocks.len(), 2, "路径文本与 blob 引用合并");
        assert_eq!(
            blocks[0],
            TranscriptBlock::Text {
                text: "看看这张图".into()
            }
        );
        let TranscriptBlock::Image {
            path,
            filename,
            size,
            exists,
            data_url,
        } = &blocks[1]
        else {
            panic!("应为 Image");
        };
        assert_eq!(path, "C:\\Users\\me\\.workbuddy\\blobs\\ab\\abc.png");
        assert_eq!(filename, "Clipboard_Screenshot.png");
        assert_eq!(*size, Some(194_910));
        assert_eq!(*data_url, None);
        // 测试路径不存在：exists 如实为 false，不编造。
        assert!(!exists);
    }

    /// 单独出现的路径标签（无 blob 引用配对）也归一成 Image，文件名取尾段。
    #[test]
    fn standalone_image_local_path_tag_becomes_image_block() {
        let blocks = blocks_from_content(&json!([
            { "type": "input_text", "text": r#"<image_local_path>C:\imgs\shot-1.png</image_local_path>"# }
        ]));
        assert_eq!(
            blocks,
            vec![TranscriptBlock::Image {
                path: r"C:\imgs\shot-1.png".into(),
                filename: "shot-1.png".into(),
                size: None,
                exists: false,
                data_url: None,
            }]
        );
    }

    /// 混在长文本里的路径标签是消息正文的一部分，不拆不转。
    #[test]
    fn image_tag_inside_larger_text_stays_text() {
        let mixed = format!(
            "先看这个 <image_local_path>{}</image_local_path> 再说",
            r"C:\imgs\shot.png"
        );
        let blocks = blocks_from_content(&json!([{ "type": "input_text", "text": mixed }]));
        assert_eq!(blocks, vec![TranscriptBlock::Text { text: mixed }]);
    }

    /// zcode 分享图片的成对记法：图片块（source.path，dataUrl 已被日志
    /// 脱敏）+ 紧跟的 `[Image: source: …]` 引用文本。归一成一个 Image 块，
    /// 文件名取 source.placeholder；引用文本那份折叠掉。
    #[test]
    fn zcode_image_pair_collapses_into_single_image_block() {
        let content = json!([
            { "type": "text", "text": "是不是就是这样的。\n我们先讨论，。" },
            { "type": "image", "mediaType": "image/png",
              "dataUrl": "[dataUrl omitted from model-io: image/png, 115314 chars]",
              "source": { "id": "turn-attachment-1", "kind": "inline", "mimeType": "image/png",
                          "placeholder": "image.png",
                          "path": r"C:\Users\me\.zcode\cli\image-cache\s1\image-x.png" } },
            { "type": "text", "text": r"[Image: source: C:\Users\me\.zcode\cli\image-cache\s1\image-x.png]" }
        ]);
        let blocks = blocks_from_content(&content);
        assert_eq!(blocks.len(), 2, "引用文本与图片块合并");
        assert_eq!(
            blocks[0],
            TranscriptBlock::Text {
                text: "是不是就是这样的。\n我们先讨论，。".into()
            }
        );
        let TranscriptBlock::Image {
            path,
            filename,
            size,
            exists,
            data_url,
        } = &blocks[1]
        else {
            panic!("应为 Image");
        };
        assert_eq!(path, r"C:\Users\me\.zcode\cli\image-cache\s1\image-x.png");
        assert_eq!(filename, "image.png");
        assert_eq!(*size, None, "脱敏日志不带体积");
        assert_eq!(*data_url, None, "脱敏串绝不能当 data URL 渲染");
        assert!(!exists, "测试路径不存在，exists 如实为 false");
    }

    /// 单独出现的引用文本（无图片块配对）也归一成 Image，文件名取尾段。
    #[test]
    fn zcode_image_ref_text_alone_becomes_image_block() {
        let blocks = blocks_from_content(&json!([
            { "type": "text", "text": r"[Image: source: C:\imgs\shot.png]" }
        ]));
        assert_eq!(
            blocks,
            vec![TranscriptBlock::Image {
                path: r"C:\imgs\shot.png".into(),
                filename: "shot.png".into(),
                size: None,
                exists: false,
                data_url: None,
            }]
        );
    }

    /// 混在长文本里的引用是消息正文的一部分，不拆不转。
    #[test]
    fn zcode_image_ref_inside_larger_text_stays_text() {
        let mixed = r"图在 [Image: source: C:\imgs\shot.png] 这里".to_string();
        let blocks = blocks_from_content(&json!([{ "type": "text", "text": mixed }]));
        assert_eq!(blocks, vec![TranscriptBlock::Text { text: mixed }]);
    }

    /// MCP 工具图（source 无 path、dataUrl 已脱敏）：解析不出本地文件也
    /// 不猜，Raw 兜底保留原文。
    #[test]
    fn zcode_mcp_image_without_path_stays_raw() {
        let blocks = blocks_from_content(&json!([
            { "type": "image", "mediaType": "image/png",
              "dataUrl": "[dataUrl omitted from model-io: image/png, 36360 chars]",
              "source": { "id": "mcp-image", "kind": "inline", "mimeType": "image/png",
                          "placeholder": "MCP image", "sizeBytes": 36360 } }
        ]));
        assert_eq!(blocks.len(), 1);
        let TranscriptBlock::Raw { json } = &blocks[0] else {
            panic!("应为 Raw");
        };
        assert!(json.contains("mcp-image"), "原文必须保留");
    }

    /// 连续粘贴两张图：两对记法各自折叠，互不误伤。
    #[test]
    fn consecutive_image_pairs_each_collapse_once() {
        let content = json!([
            { "type": "image_blob_ref", "blob_path": r"C:\b\a.png", "original_filename": "a.png", "size": 1 },
            { "type": "input_text", "text": r#"<image_local_path>C:\c\a.png</image_local_path>"# },
            { "type": "image_blob_ref", "blob_path": r"C:\b\b.png", "original_filename": "b.png", "size": 2 },
            { "type": "input_text", "text": r#"<image_local_path>C:\c\b.png</image_local_path>"# }
        ]);
        let blocks = blocks_from_content(&content);
        let names: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                TranscriptBlock::Image { filename, .. } => Some(filename.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["a.png", "b.png"], "两对各自保留 blob 引用");
    }

    /// opencode 的 tool part：调用与结果合记，拆成 ToolCall + ToolResult；
    /// step 标记与 compaction 无内容，直接跳过。
    #[test]
    fn opencode_tool_part_splits_and_step_markers_are_skipped() {
        let content = json!([
            { "type": "step-start" },
            { "type": "tool", "tool": "bash", "callID": "call_1",
              "state": { "status": "completed",
                         "input": { "command": "ls" },
                         "output": "a\nb", "metadata": { "exit": 0 } } },
            { "type": "step-finish", "tokens": { "input": 1 } },
            { "type": "compaction", "auto": true }
        ]);
        let blocks = blocks_from_content(&content);
        assert_eq!(
            blocks,
            vec![
                TranscriptBlock::ToolCall {
                    id: Some("call_1".into()),
                    name: Some("bash".into()),
                    arguments: Some(r#"{"command":"ls"}"#.into()),
                },
                TranscriptBlock::ToolResult {
                    call_id: Some("call_1".into()),
                    content: "a\nb".into(),
                    is_error: false,
                },
            ]
        );
    }

    /// opencode 的 file part：图片 data URL 原样进 Image 块，体积从载荷
    /// 长度反推；非图片文件 Raw 兜底。
    #[test]
    fn opencode_file_part_becomes_image_with_data_url() {
        let png = "data:image/png;base64,iVBORw0KGgoAAAAN"; // 载荷 16 字符
        let blocks = blocks_from_content(&json!([
            { "type": "file", "mime": "image/png", "filename": "image", "url": png },
            { "type": "file", "mime": "text/plain", "filename": "x.txt", "url": "data:text/plain;base64,SGk=" }
        ]));
        assert_eq!(
            blocks[0],
            TranscriptBlock::Image {
                path: String::new(),
                filename: "image".into(),
                size: Some(12), // 16/4*3
                exists: true,
                data_url: Some(png.into()),
            }
        );
        let TranscriptBlock::Raw { json } = &blocks[1] else {
            panic!("非图片文件应为 Raw");
        };
        assert!(json.contains("text/plain"), "原文必须保留");
    }
    /// 线上字段名必须是 camelCase：枚举容器的 rename_all 不覆盖变体字段，
    /// 少了 rename_all_fields 时 dataUrl / isError / callId 全部变成
    /// snake_case，前端读 camelCase 全是 undefined（裂图事故的根因）。
    #[test]
    fn serialized_field_names_are_camel_case() {
        let image = serde_json::to_string(&image_named("a.png")).unwrap();
        assert!(image.contains("\"dataUrl\""), "{image}");
        let result = serde_json::to_string(&TranscriptBlock::ToolResult {
            call_id: Some("c1".into()),
            content: "ok".into(),
            is_error: true,
        })
        .unwrap();
        assert!(result.contains("\"callId\""), "{result}");
        assert!(result.contains("\"isError\""), "{result}");
    }

    fn image_named(name: &str) -> TranscriptBlock {
        TranscriptBlock::Image {
            path: format!(r"C:\b\{name}"),
            filename: name.into(),
            size: Some(1),
            exists: false,
            data_url: None,
        }
    }

    /// 提及处插入：正文里 @image#N:文件名 的位置就是图片渲染的位置——
    /// 提及处切开文本，附件块插进去（日志里它们原本都挂在消息尾部）。
    #[test]
    fn image_mentions_interleave_attachments_at_paste_position() {
        let blocks = vec![
            TranscriptBlock::Text {
                text: "前文 @image#1:a.png 中间 @image#2:b.png 后文".into(),
            },
            image_named("a.png"),
            image_named("b.png"),
        ];
        let out = interleave_image_mentions(blocks);
        let shapes: Vec<String> = out
            .iter()
            .map(|b| match b {
                TranscriptBlock::Text { text } => format!("T({text})"),
                TranscriptBlock::Image { filename, .. } => format!("I({filename})"),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(
            shapes,
            vec!["T(前文 )", "I(a.png)", "T( 中间 )", "I(b.png)", "T( 后文)",]
        );
    }

    /// 提及文件名缀着 `</user_query>` 结尾时文件名截到 `<` 为止，仍能对上；
    /// 没被提及的图片按日志顺序追加在末尾（旧行为）。
    #[test]
    fn unmatched_images_append_and_tag_adjacent_mention_matches() {
        let blocks = vec![
            TranscriptBlock::Text {
                text: "@image#1:shot.png</user_query> 正文".into(),
            },
            image_named("shot.png"),
            image_named("unused.png"),
        ];
        let out = interleave_image_mentions(blocks);
        let shapes: Vec<String> = out
            .iter()
            .map(|b| match b {
                TranscriptBlock::Text { text } => format!("T({text})"),
                TranscriptBlock::Image { filename, .. } => format!("I({filename})"),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(
            shapes,
            // 文件名截到 `<`，`</user_query>` 标签留在文本段（转录对标记
            // 原样透传的既有口径）。
            vec!["I(shot.png)", "T(</user_query> 正文)", "I(unused.png)"]
        );
    }

    /// 两图同名（连拍）：文件名匹配不到唯一时按出现顺序取下一个未用的。
    #[test]
    fn same_filename_mentions_consume_images_in_order() {
        let blocks = vec![
            TranscriptBlock::Text {
                text: "@image#1:clip.png 然后 @image#2:clip.png".into(),
            },
            image_named("clip.png"),
            image_named("clip.png"),
        ];
        let out = interleave_image_mentions(blocks);
        let shapes: Vec<String> = out
            .iter()
            .map(|b| match b {
                TranscriptBlock::Text { text } => format!("T({text})"),
                TranscriptBlock::Image { filename, .. } => format!("I({filename})"),
                _ => "?".into(),
            })
            .collect();
        // 首个提及在文本开头：前段为空不产文本块。
        assert_eq!(shapes, vec!["I(clip.png)", "T( 然后 )", "I(clip.png)"]);
    }

    /// 无图片的消息与 `@scene#N:"…"` 等其他提及完全不受影响。
    #[test]
    fn non_image_mentions_and_imageless_messages_pass_through() {
        let blocks = vec![TranscriptBlock::Text {
            text: "@scene#4:\"文档处理\" 普通 @image#1: 提及（无文件名）".into(),
        }];
        let expected = blocks.clone();
        let out = interleave_image_mentions(blocks);
        assert_eq!(out, expected, "无图片块时原样返回");
    }

    #[test]
    fn tool_call_arguments_accept_string_and_object() {
        let as_string = blocks_from_content(&json!([
            { "type": "tool_use", "id": "t", "name": "bash", "input": "{\"cmd\":1}" }
        ]));
        let TranscriptBlock::ToolCall { arguments, .. } = &as_string[0] else {
            panic!("应为 ToolCall");
        };
        assert_eq!(arguments.as_deref(), Some("{\"cmd\":1}"), "字符串参数原样");

        let as_object = blocks_from_content(&json!([
            { "type": "function_call", "call_id": "c", "name": "n", "arguments": { "a": 1 } }
        ]));
        let TranscriptBlock::ToolCall { id, arguments, .. } = &as_object[0] else {
            panic!("应为 ToolCall");
        };
        assert_eq!(id.as_deref(), Some("c"), "call_id 也当 id");
        assert_eq!(arguments.as_deref(), Some(r#"{"a":1}"#));
    }

    #[test]
    fn pi_style_lines_are_filtered_by_current_session_header() {
        let lines = [
            r#"{"type":"session","id":"s1","cwd":"E:\\p"}"#,
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"第一条"}],"timestamp":1000}}"#,
            // compaction 换头：新头之后的行归属新会话。
            r#"{"type":"session","id":"s2"}"#,
            r#"{"type":"message","message":{"role":"user","content":"第二条"}}"#,
            r#"{"type":"message","message":{"role":"assistant","model":"m","content":[{"type":"text","text":"回答"}],"timestamp":2000}}"#,
        ];
        let entries = pi_style_transcript(&lines, "s1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ts_ms, Some(1000));

        let entries = pi_style_transcript(&lines, "s2");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].model.as_deref(), Some("m"), "assistant 带模型");
        assert_eq!(
            entries[1].blocks[0],
            TranscriptBlock::Text {
                text: "回答".into()
            }
        );
    }

    #[test]
    fn garbage_lines_are_skipped_silently() {
        let lines = [
            "{ not json",
            r#"{"type":"session","id":"s1"}"#,
            "not json at all",
            r#"{"type":"message","message":{"role":"user","content":"ok"}}"#,
        ];
        let entries = pi_style_transcript(&lines, "s1");
        assert_eq!(entries.len(), 1);
    }
}
