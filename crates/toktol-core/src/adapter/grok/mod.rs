//! grok 适配器：解析 `~/.grok/sessions/**/updates.jsonl`——每回合一条
//! `_x.ai/session/update` 事件，自带 `params.sessionId` 与**当次回合**的用量
//! （非累计）。行级解析即可；项目目录是唯一的路径派生事实——编码在会话
//! 目录名里（URL 编码的 cwd，[`Self::parse_file_at`] 解码）。
//!
//! 红线：会话目录里的 `events.jsonl` 记录 MCP 启动命令，其中含带密钥的 URL——
//! 它不匹配 [`Self::is_session_log`]，绝不解析；同级的 `config.toml`、`credentials`
//! 类文件一律不碰。
//!
//! 口径（实测）：`totalTokens = inputTokens + outputTokens`，`cachedReadTokens`
//! 是 input 的一部分（OpenAI 语义，与 workbuddy 相同），`reasoningTokens` 是
//! output 的一部分——入库前拆出，否则缓存读按输入价二次计费。
//! 已知取舍：一回合多模型时只取 totalTokens 最大的那个（本机数据均为单模型）；
//! 请求数取全部模型的 `modelCalls` 之和（token 是回合聚合，调用次数只有它给）。
//!
//! 请求级拆分（[`Self::parse_file_at`]）：turn_completed 的 token 是整回合
//! 聚合，真实网络请求数在 `modelUsage[].modelCalls`。按兄弟文件
//! chat_history.jsonl 分段出的同序回合拆成 modelCalls 条请求级行：净输入与
//! 缓存写 ∝ 上下文增长、缓存读 ∝ 上一请求的上下文、输出与推理 ∝ assistant
//! 行字节（比例估计——本地数据不存在每次请求的 token）；请求时刻用
//! assistant 的 tool_call 事件（`params._meta.agentTimestampMs`）锚定。
//! 耗时是真实时间戳推导（非估算）：请求起点 = 上一请求的工具执行完成
//! （最后一条 tool_call_update；首请求用 user_message 时刻），终点 = 本请求
//! 响应完成（tool_call 最早事件；末请求用 turn_completed 时刻）——工具执行
//! 时间被剔除，Σ ≤ elapsed_ms 的差额即工具执行。互锁：回合的 assistant 数
//! ≠ modelCalls 时整行回退为单行（耗时取 elapsed_ms）。chat_history 只增
//! 不删，已入账回合的拆分结果是前缀稳定的。
//!
//! 转录（[`Self::read_transcript`]）：读同目录 `chat_history.jsonl`（本机 2026-10
//! 实测验证）。行格式：`{type, content, …}`，type 为 user / assistant /
//! tool_result / system；user 的 content 是块数组，assistant 是纯文本加
//! `tool_calls[{id,name,arguments}]`（arguments 是 JSON 字符串），并有
//! `model_id`；tool_result 带 `tool_call_id`。行内**无 sessionId 也无时间戳**：
//! 归属靠目录——登记源 updates.jsonl 与转录同处一个会话目录，整文件即该会话；
//! ts 留空（与"绝不编造"的口径一致）。user 文本分段（实测：`<user_info>`/
//! `<rules>`/`<git_status>` 等会话级信封只在首条 user 行出现一次，
//! `<system-reminder>` 是 harness 事件广播的独立行）：信封整体归
//! HarnessContext 块并拆成前置独立条目（codex 口径——中性卡、不进气泡、
//! 不锚轮次），`<system-reminder>` 段归 Injected，真实提问在 `<user_query>`
//! 标签里（与 workbuddy 同一约定，共享提取）；`synthetic_reason` 非空标记
//! 整行注入，独立成条后吸收进相邻用户回合（有前序回合归前序——事件发生
//! 在该回合内、模型应答时已看到；会话开头落到最近的后继回合），呈现与
//! workbuddy 的行内 reminder 对齐。system 行是各会话相同的 Grok 通用系统
//! 提示，跳过。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::transcript::{
    self, TextPiece, TranscriptBlock, TranscriptEntry, TranscriptRole, top_level_pieces,
};
use super::{Adapter, LineFacts, LineParse, UsageFacts};
use crate::error::Result;
use crate::model::{TokenUsage, Tool};

/// grok 的适配器实现；逐行解析回合完成事件。
pub struct GrokAdapter;

impl Adapter for GrokAdapter {
    fn tool(&self) -> Tool {
        Tool::Grok
    }

    fn log_dirs(&self) -> Vec<PathBuf> {
        match crate::paths::home_dir() {
            Some(home) => vec![home.join(".grok").join("sessions")],
            None => vec![],
        }
    }

    /// 只认 updates.jsonl；events.jsonl（含密钥 URL）与 chat_history.jsonl（无
    /// sessionId、无用量）都不匹配。
    fn is_session_log(&self, path: &Path) -> bool {
        path.file_name().is_some_and(|name| name == "updates.jsonl")
    }

    /// 会话 = 整个目录（updates.jsonl 只是入口，旁边还有 chat_history、summary 等）。
    fn session_artifacts(&self, source_file: &Path) -> Vec<PathBuf> {
        source_file
            .parent()
            .map(|dir| vec![dir.to_path_buf()])
            .unwrap_or_default()
    }

    fn parse_line(&self, line: &str) -> LineParse {
        let raw: RawLine = match serde_json::from_str(line) {
            Ok(raw) => raw,
            Err(_) => return LineParse::Malformed,
        };
        if raw.method.as_deref() != Some("_x.ai/session/update") {
            return LineParse::Skip;
        }
        let params = match raw.params {
            Some(params) => params,
            None => return LineParse::Skip,
        };
        let Some(session_external_id) = params
            .session_id
            .filter(|id| !id.trim().is_empty())
            .map(|id| id.trim().to_string())
        else {
            return LineParse::Skip;
        };
        // 行时间戳是 epoch 秒；usage_records.ts 是 epoch ms。
        let ts_ms = raw
            .timestamp
            .filter(|&ts| ts > 0)
            .map(|ts| ts.saturating_mul(1_000));

        // 真实网络请求数 = 全部模型的 modelCalls 之和；缺字段按 1 兜底——
        // 宁可少计，不凭空多计。
        let request_count = params
            .update
            .as_ref()
            .and_then(|update| update.usage.as_ref())
            .and_then(|usage| usage.model_usage.as_ref())
            .map(|per_model| per_model.values().map(|m| m.model_calls).sum::<i64>())
            .unwrap_or(0)
            .max(1);

        let Some(update) = params.update else {
            return LineParse::Skip;
        };
        if update.session_update.as_deref() != Some("turn_completed") {
            return LineParse::Skip;
        }
        let Some(usage) = update.usage else {
            return LineParse::Skip;
        };

        // 一回合多模型时取 totalTokens 最大的那个；其余丢失（本机数据均为单模型）。
        // modelUsage 缺失时无法归属模型（model 列非空），跳过。请求数取全部
        // 模型的 modelCalls 之和（真实网络请求不因聚合取舍而变少），缺字段时
        // 按 1 兜底——宁可少计，不凭空多计。
        let Some(usage) = usage.model_usage.as_ref().and_then(|per_model| {
            per_model
                .iter()
                .max_by_key(|(_, m)| m.total_tokens)
                .and_then(|(model, m)| usage_facts(m, model, ts_ms))
        }) else {
            return LineParse::Skip;
        };

        LineParse::Facts(Box::new(LineFacts {
            dedup_suffix: None,
            request_count,
            session_external_id,
            title: None,
            project_dir: None,
            ts_ms,
            usage: Some(usage),
        }))
    }

    /// 项目目录编码在会话目录名里：`~/.grok/sessions/<URL 编码的 cwd>/
    /// <sessionId>/updates.jsonl`（实测 `E%3A%5CDevelopment%5C…` =
    /// `E:\Development\…`）。workbuddy 的"路径派生事实"同款；解码失败的行
    /// project_dir 留空，不影响用量归属。
    ///
    /// 同时做请求级拆分（见模块文档"请求级拆分"）：turn_completed 的回合
    /// 聚合按同序回合的 assistant 数拆成请求级行。明细缺失或形状对不上时
    /// 整行回退为单行（request_count = modelCalls 之和），扫描不因明细缺失失败。
    fn parse_file_at(&self, source_file: &Path, lines: &[&str], start: usize) -> Vec<LineParse> {
        let project_dir = source_file
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .map(percent_decode);
        let tool_defs_bytes =
            std::fs::metadata(source_file.with_file_name("tool_definitions.json"))
                .map(|meta| i64::try_from(meta.len()).unwrap_or(0))
                .unwrap_or(0);
        let turns = std::fs::read_to_string(source_file.with_file_name("chat_history.jsonl"))
            .map(|text| turn_request_weights(&text, tool_defs_bytes))
            .unwrap_or_default();
        let anchors = turn_anchors(self, lines);
        // 第 i 个可入账的 turn_completed ↔ 第 i 个非空回合：无应答的空回合
        //（用户消息后没有 assistant）不会产生 turn_completed，配对时跳过。
        let turns: Vec<&[RequestWeights]> = turns
            .iter()
            .filter(|turn| !turn.is_empty())
            .map(Vec::as_slice)
            .collect();
        let mut ordinal = 0usize;
        let mut out = Vec::with_capacity(lines.len().saturating_sub(start));
        for (i, line) in lines.iter().enumerate() {
            let parsed = match self.parse_line(line) {
                LineParse::Facts(facts) => {
                    let index = ordinal;
                    ordinal += 1;
                    if i < start {
                        // 游标前的行只推进配对序号：事实已在早前扫描入账，
                        // 重复产出会被 dedup 拦住，但白做功还拖慢扫描。
                        continue;
                    }
                    let mut facts = *facts;
                    facts.project_dir = project_dir.clone();
                    split_turn(facts, turns.get(index).copied(), anchors.get(index))
                }
                other => other,
            };
            out.push(parsed);
        }
        out
    }

    /// 登记源 updates.jsonl 与转录同处一个会话目录（`is_session_log` 只认
    /// updates.jsonl），按路径取同目录的 chat_history.jsonl，整文件即该会话。
    fn read_transcript(
        &self,
        source_file: &Path,
        _external_id: &str,
    ) -> Result<Vec<TranscriptEntry>> {
        let text = transcript::read_text(&source_file.with_file_name("chat_history.jsonl"))?;
        let mut entries = Vec::new();
        for line in text.lines() {
            let Ok(raw) = serde_json::from_str::<ChatLine>(line) else {
                continue;
            };
            match raw.kind.as_str() {
                // 通用 Grok 系统提示，各会话相同，无展示价值。
                "system" => {}
                "user" => {
                    for entry in user_entries(&raw) {
                        entries.push(entry);
                    }
                }
                "assistant" => {
                    let mut blocks = raw
                        .content
                        .as_ref()
                        .map(transcript::blocks_from_content)
                        .unwrap_or_default();
                    blocks.extend(raw.tool_calls.iter().flatten().map(|call| {
                        TranscriptBlock::ToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.as_ref().map(transcript::arguments_text),
                        }
                    }));
                    if blocks.is_empty() {
                        continue;
                    }
                    entries.push(TranscriptEntry {
                        role: TranscriptRole::Assistant,
                        ts_ms: None,
                        model: raw.model_id.filter(|m| !m.trim().is_empty()),
                        blocks,
                    });
                }
                "tool_result" => {
                    let content = raw
                        .content
                        .as_ref()
                        .map(transcript::value_text)
                        .unwrap_or_default();
                    if content.is_empty() {
                        continue;
                    }
                    entries.push(TranscriptEntry {
                        role: TranscriptRole::Tool,
                        ts_ms: None,
                        model: None,
                        blocks: vec![TranscriptBlock::ToolResult {
                            call_id: raw.tool_call_id.clone(),
                            content,
                            is_error: false,
                        }],
                    });
                }
                _ => {}
            }
        }
        Ok(absorb_standalone_reminders(entries))
    }
}

/// URL 百分号解码（会话目录名是编码后的 cwd）。`+` 在路径编码里是字面
/// 加号不转空格；非法转义原样保留，非 UTF8 序列按替换符兜底。
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Some(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16).ok()
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 一次请求的分摊权重与时刻锚点（由 chat_history 的 assistant 行派生）。
struct RequestWeights {
    /// 请求发出时的上下文规模：tool_definitions 字节 + chat_history 里该
    /// assistant 行之前的全部行字节（与"发给模型的上下文"同向增长）。
    context: i64,
    /// 请求产出的 assistant 行字节（输出/推理的分摊权重）。
    output: i64,
    /// assistant 消息携带的 tool_call id：对回 updates.jsonl 的事件锚定时刻。
    call_ids: Vec<String>,
}

/// chat_history 分段出每回合的请求权重。回合 = 非合成 user 行开启，请求 =
/// assistant 行；权重单位是行的原始 JSON 字节——比例估计只要求口径一致。
/// 坏行跳过。
fn turn_request_weights(history: &str, tool_defs_bytes: i64) -> Vec<Vec<RequestWeights>> {
    let mut turns: Vec<Vec<RequestWeights>> = Vec::new();
    let mut context = tool_defs_bytes.max(0);
    for line in history.lines() {
        let Ok(raw) = serde_json::from_str::<ChatLine>(line) else {
            continue;
        };
        let bytes = i64::try_from(line.len()).unwrap_or(i64::MAX);
        if raw.kind == "assistant" {
            let call_ids = raw
                .tool_calls
                .iter()
                .flatten()
                .filter_map(|call| call.id.clone())
                .filter(|id| !id.trim().is_empty())
                .collect();
            let weights = RequestWeights {
                context,
                output: bytes,
                call_ids,
            };
            match turns.last_mut() {
                Some(turn) => turn.push(weights),
                // assistant 先于任何 user 行（反常布局）：开一个合成回合接住。
                None => {
                    turns.push(vec![weights]);
                }
            }
            context += bytes;
        } else {
            // 非合成 user 行开启新回合；其余行（system、tool_result、合成
            // reminder）只是上下文的一部分。
            if raw.kind == "user"
                && raw
                    .synthetic_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                turns.push(Vec::new());
            }
            context += bytes;
        }
    }
    turns
}

/// updates.jsonl 事件的最小投影：只取耗时推导需要的字段（事件种类、调用
/// id、时刻、回合墙钟），其余字段（含可能带密钥的事件）不落任何结构。
#[derive(Deserialize)]
struct EventRef {
    /// epoch 秒；`_meta.agentTimestampMs` 缺失时的兜底时刻。
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    params: Option<EventParams>,
}

#[derive(Deserialize)]
struct EventParams {
    #[serde(rename = "_meta", default)]
    meta: Option<EventMeta>,
    #[serde(default)]
    update: Option<EventUpdate>,
}

#[derive(Deserialize)]
struct EventMeta {
    /// 事件墙钟（epoch ms）。
    #[serde(rename = "agentTimestampMs", default)]
    agent_timestamp_ms: Option<i64>,
}

#[derive(Deserialize)]
struct EventUpdate {
    #[serde(rename = "sessionUpdate", default)]
    session_update: Option<String>,
    #[serde(rename = "toolCallId", default)]
    tool_call_id: Option<String>,
    /// 回合墙钟（毫秒），仅 turn_completed 携带。
    #[serde(default)]
    elapsed_ms: Option<i64>,
}

/// 一次回合的事件锚点（updates.jsonl 按 user_message_chunk … turn_completed
/// 分段）。耗时推导用的真实时间戳全在这里：回合起点、每请求的响应完成、
/// 工具执行完成、回合终点。
#[derive(Default)]
struct TurnAnchors {
    /// user_message_chunk 的毫秒时刻（回合起点 = 首个 API 请求的起点）。
    user_ts: Option<i64>,
    /// tool_call 事件：id → 最早事件时刻（该请求响应完成的锚点）。
    call_ts: HashMap<String, i64>,
    /// tool_call_update 事件：id → 最新事件时刻（工具执行完成的锚点，
    /// 即下一请求的起点）。
    update_ts: HashMap<String, i64>,
    /// turn_completed 的毫秒时刻（末请求的终点锚点）。
    completed_ts: Option<i64>,
    /// turn_completed 的 elapsed_ms（回合墙钟，回退单行的耗时）。
    elapsed_ms: Option<i64>,
}

/// updates.jsonl → 每个可入账回合的锚点，顺序与 parse_file_at 的事实序号
/// 一致：只有产出 Facts 的 turn_completed 才闭合并占一个锚点位；没等到
/// turn_completed 的中断回合整段丢弃。同一调用的 tool_call 取最早（响应
/// 完成）、tool_call_update 取最晚（执行完成）。
fn turn_anchors(adapter: &GrokAdapter, lines: &[&str]) -> Vec<TurnAnchors> {
    let mut out: Vec<TurnAnchors> = Vec::new();
    let mut current = TurnAnchors::default();
    let mut open = false;
    for line in lines {
        let Ok(raw) = serde_json::from_str::<EventRef>(line) else {
            continue;
        };
        let Some(params) = raw.params else {
            continue;
        };
        let Some(update) = params.update else {
            continue;
        };
        let ts = params
            .meta
            .and_then(|meta| meta.agent_timestamp_ms)
            .or_else(|| {
                raw.timestamp
                    .filter(|&ts| ts > 0)
                    .map(|ts| ts.saturating_mul(1_000))
            });
        match update.session_update.as_deref() {
            Some("user_message_chunk") => {
                current = TurnAnchors {
                    user_ts: ts,
                    ..TurnAnchors::default()
                };
                open = true;
            }
            Some("tool_call") => {
                if open
                    && let Some(ts) = ts
                    && let Some(id) = update.tool_call_id.filter(|id| !id.trim().is_empty())
                {
                    current.call_ts.entry(id).or_insert(ts);
                }
            }
            Some("tool_call_update") => {
                if open
                    && let Some(ts) = ts
                    && let Some(id) = update.tool_call_id.filter(|id| !id.trim().is_empty())
                {
                    current.update_ts.insert(id, ts);
                }
            }
            Some("turn_completed") => {
                current.completed_ts = ts;
                current.elapsed_ms = update.elapsed_ms.filter(|&ms| ms > 0);
                // 与事实序号对齐：入不了账的回合（无用量等）不占锚点位。
                if matches!(adapter.parse_line(line), LineParse::Facts(_)) {
                    out.push(current);
                }
                current = TurnAnchors::default();
                open = false;
            }
            _ => {}
        }
    }
    out
}

/// 按权重把 total 整数分摊到各行：floor 之后余数全归末行，各桶 Σ 严格等于
/// 聚合值。权重全零（如首请求的缓存读）时全部落末行——退化为"保持原值"。
fn allocate(total: i64, weights: &[i64]) -> Vec<i64> {
    let mut out = vec![0i64; weights.len()];
    let sum: i64 = weights.iter().sum();
    if sum > 0 {
        for (slot, weight) in out.iter_mut().zip(weights) {
            *slot = total * weight / sum;
        }
    }
    let remainder = total - out.iter().sum::<i64>();
    if let Some(last) = out.last_mut() {
        *last += remainder;
    }
    out
}

/// 把回合聚合事实拆成请求级行（见模块文档"请求级拆分"）。分摊比例：净输入
/// 与缓存写 ∝ 上下文增长（本次请求比上次多出的内容）、缓存读 ∝ 上一请求的
/// 上下文（首请求没有前置缓存）、输出与推理 ∝ assistant 行字节；互锁：回合
/// 的 assistant 数必须等于 modelCalls 之和，否则整行回退。请求时刻：assistant
/// 的 tool_call 事件锚定，缺事件的后继行向前继承，末行恒为 turn_completed
/// 的行时刻。耗时：真实时间戳推导（见 `derive_durations`），回退单行用
/// elapsed_ms。回合缺失 / 单请求 / 上下文回退（历史被重写，比例失真）→ 原样。
fn split_turn(
    facts: LineFacts,
    turn: Option<&[RequestWeights]>,
    anchors: Option<&TurnAnchors>,
) -> LineParse {
    // 回退单行的耗时：elapsed_ms 是回合墙钟的真实值（单请求回合 ≈ API 耗时；
    // 互锁失败的回合含工具执行，上限口径）。
    let fallback_duration = anchors.and_then(|a| a.elapsed_ms);
    let single = |mut facts: LineFacts| {
        if let Some(usage) = facts.usage.as_mut() {
            usage.duration_ms = fallback_duration;
        }
        LineParse::Facts(Box::new(facts))
    };
    let Some(turn) = turn else {
        return single(facts);
    };
    let Some(usage) = facts.usage.as_ref() else {
        return single(facts);
    };
    let n = turn.len();
    if n <= 1 || facts.request_count != n as i64 {
        return single(facts);
    }
    let contexts: Vec<i64> = turn.iter().map(|w| w.context).collect();
    if contexts.windows(2).any(|pair| pair[0] > pair[1]) {
        return single(facts);
    }
    let growth: Vec<i64> = (0..n)
        .map(|k| contexts[k] - if k == 0 { 0 } else { contexts[k - 1] })
        .collect();
    let cached: Vec<i64> = (0..n)
        .map(|k| if k == 0 { 0 } else { contexts[k - 1] })
        .collect();
    let output: Vec<i64> = turn.iter().map(|w| w.output).collect();

    let input = allocate(usage.usage.input_tokens, &growth);
    let cache_read = allocate(usage.usage.cache_read_tokens, &cached);
    let cache_write = allocate(usage.usage.cache_write_tokens, &growth);
    let output_tokens = allocate(usage.usage.output_tokens, &output);
    let reasoning = usage
        .usage
        .reasoning_tokens
        .map(|total| allocate(total, &output));

    let turn_ts = usage.ts_ms;
    let mut ts: Vec<Option<i64>> = turn
        .iter()
        .map(|w| {
            w.call_ids
                .iter()
                .find_map(|id| anchors.and_then(|a| a.call_ts.get(id)).copied())
        })
        .collect();
    ts[n - 1] = anchors.and_then(|a| a.completed_ts).or(Some(turn_ts));
    for k in (0..n - 1).rev() {
        if ts[k].is_none() {
            ts[k] = ts[k + 1];
        }
    }

    // 请求 k 的耗时（真实时间戳推导，非估算）：起点 = 上一请求全部工具
    // 执行完成（最后一条 tool_call_update；首请求用 user_message 时刻），
    // 终点 = 本请求响应完成（tool_call 最早事件；末请求用 turn_completed
    // 时刻）。工具执行时间落在区间之间，不计入任何请求——Σ 耗时 ≤
    // elapsed_ms 的差额即工具执行。锚点缺失或区间非正 → 留空，绝不编造。
    let duration = |k: usize| -> Option<i64> {
        let start = if k == 0 {
            anchors.and_then(|a| a.user_ts)
        } else {
            turn[k - 1]
                .call_ids
                .iter()
                .filter_map(|id| anchors.and_then(|a| a.update_ts.get(id)).copied())
                .max()
        };
        let end = if k + 1 == n {
            anchors.and_then(|a| a.completed_ts).or(Some(turn_ts))
        } else {
            turn[k]
                .call_ids
                .iter()
                .filter_map(|id| anchors.and_then(|a| a.call_ts.get(id)).copied())
                .min()
        };
        match (start, end) {
            (Some(start), Some(end)) if end > start => Some(end - start),
            _ => None,
        }
    };

    LineParse::Split(
        (0..n)
            .map(|k| {
                let ts_k = ts[k].unwrap_or(turn_ts);
                LineFacts {
                    dedup_suffix: Some(format!("r{k}")),
                    request_count: 1,
                    session_external_id: facts.session_external_id.clone(),
                    title: None,
                    project_dir: facts.project_dir.clone(),
                    ts_ms: Some(ts_k),
                    usage: Some(UsageFacts {
                        ts_ms: ts_k,
                        model_raw: usage.model_raw.clone(),
                        model: usage.model.clone(),
                        duration_ms: duration(k),
                        usage: TokenUsage {
                            input_tokens: input[k],
                            output_tokens: output_tokens[k],
                            cache_read_tokens: cache_read[k],
                            cache_write_tokens: cache_write[k],
                            reasoning_tokens: reasoning.as_ref().map(|rows| rows[k]),
                        },
                    }),
                }
            })
            .collect(),
    )
}

/// 全零用量无事实价值，跳过；时间戳缺失的用量不落库（schema 非空，绝不编造）。
fn usage_facts(u: &ModelUsage, model_raw: &str, ts_ms: Option<i64>) -> Option<UsageFacts> {
    let model_raw = model_raw.trim();
    if model_raw.is_empty() {
        return None;
    }
    // input 含缓存读、output 含推理（依据见模块文档）：入库前拆出净量。
    let cache_read = u.cached_read_tokens.max(0);
    let reasoning = u.reasoning_tokens.filter(|&n| n > 0);
    let usage = TokenUsage {
        input_tokens: (u.input_tokens.max(0) - cache_read).max(0),
        output_tokens: (u.output_tokens.max(0) - reasoning.unwrap_or(0)).max(0),
        cache_read_tokens: cache_read,
        // cacheCreationTokens 语义无法从本机数据验证（恒为 0），按独立桶直记，
        // 不从 input 里扣——观察值为零，暂无计价风险。
        cache_write_tokens: u.cache_creation_tokens.max(0),
        reasoning_tokens: reasoning,
    };
    if usage.input_tokens == 0 && usage.output_tokens == 0 && usage.cache_read_tokens == 0 {
        return None;
    }
    Some(UsageFacts {
        ts_ms: ts_ms?,
        model_raw: model_raw.to_string(),
        model: model_raw.to_string(),
        duration_ms: None,
        usage,
    })
}

/// chat_history.jsonl 的一行（转录路径专用，与 updates.jsonl 的 RawLine 无关）。
/// 行内无 sessionId（归属靠目录）、无时间戳（ts 留空，见模块文档）。
#[derive(Deserialize)]
struct ChatLine {
    #[serde(rename = "type", default)]
    kind: String,
    /// user 是块数组、assistant 是字符串、tool_result 是字符串或块数组，
    /// 统一按值处理（摊平走 [`transcript::blocks_from_content`] /
    /// [`transcript::value_text`]）。
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatToolCall>>,
    #[serde(rename = "tool_call_id", default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    model_id: Option<String>,
    /// 非空 = 整行是 harness 注入（system_reminder 等），不是用户说的话。
    #[serde(default)]
    synthetic_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// JSON 字符串形态的参数原文。
    #[serde(default)]
    arguments: Option<serde_json::Value>,
}

/// user 行 → 转录条目（一至两条）。`synthetic_reason` 非空按纯注入建模
/// （单 Injected 块，占比条满格）；否则文本块走注入/信封/提问分段，会话级
/// 信封拆成前置独立条目——展示为中性卡而非用户气泡里的占比条（与 codex 的
/// environment_context 同一呈现）。信封 harness 都缀在提问之前，拆分顺序
/// 即原文顺序。
fn user_entries(raw: &ChatLine) -> Vec<TranscriptEntry> {
    let entry = |blocks: Vec<TranscriptBlock>| TranscriptEntry {
        role: TranscriptRole::User,
        ts_ms: None,
        model: None,
        blocks,
    };
    if raw
        .synthetic_reason
        .as_deref()
        .is_some_and(|reason| !reason.trim().is_empty())
    {
        let text = raw
            .content
            .as_ref()
            .map(transcript::value_text)
            .unwrap_or_default();
        if text.is_empty() {
            return vec![];
        }
        let injected_chars = text.chars().count() as i64;
        return vec![entry(vec![TranscriptBlock::Injected {
            text,
            injected_chars,
        }])];
    }
    let blocks: Vec<TranscriptBlock> = raw
        .content
        .as_ref()
        .map(transcript::blocks_from_content)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|block| match block {
            TranscriptBlock::Text { text } => segment_user_text(&text),
            kept => vec![kept],
        })
        .collect();
    let (harness, rest): (Vec<_>, Vec<_>) = blocks
        .into_iter()
        .partition(|block| matches!(block, TranscriptBlock::HarnessContext { .. }));
    let mut entries = Vec::with_capacity(2);
    if !harness.is_empty() {
        entries.push(entry(harness));
    }
    if !rest.is_empty() {
        entries.push(entry(rest));
    }
    entries
}

/// 独立合成 reminder 行不单挂——吸收进相邻用户回合的气泡（与 workbuddy
/// 的行内 reminder 同一呈现）。时序落位：有前序回合归前序（事件发生在该
/// 回合内，模型应答时已看到）；会话开头没有前序回合时归最近的后继回合；
/// 两头都没有才保持独立。搬动的是 Injected 块，占比条由前端按块求和。
fn absorb_standalone_reminders(mut entries: Vec<TranscriptEntry>) -> Vec<TranscriptEntry> {
    fn is_reminder(entry: &TranscriptEntry) -> bool {
        entry.role == TranscriptRole::User
            && entry.blocks.len() == 1
            && matches!(entry.blocks[0], TranscriptBlock::Injected { .. })
    }
    fn is_turn(entry: &TranscriptEntry) -> bool {
        entry.role == TranscriptRole::User
            && entry.blocks.iter().any(|block| {
                matches!(
                    block,
                    TranscriptBlock::Text { .. } | TranscriptBlock::Image { .. }
                )
            })
    }
    let len = entries.len();
    let mut moved: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    // target 下标 → 按原顺序收集的注入块。
    let mut extra: std::collections::BTreeMap<usize, Vec<TranscriptBlock>> =
        std::collections::BTreeMap::new();
    for i in 0..len {
        if !is_reminder(&entries[i]) {
            continue;
        }
        let target = (0..i)
            .rev()
            .find(|&j| is_turn(&entries[j]))
            .or_else(|| (i + 1..len).find(|&j| is_turn(&entries[j])));
        if let Some(j) = target {
            moved.insert(i);
            extra.entry(j).or_default().append(&mut entries[i].blocks);
        }
    }
    if moved.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .enumerate()
        .filter_map(|(i, mut entry)| {
            if let Some(blocks) = extra.remove(&i) {
                entry.blocks.extend(blocks);
            }
            (!moved.contains(&i)).then_some(entry)
        })
        .collect()
}

/// user 文本的展示分段。顶层信封按标签三分流：`<user_query>` 是提问
/// （Text 块）；`<system-reminder>` 是事件广播风格的注入（Injected 块，
/// 携带整条原文）；其余信封（`<user_info>`/`<rules>`/`<git_status>` 等
/// 会话级上下文，穷举清单打不完）各归一个 [`TranscriptBlock::HarnessContext`]
/// ——不再冒充用户正文，也不挤占比条。信封之外的自由文字在无
/// `<user_query>` 时是用户说的话。纯信封消息只产信封块。
fn segment_user_text(text: &str) -> Vec<TranscriptBlock> {
    let mut blocks = Vec::new();
    let mut reminder_chars = 0i64;
    let mut query: Option<String> = None;
    let mut query_envelope = false;
    let mut free = String::new();
    for piece in top_level_pieces(text) {
        match piece {
            TextPiece::Free(part) => free.push_str(part),
            TextPiece::Envelope { tag, text: span } => match tag {
                "user_query" => {
                    // grok 的提问标签体内带换行（`<user_query>\n…\n</user_query>`，
                    // 实测口径），正文取 trim 后的——原文里的换行留给完整口径
                    // 的原文树，纯净正文不带空行。
                    let body = span
                        .split_once('>')
                        .map(|(_, body)| body)
                        .and_then(|body| body.strip_suffix("</user_query>"))
                        .unwrap_or_default()
                        .trim();
                    query.get_or_insert(body.to_string());
                    query_envelope = true;
                }
                "system-reminder" => reminder_chars += span.chars().count() as i64,
                _ => blocks.push(TranscriptBlock::HarnessContext {
                    tag: tag.to_string(),
                    text: span.to_string(),
                }),
            },
        }
    }
    let free = free.trim();
    // 有 <user_query> 信封就产 Injected 块（注入字符可为 0）：独立 reminder
    // 吸收进气泡后，原文树要像 workbuddy 一样含 <user_query> 元素；占比条
    // 由前端按注入字符总数显隐，0 字符块不改变总量也不显条。
    if reminder_chars > 0 || query_envelope {
        blocks.push(TranscriptBlock::Injected {
            text: text.to_string(),
            injected_chars: reminder_chars,
        });
    }
    match query.filter(|query| !query.trim().is_empty()) {
        Some(query) => blocks.push(TranscriptBlock::Text { text: query }),
        None if !free.is_empty() => blocks.push(TranscriptBlock::Text {
            text: free.to_string(),
        }),
        None => {}
    }
    blocks
}

#[derive(Deserialize)]
struct RawLine {
    #[serde(default)]
    method: Option<String>,
    /// epoch 秒（不是毫秒）。
    #[serde(default)]
    timestamp: Option<i64>,
    #[serde(default)]
    params: Option<RawParams>,
}

#[derive(Deserialize)]
struct RawParams {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    update: Option<RawUpdate>,
}

#[derive(Deserialize)]
struct RawUpdate {
    #[serde(rename = "sessionUpdate")]
    session_update: Option<String>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

/// 回合用量：只取按模型分解的部分——top-level 聚合是同样的数字，不需要第二份。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawUsage {
    #[serde(default)]
    model_usage: Option<std::collections::BTreeMap<String, ModelUsage>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cached_read_tokens: i64,
    #[serde(rename = "cacheCreationTokens", default)]
    cache_creation_tokens: i64,
    #[serde(default)]
    reasoning_tokens: Option<i64>,
    #[serde(default)]
    total_tokens: i64,
    /// 该回合内此模型的真实网络请求数——请求数的权威来源（token 是回合聚合，
    /// 调用次数只有这里给）。
    #[serde(default)]
    model_calls: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURN: &str = r#"{"timestamp":1789204709,"method":"_x.ai/session/update","params":{"sessionId":"01a094e9-06d7","update":{"sessionUpdate":"turn_completed","prompt_id":"1377dc75","stop_reason":"end_turn","usage":{"inputTokens":30928,"outputTokens":821,"totalTokens":31749,"cachedReadTokens":13312,"cacheCreationTokens":0,"reasoningTokens":139,"modelCalls":2,"modelUsage":{"deepseek/deepseek-v4-flash-0731":{"inputTokens":30928,"outputTokens":821,"totalTokens":31749,"cachedReadTokens":13312,"cacheCreationTokens":0,"reasoningTokens":139,"modelCalls":2}}},"numTurns":2}}}"#;

    #[test]
    fn turn_completed_line_yields_usage_with_cache_split() {
        let facts = match GrokAdapter.parse_line(TURN) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.session_external_id, "01a094e9-06d7");
        assert_eq!(
            facts.title, None,
            "chat_history 的 user 行无 sessionId，标题不做"
        );
        assert_eq!(facts.ts_ms, Some(1_789_204_709_000), "行时间戳是秒，转毫秒");

        let usage = facts.usage.expect("turn_completed 必须有用量");
        assert_eq!(usage.model_raw, "deepseek/deepseek-v4-flash-0731");
        assert_eq!(usage.usage.input_tokens, 17_616, "净输入 = 30928 − 13312");
        assert_eq!(usage.usage.cache_read_tokens, 13_312);
        assert_eq!(usage.usage.output_tokens, 682, "净输出 = 821 − 139");
        assert_eq!(usage.usage.reasoning_tokens, Some(139));
        assert_eq!(usage.usage.cache_write_tokens, 0);
        assert_eq!(
            facts.request_count, 2,
            "请求数 = modelCalls（回合聚合，行级无法再细）"
        );
    }

    #[test]
    fn non_update_events_are_skipped() {
        // MCP 事件行（可能含密钥 URL）：不是 update 通知，跳过且不产出任何字段。
        let mcp = r#"{"ts":"2026-09-12T09:16:41.498Z","type":"mcp_server_starting","server_name":"tavily"}"#;
        assert_eq!(GrokAdapter.parse_line(mcp), LineParse::Skip);
        // 非回合完成的 update。
        let other = r#"{"timestamp":1789204709,"method":"_x.ai/session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_delta"}}}"#;
        assert_eq!(GrokAdapter.parse_line(other), LineParse::Skip);
        // 无 sessionId。
        assert_eq!(
            GrokAdapter.parse_line(
                r#"{"timestamp":1,"method":"_x.ai/session/update","params":{"update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":5,"outputTokens":1}}}}"#
            ),
            LineParse::Skip
        );
        assert_eq!(GrokAdapter.parse_line("{ not json"), LineParse::Malformed);
    }

    /// 项目目录派生：会话目录名是 URL 编码的 cwd，解码后随事实入账
    /// （`%5C` 还原反斜杠、`%20` 还原空格）；Skip 行不受影响。
    #[test]
    fn parse_file_at_decodes_project_dir_from_session_dir_name() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source = dir
            .join("E%3A%5CDev%5CDemo%20App")
            .join("01a094e9-06d7")
            .join("updates.jsonl");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();

        let parses = GrokAdapter.parse_file_at(&source, &[TURN, "{ not json"], 0);
        let facts = match &parses[0] {
            LineParse::Facts(facts) => facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.project_dir.as_deref(), Some(r"E:\Dev\Demo App"));
        assert_eq!(
            facts.request_count, 2,
            "chat_history 缺失 → 整轮回退单行，请求数保持 modelCalls"
        );
        assert!(matches!(parses[1], LineParse::Malformed));

        // 目录名不编码（异常布局）：解码按字面透传，不 panic、不给假路径。
        let plain = dir.join("weird-name").join("s1").join("updates.jsonl");
        std::fs::create_dir_all(plain.parent().unwrap()).unwrap();
        let facts = match GrokAdapter.parse_file_at(&plain, &[TURN], 0).remove(0) {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.project_dir.as_deref(), Some("weird-name"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zero_usage_is_skipped() {
        let zero = r#"{"timestamp":1789204709,"method":"_x.ai/session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":0,"outputTokens":0,"totalTokens":0}}}}"#;
        assert_eq!(GrokAdapter.parse_line(zero), LineParse::Skip);
    }

    /// 请求级拆分的夹具回合：净输入 600、净输出 200、缓存读 400、推理 100，
    /// modelCalls 2，回合墙钟 15000ms。
    const SPLIT_TURN: &str = r#"{"timestamp":1789204709,"method":"_x.ai/session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":1000,"outputTokens":300,"totalTokens":1300,"cachedReadTokens":400,"cacheCreationTokens":0,"reasoningTokens":100,"modelCalls":2,"modelUsage":{"m":{"inputTokens":1000,"outputTokens":300,"totalTokens":1300,"cachedReadTokens":400,"cacheCreationTokens":0,"reasoningTokens":100,"modelCalls":2}}},"elapsed_ms":15000}}}"#;
    const USER_EVENT: &str = r#"{"timestamp":1789204699,"method":"_x.ai/session/update","params":{"sessionId":"s1","_meta":{"agentTimestampMs":1789204699000},"update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"帮我看目录"}}}}"#;
    const CALL_EVENT: &str = r#"{"timestamp":1789204700,"method":"_x.ai/session/update","params":{"sessionId":"s1","_meta":{"agentTimestampMs":1789204700500},"update":{"sessionUpdate":"tool_call","toolCallId":"call_1"}}}"#;
    const UPDATE_EVENT: &str = r#"{"timestamp":1789204700,"method":"_x.ai/session/update","params":{"sessionId":"s1","_meta":{"agentTimestampMs":1789204700800},"update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed"}}}"#;
    const SPLIT_USER: &str = r#"{"type":"user","content":"帮我看目录"}"#;
    const SPLIT_ASSISTANT1: &str = r#"{"type":"assistant","content":"先列目录。","tool_calls":[{"id":"call_1","name":"list_dir","arguments":"{}"}]}"#;
    const SPLIT_TOOL_RESULT: &str =
        r#"{"type":"tool_result","tool_call_id":"call_1","content":"- src"}"#;
    const SPLIT_ASSISTANT2: &str = r#"{"type":"assistant","content":"目录在 src。"}"#;

    /// 互锁成立（assistant 数 = modelCalls）：回合聚合拆成每请求一行，各桶
    /// Σ 严格等于聚合值；缓存读首请求为 0；时刻与耗时由事件锚点推导。
    #[test]
    fn turn_completed_splits_into_request_level_rows() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("updates.jsonl");
        std::fs::write(
            &source,
            [USER_EVENT, CALL_EVENT, UPDATE_EVENT, SPLIT_TURN].join("\n"),
        )
        .unwrap();
        std::fs::write(
            dir.join("chat_history.jsonl"),
            [
                SPLIT_USER,
                SPLIT_ASSISTANT1,
                SPLIT_TOOL_RESULT,
                SPLIT_ASSISTANT2,
            ]
            .join("\n"),
        )
        .unwrap();

        let lines = [USER_EVENT, CALL_EVENT, UPDATE_EVENT, SPLIT_TURN];
        let parses = GrokAdapter.parse_file_at(&source, &lines, 0);
        assert_eq!(parses.len(), 4);
        assert!(matches!(parses[0], LineParse::Skip));
        assert!(matches!(parses[1], LineParse::Skip), "tool_call 事件不入账");
        assert!(matches!(parses[2], LineParse::Skip));
        let LineParse::Split(rows) = &parses[3] else {
            panic!("期望 Split，实际 {:?}", parses[3]);
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].dedup_suffix.as_deref(), Some("r0"));
        assert_eq!(rows[1].dedup_suffix.as_deref(), Some("r1"));
        assert!(rows.iter().all(|row| row.request_count == 1));

        fn tokens(row: &LineFacts) -> &TokenUsage {
            &row.usage.as_ref().unwrap().usage
        }
        assert_eq!(
            tokens(&rows[0]).input_tokens + tokens(&rows[1]).input_tokens,
            600,
            "净输入 Σ = 聚合值"
        );
        assert_eq!(
            tokens(&rows[0]).output_tokens + tokens(&rows[1]).output_tokens,
            200,
            "净输出 Σ = 聚合值"
        );
        assert_eq!(
            tokens(&rows[0]).reasoning_tokens.unwrap() + tokens(&rows[1]).reasoning_tokens.unwrap(),
            100
        );
        assert_eq!(
            tokens(&rows[0]).cache_read_tokens,
            0,
            "首请求没有前置缓存可读"
        );
        assert_eq!(tokens(&rows[1]).cache_read_tokens, 400);
        // 分摊比例与口径一致：净输入 ∝ 上下文增长。
        let growth = [
            SPLIT_USER.len() as i64,
            (SPLIT_ASSISTANT1.len() + SPLIT_TOOL_RESULT.len()) as i64,
        ];
        let input = allocate(600, &growth);
        assert_eq!(tokens(&rows[0]).input_tokens, input[0]);
        assert_eq!(tokens(&rows[1]).input_tokens, input[1]);

        assert_eq!(
            rows[0].usage.as_ref().unwrap().ts_ms,
            1_789_204_700_500,
            "请求时刻 = assistant 的 tool_call 事件墙钟"
        );
        assert_eq!(
            rows[1].usage.as_ref().unwrap().ts_ms,
            1_789_204_709_000,
            "末请求时刻 = turn_completed 行时刻"
        );

        // 耗时推导：首请求起点 = user_message 时刻；后继请求起点 = 上一请求
        // 的最后一条 tool_call_update（工具执行完毕）。Σ ≤ 回合墙钟，差额
        // 即被剔除的工具执行时间。
        let duration = |row: &LineFacts| row.usage.as_ref().unwrap().duration_ms;
        assert_eq!(duration(&rows[0]), Some(1_500), "700500 − user@699000");
        assert_eq!(
            duration(&rows[1]),
            Some(8_200),
            "turn_completed@709000 − update@700800"
        );
        assert!(
            duration(&rows[0]).unwrap() + duration(&rows[1]).unwrap() <= 15_000,
            "推导耗时 Σ ≤ elapsed_ms"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 互锁失败（assistant 数 ≠ modelCalls）：整行回退单行，请求数保持
    /// modelCalls，耗时取 elapsed_ms，不做比例分摊。
    #[test]
    fn assistant_count_mismatch_falls_back_to_single_row() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-mismatch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("updates.jsonl");
        std::fs::write(&source, SPLIT_TURN).unwrap();
        std::fs::write(
            dir.join("chat_history.jsonl"),
            [SPLIT_USER, SPLIT_ASSISTANT1].join("\n"),
        )
        .unwrap();

        let facts = match GrokAdapter
            .parse_file_at(&source, &[SPLIT_TURN], 0)
            .remove(0)
        {
            LineParse::Facts(facts) => *facts,
            other => panic!("期望 Facts，实际 {other:?}"),
        };
        assert_eq!(facts.request_count, 2);
        assert_eq!(facts.dedup_suffix, None);
        assert_eq!(
            facts.usage.as_ref().unwrap().usage.input_tokens,
            600,
            "回退行保持聚合原值"
        );
        assert_eq!(
            facts.usage.as_ref().unwrap().duration_ms,
            Some(15_000),
            "回退行耗时 = elapsed_ms"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 配对按回合序推进：两个 turn_completed 各配各的回合——首回合两个
    /// assistant 拆成两行，次回合单 assistant 保持单行。
    #[test]
    fn turn_completed_lines_pair_with_turns_in_order() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-pair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let second = r#"{"timestamp":1789204809,"method":"_x.ai/session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"turn_completed","usage":{"inputTokens":10,"outputTokens":5,"totalTokens":15,"cachedReadTokens":0,"cacheCreationTokens":0,"reasoningTokens":0,"modelCalls":1,"modelUsage":{"m":{"inputTokens":10,"outputTokens":5,"totalTokens":15,"cachedReadTokens":0,"cacheCreationTokens":0,"reasoningTokens":0,"modelCalls":1}}}}}}"#;
        let source = dir.join("updates.jsonl");
        std::fs::write(&source, [CALL_EVENT, SPLIT_TURN, second].join("\n")).unwrap();
        std::fs::write(
            dir.join("chat_history.jsonl"),
            [
                SPLIT_USER,
                SPLIT_ASSISTANT1,
                SPLIT_ASSISTANT2,
                SPLIT_USER,
                SPLIT_ASSISTANT1,
            ]
            .join("\n"),
        )
        .unwrap();

        let parses = GrokAdapter.parse_file_at(&source, &[CALL_EVENT, SPLIT_TURN, second], 0);
        assert_eq!(parses.len(), 3);
        assert!(matches!(parses[0], LineParse::Skip));
        assert!(matches!(parses[1], LineParse::Split(_)), "首回合拆分");
        assert!(
            matches!(parses[2], LineParse::Facts(_)),
            "次回合单 assistant，保持单行"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_updates_jsonl_matches() {
        let adapter = GrokAdapter;
        assert!(adapter.is_session_log(Path::new("/s/01a094e7/updates.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/s/01a094e7/events.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/s/01a094e7/chat_history.jsonl")));
        assert!(!adapter.is_session_log(Path::new("/s/01a094e7/summary.json")));
    }

    /// 从 user 文本切出注入段与提问（依据见模块文档的实测口径）。
    const USER_INFO_SPAN: &str = "<user_info>\nOS: windows\n</user_info>";
    const RULES_SPAN: &str = "<rules>\nsome rules\n</rules>";
    const INJECTED_USER: &str = concat!(
        "<user_info>\nOS: windows\n</user_info>\n\n<rules>\nsome rules\n</rules>\n",
        "<user_query>查看项目内容</user_query>"
    );

    #[test]
    fn transcript_reads_chat_history_next_to_source() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-tr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("updates.jsonl");
        std::fs::write(&source, TURN).unwrap();
        std::fs::write(
            dir.join("chat_history.jsonl"),
            [
                // 系统提示行：各会话相同，跳过。
                r#"{"type":"system","content":"You are Grok released by xAI."}"#,
                // 会话级信封 + <user_query> 提问：信封拆成前置独立条目。
                r#"{"type":"user","content":[{"type":"text","text":"<user_info>\nOS: windows\n</user_info>\n\n<rules>\nsome rules\n</rules>\n<user_query>查看项目内容</user_query>"}]}"#,
                // synthetic 标记的整行注入：单 Injected 块。
                r#"{"type":"user","content":[{"type":"text","text":"<system-reminder>MCP servers connected</system-reminder>"}],"synthetic_reason":"system_reminder"}"#,
                // assistant：文本 + 工具调用 + 模型标签。
                r#"{"type":"assistant","content":"I'll look at the project structure.","tool_calls":[{"id":"call_1","name":"list_dir","arguments":"{\"target_directory\": \"D:/Work\"}"}],"model_id":"deepseek/deepseek-v4.1-flash","model_fingerprint":"fp"}"#,
                // 工具结果行。
                r#"{"type":"tool_result","tool_call_id":"call_1","content":"- D:/Work"}"#,
                // 裸 <user_query>（无注入）：正文即提问，不产信封块。
                r#"{"type":"user","content":[{"type":"text","text":"<user_query>裸提问</user_query>"}]}"#,
                "{ not json",
            ]
            .join("\n"),
        )
        .unwrap();

        let entries = GrokAdapter.read_transcript(&source, "01a094e7").unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0].role, TranscriptRole::User);
        assert_eq!(entries[0].ts_ms, None, "chat_history 无时间戳，留空");
        assert_eq!(
            entries[0].blocks,
            vec![
                TranscriptBlock::HarnessContext {
                    tag: "user_info".into(),
                    text: USER_INFO_SPAN.into(),
                },
                TranscriptBlock::HarnessContext {
                    tag: "rules".into(),
                    text: RULES_SPAN.into(),
                },
            ]
        );
        // 会话中段的独立合成 reminder：向前吸收进上一回合的气泡。首行信封 +
        // 提问同行，Injected 携带整行原文（注入字符 0，树素材）。
        assert_eq!(
            entries[1].blocks,
            vec![
                TranscriptBlock::Injected {
                    text: INJECTED_USER.into(),
                    injected_chars: 0,
                },
                TranscriptBlock::Text {
                    text: "查看项目内容".into()
                },
                TranscriptBlock::Injected {
                    text: "<system-reminder>MCP servers connected</system-reminder>".into(),
                    injected_chars: "<system-reminder>MCP servers connected</system-reminder>"
                        .chars()
                        .count() as i64,
                },
            ]
        );
        assert_eq!(entries[2].role, TranscriptRole::Assistant);
        assert_eq!(
            entries[2].model.as_deref(),
            Some("deepseek/deepseek-v4.1-flash")
        );
        assert_eq!(
            entries[2].blocks,
            vec![
                TranscriptBlock::Text {
                    text: "I'll look at the project structure.".into()
                },
                TranscriptBlock::ToolCall {
                    id: Some("call_1".into()),
                    name: Some("list_dir".into()),
                    arguments: Some(r#"{"target_directory": "D:/Work"}"#.into()),
                },
            ]
        );
        assert_eq!(entries[3].role, TranscriptRole::Tool);
        assert_eq!(
            entries[3].blocks,
            vec![TranscriptBlock::ToolResult {
                call_id: Some("call_1".into()),
                content: "- D:/Work".into(),
                is_error: false,
            }]
        );
        assert_eq!(entries[4].role, TranscriptRole::User);
        // 裸 <user_query> 也产 0 注入字符的 Injected 块（原文树素材），前端不显条。
        assert_eq!(
            entries[4].blocks,
            vec![
                TranscriptBlock::Injected {
                    text: "<user_query>裸提问</user_query>".into(),
                    injected_chars: 0,
                },
                TranscriptBlock::Text {
                    text: "裸提问".into()
                },
            ]
        );
    }

    #[test]
    fn standalone_reminders_absorb_by_temporal_position() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-absorb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("updates.jsonl");
        std::fs::write(&source, TURN).unwrap();
        let reminder = |text: &str| {
            format!(
                r#"{{"type":"user","content":[{{"type":"text","text":"<system-reminder>{text}</system-reminder>"}}],"synthetic_reason":"system_reminder"}}"#
            )
        };
        let query = |text: &str| {
            format!(
                r#"{{"type":"user","content":[{{"type":"text","text":"<user_query>\n{text}\n</user_query>"}}]}}"#
            )
        };
        std::fs::write(
            dir.join("chat_history.jsonl"),
            [
                // 会话开头、前面没有回合：向后兜底落到最近的后继回合。
                &reminder("workflows available"),
                &query("问题一"),
                r#"{"type":"assistant","content":"好"}"#,
                // 两个回合之间：仍归前序回合（时序上落在该回合活动期内）。
                &reminder("mcp connected"),
                &query("问题二"),
                // 会话末尾：归前序回合——两条 reminder 在同一气泡叠加。
                &reminder("session ending"),
            ]
            .join("\n"),
        )
        .unwrap();

        let entries = GrokAdapter.read_transcript(&source, "s1").unwrap();
        assert_eq!(entries.len(), 3);
        // query 信封行产 0 注入字符的 Injected 块（树里要有 <user_query> 元素），
        // 吸收的 reminder 块缀在后面。
        let query_envelope = |text: &str| TranscriptBlock::Injected {
            text: format!("<user_query>\n{text}\n</user_query>"),
            injected_chars: 0,
        };
        let reminder = |text: &str| TranscriptBlock::Injected {
            text: format!("<system-reminder>{text}</system-reminder>"),
            injected_chars: format!("<system-reminder>{text}</system-reminder>")
                .chars()
                .count() as i64,
        };
        assert_eq!(
            entries[0].blocks,
            vec![
                query_envelope("问题一"),
                TranscriptBlock::Text {
                    text: "问题一".into()
                },
                reminder("workflows available"),
                reminder("mcp connected"),
            ]
        );
        assert_eq!(entries[1].role, TranscriptRole::Assistant);
        assert_eq!(
            entries[2].blocks,
            vec![
                query_envelope("问题二"),
                TranscriptBlock::Text {
                    text: "问题二".into()
                },
                reminder("session ending"),
            ]
        );
    }

    #[test]
    fn transcript_missing_chat_history_is_data_file_error() {
        let dir = std::env::temp_dir().join(format!("toktol-grok-tr-miss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("updates.jsonl");
        std::fs::write(&source, TURN).unwrap();
        assert!(matches!(
            GrokAdapter.read_transcript(&source, "s1"),
            Err(crate::error::Error::DataFile { .. })
        ));
    }

    #[test]
    fn session_envelopes_become_harness_context_in_source_order() {
        // 真实会话首条 user 行的形态（合成值）：user_info + git_status +
        // rules、无 <user_query>。git_status 是未收录的信封——与已知信封
        // 一起按原文顺序各归 HarnessContext（回归：整段 XML 曾渲染成用户
        // 气泡，后来又曾并入 Injected 占比条）。
        let text = concat!(
            "<user_info>\nOS: windows\n</user_info>\n\n",
            "<git_status>\n M src/main.rs\n</git_status>\n",
            "<rules>\nsome rules\n</rules>"
        );
        assert_eq!(
            segment_user_text(text),
            vec![
                TranscriptBlock::HarnessContext {
                    tag: "user_info".into(),
                    text: "<user_info>\nOS: windows\n</user_info>".into(),
                },
                TranscriptBlock::HarnessContext {
                    tag: "git_status".into(),
                    text: "<git_status>\n M src/main.rs\n</git_status>".into(),
                },
                TranscriptBlock::HarnessContext {
                    tag: "rules".into(),
                    text: "<rules>\nsome rules\n</rules>".into(),
                },
            ]
        );
    }

    #[test]
    fn inline_system_reminder_stays_injected() {
        // 行内 <system-reminder>（带属性）是事件广播风格，仍走 Injected；
        // 占比条只对它负责，不再被会话级信封撑大。
        let text = concat!(
            "<system-reminder data-role=\"x\">r</system-reminder>\n",
            "<user_query>问题</user_query>"
        );
        assert_eq!(
            segment_user_text(text),
            vec![
                TranscriptBlock::Injected {
                    text: text.into(),
                    injected_chars: "<system-reminder data-role=\"x\">r</system-reminder>"
                        .chars()
                        .count() as i64,
                },
                TranscriptBlock::Text {
                    text: "问题".into()
                },
            ]
        );
    }

    #[test]
    fn envelope_scanner_keeps_free_text_and_prose() {
        // 信封之外的自由文字仍是用户的话；混合形态产 HarnessContext + Text。
        assert_eq!(
            segment_user_text("帮我看看"),
            vec![TranscriptBlock::Text {
                text: "帮我看看".into()
            }]
        );
        let mixed = "<git_status>\n M x\n</git_status> 帮我看看";
        assert_eq!(
            segment_user_text(mixed),
            vec![
                TranscriptBlock::HarnessContext {
                    tag: "git_status".into(),
                    text: "<git_status>\n M x\n</git_status>".into(),
                },
                TranscriptBlock::Text {
                    text: "帮我看看".into()
                },
            ]
        );
        // 裸信封（无已知注入标记）也归 HarnessContext——对未来的新信封标签免疫。
        let bare = "<environment_context>env</environment_context>";
        assert_eq!(
            segment_user_text(bare),
            vec![TranscriptBlock::HarnessContext {
                tag: "environment_context".into(),
                text: bare.into(),
            }]
        );
        // 未闭合的信封视为注入直到结尾。
        let unclosed = "<git_status>\n M x";
        assert_eq!(
            segment_user_text(unclosed),
            vec![TranscriptBlock::HarnessContext {
                tag: "git_status".into(),
                text: unclosed.into(),
            }]
        );
        // 散文里的 '<' 不是标签，不误伤。
        assert_eq!(
            segment_user_text("a < b 与 c > d"),
            vec![TranscriptBlock::Text {
                text: "a < b 与 c > d".into()
            }]
        );
    }
}
