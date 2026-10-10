//! Responses bridges for third-party upstreams. These conversions are local and
//! preserve the Codex-facing protocol independently of the supplier transport.

mod response;
mod stream;
#[cfg(test)]
mod tests;

pub(crate) use response::{convert_response, error_message};
pub(crate) use stream::ResponseStream;

use crate::error::{CodexxError, Result};
use base64::Engine;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamApi {
    Responses,
    ChatCompletions,
    AnthropicMessages,
    Gemini,
}

impl UpstreamApi {
    pub(crate) fn from_provider(value: Option<&str>, legacy: &str) -> Result<Self> {
        let value = value.filter(|v| !v.trim().is_empty()).unwrap_or(legacy);
        match value.trim().to_ascii_lowercase().as_str() {
            "responses" | "openai_responses" => Ok(Self::Responses),
            "chat" | "chat_completions" | "openai_chat" | "chat-completions" => Ok(Self::ChatCompletions),
            "anthropic" | "anthropic_messages" | "anthropic-messages" | "messages" | "claude" => Ok(Self::AnthropicMessages),
            "gemini" | "gemini_native" | "generate_content" => Ok(Self::Gemini),
            _ => Err(CodexxError::Config("未知的供应商上游协议，请选择 Responses、Chat Completions、Anthropic Messages 或 Gemini".into())),
        }
    }

    pub(crate) fn requires_routing(self) -> bool {
        self != Self::Responses
    }
}

#[derive(Debug, Clone)]
pub(super) struct ToolSpec {
    pub(super) name: String,
    pub(super) namespace: Option<String>,
    pub(super) custom: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolContext {
    pub(super) single_tool: bool,
    pub(super) names: HashMap<String, ToolSpec>,
    declarations: Vec<Value>,
}

impl ToolContext {
    fn upstream_name(&self, name: &str, namespace: Option<&str>) -> String {
        self.names
            .iter()
            .find(|(_, spec)| spec.name == name && spec.namespace.as_deref() == namespace)
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| name.to_owned())
    }

    fn add(&mut self, tool: &Value, namespace: Option<&str>) -> std::result::Result<(), String> {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("工具声明缺少名称")?;
        let custom = tool.get("type").and_then(Value::as_str) == Some("custom");
        if self
            .names
            .values()
            .any(|spec| spec.name == name && spec.namespace.as_deref() == namespace)
        {
            return Err(format!("重复的工具声明：{name}"));
        }
        let original = namespace.map_or_else(|| name.to_string(), |ns| format!("{ns}__{name}"));
        let mut upstream = original
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(48)
            .collect::<String>();
        if namespace.is_some() || upstream != original || self.names.contains_key(&upstream) {
            let hash = Sha256::digest(original.as_bytes());
            upstream.push('_');
            for b in hash.iter().take(6) {
                upstream.push_str(&format!("{b:02x}"));
            }
        }
        if self.names.contains_key(&upstream) {
            return Err(format!("重复的工具声明：{original}"));
        }
        let parameters = if custom {
            json!({"type":"object","properties":{"input":{"type":"string","description":"Raw input for the original custom tool. Preserve its formatting exactly."}},"required":["input"]})
        } else {
            tool.get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}))
        };
        let description = if custom {
            format!(
                "{}\nOriginal custom tool definition: {}",
                tool.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                tool
            )
        } else {
            tool.get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let mut declaration =
            json!({"name":upstream,"description":description,"parameters":parameters});
        if let Some(strict) = tool.get("strict") {
            declaration["strict"] = strict.clone();
        }
        self.declarations.push(declaration);
        self.names.insert(
            upstream,
            ToolSpec {
                name: name.into(),
                namespace: namespace.map(str::to_owned),
                custom,
            },
        );
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Part {
    Text(String),
    Image(String, Option<String>),
    File(Value),
    Call {
        id: String,
        name: String,
        arguments: String,
    },
    Result {
        id: String,
        output: Value,
    },
    Thinking(Value),
}

#[derive(Debug, Clone)]
struct Message {
    id: Option<String>,
    role: String,
    parts: Vec<Part>,
}

#[derive(Debug)]
pub(crate) struct PreparedRequest {
    pub(crate) bytes: Vec<u8>,
    pub(crate) context: ToolContext,
    pub(crate) model: String,
}

pub(crate) fn prepare(
    api: UpstreamApi,
    bytes: &[u8],
    compact: bool,
) -> std::result::Result<PreparedRequest, String> {
    let body: Value = serde_json::from_slice(bytes).map_err(|_| "请求不是有效的 JSON")?;
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if api == UpstreamApi::Responses {
        return Ok(PreparedRequest {
            bytes: bytes.to_vec(),
            context: ToolContext::default(),
            model,
        });
    }
    validate_fields(api, &body)?;
    if compact {
        return Err(
            "该上游协议不支持 Responses /compact；请使用 Codex 本地压缩或切换到 Responses 供应商"
                .into(),
        );
    }
    for key in ["previous_response_id", "conversation"] {
        if body
            .get(key)
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""))
        {
            return Err(format!("该上游协议无法读取 Responses {key} 服务端会话状态；请新建会话并发送完整对话，或使用 Responses 供应商"));
        }
    }
    if body.get("background").and_then(Value::as_bool) == Some(true) {
        return Err("该上游协议不支持 Responses 后台任务，请关闭 background".into());
    }
    let mut context = ToolContext::default();
    context.single_tool = body.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false);
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("function" | "custom") => context.add(tool, None)?,
                Some("namespace") => {
                    let namespace = tool.get("name").and_then(Value::as_str).ok_or("命名空间工具缺少名称")?;
                    for child in tool.get("tools").and_then(Value::as_array).ok_or("命名空间工具缺少 tools")? {
                        if !matches!(child.get("type").and_then(Value::as_str), Some("function" | "custom")) { return Err("命名空间中存在不支持的工具类型".into()); }
                        context.add(child, Some(namespace))?;
                    }
                }
                Some(kind) => return Err(format!("该上游协议无法转换 Responses 内置工具 {kind}；请关闭该工具或使用 Responses 供应商")),
                None => return Err("工具声明缺少 type".into()),
            }
        }
    }
    let messages = input_messages(&body, &context)?;
    let converted = match api {
        UpstreamApi::ChatCompletions => chat_request(&body, &messages, &context)?,
        UpstreamApi::AnthropicMessages => anthropic_request(&body, &messages, &context)?,
        UpstreamApi::Gemini => gemini_request(&body, &messages, &context)?,
        UpstreamApi::Responses => unreachable!(),
    };
    Ok(PreparedRequest {
        bytes: serde_json::to_vec(&converted).map_err(|_| "无法编码上游请求")?,
        context,
        model,
    })
}

fn input_messages(
    body: &Value,
    context: &ToolContext,
) -> std::result::Result<Vec<Message>, String> {
    let mut messages = Vec::new();
    if let Some(instructions) = body
        .get("instructions")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        messages.push(Message {
            id: None,
            role: "system".into(),
            parts: vec![Part::Text(instructions.into())],
        });
    }
    match body.get("input") {
        Some(Value::String(s)) => messages.push(Message {
            id: None,
            role: "user".into(),
            parts: vec![Part::Text(s.clone())],
        }),
        Some(Value::Array(items)) => {
            for item in items {
                match item.get("type").and_then(Value::as_str).unwrap_or("message") {
                "message" => {
                    let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                    if !matches!(role, "system" | "developer" | "user" | "assistant") { return Err(format!("不支持的对话角色：{role}")); }
                    messages.push(Message {id:item.get("id").and_then(Value::as_str).map(str::to_owned),role:role.into(), parts:content_parts(item.get("content").unwrap_or(&Value::Null))?});
                }
                "function_call" | "custom_tool_call" => {
                    let name = item.get("name").and_then(Value::as_str).ok_or("工具调用缺少名称")?;
                    let custom = item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
                    let arguments = if custom { json!({"input":item.get("input").and_then(Value::as_str).unwrap_or("")}).to_string() }
                        else { item.get("arguments").and_then(Value::as_str).unwrap_or("{}").into() };
                    let id = call_id(item)?;
                    let name = context.upstream_name(name, item.get("namespace").and_then(Value::as_str));
                    append_part(&mut messages, "assistant", Part::Call {id, name, arguments});
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let id = call_id(item)?;
                    append_part(&mut messages, "tool", Part::Result {id, output:item.get("output").cloned().unwrap_or(Value::Null)});
                }
                "reasoning" => {
                    if let Some(encoded) = item.get("encrypted_content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                        let value = decode_thinking(encoded)?;
                        append_part(&mut messages, "assistant", Part::Thinking(value));
                    } else if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                        let text = summary.iter().filter_map(|v| v.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n");
                        if !text.is_empty() { append_part(&mut messages, "assistant", Part::Thinking(json!({"protocol":"chat","text":text}))); }
                    }
                }
                other => return Err(format!("该上游协议无法转换 Responses 输入项 {other}；请新建会话或使用 Responses 供应商")),
            }
            }
        }
        None | Some(Value::Null) => {}
        _ => return Err("Responses input 必须是文字或对话数组".into()),
    }
    if messages.is_empty() {
        return Err("转换后的对话为空，请输入对话内容".into());
    }
    Ok(messages)
}

fn call_id(item: &Value) -> std::result::Result<String, String> {
    item.get("call_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or("工具调用或结果缺少 call_id".into())
}

fn append_part(messages: &mut Vec<Message>, role: &str, part: Part) {
    if let Some(last) = messages.last_mut().filter(|m| m.role == role) {
        last.parts.push(part)
    } else {
        messages.push(Message {
            id: None,
            role: role.into(),
            parts: vec![part],
        })
    }
}

fn content_parts(value: &Value) -> std::result::Result<Vec<Part>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(s) => Ok(vec![Part::Text(s.clone())]),
        Value::Array(parts) => parts
            .iter()
            .map(|p| match p.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text") => Ok(Part::Text(
                    p.get("text").and_then(Value::as_str).unwrap_or("").into(),
                )),
                Some("input_image" | "image_url") => {
                    let url = p
                        .get("image_url")
                        .and_then(Value::as_str)
                        .or_else(|| p.pointer("/image_url/url").and_then(Value::as_str))
                        .ok_or("图片缺少 image_url；文件 ID 图片需要 Responses 原生供应商")?;
                    Ok(Part::Image(
                        url.into(),
                        p.get("detail")
                            .or_else(|| p.pointer("/image_url/detail"))
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    ))
                }
                Some("input_file") => {
                    if p.get("file_id").is_some() {
                        return Err(
                            "文件 ID 输入需要 Responses 原生供应商；请附加文件数据或 URL".into(),
                        );
                    }
                    Ok(Part::File(p.clone()))
                }
                Some("refusal") => Ok(Part::Text(
                    p.get("refusal")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                )),
                Some(kind) => Err(format!("无法转换输入内容类型 {kind}")),
                None => Err("输入内容缺少 type".into()),
            })
            .collect(),
        _ => Err("对话内容必须是文字或内容数组".into()),
    }
}

fn output_parts(output: &Value) -> std::result::Result<Vec<Part>, String> {
    if output.is_array()
        && output
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("type").is_some())
    {
        content_parts(output)
    } else {
        Ok(vec![Part::Text(
            output
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| output.to_string()),
        )])
    }
}

fn chat_content(parts: &[Part]) -> std::result::Result<Value, String> {
    let mut result = Vec::new();
    for part in parts {
        match part {
            Part::Text(text) => result.push(json!({"type":"text","text":text})),
            Part::Image(url, detail) => {
                let mut image = json!({"type":"image_url","image_url":{"url":url}});
                if let Some(detail) = detail {
                    if !matches!(detail.as_str(), "auto" | "low" | "high") {
                        return Err(format!("Chat Completions 无法转换 image.detail={detail}"));
                    }
                    image["image_url"]["detail"] = json!(detail);
                }
                result.push(image);
            }
            Part::File(file) => {
                let data = file
                    .get("file_data")
                    .or_else(|| file.get("file_url"))
                    .ok_or("文件缺少 file_data 或 file_url")?;
                result.push(json!({"type":"file","file":{"filename":file.get("filename").and_then(Value::as_str).unwrap_or("input.pdf"),"file_data":data}}));
            }
            _ => {}
        }
    }
    if result.len() == 1 && result[0]["type"] == "text" {
        Ok(result[0]["text"].clone())
    } else {
        Ok(json!(result))
    }
}

fn chat_request(
    body: &Value,
    input: &[Message],
    context: &ToolContext,
) -> std::result::Result<Value, String> {
    let mut messages = Vec::new();
    for message in input {
        if message.role == "tool" {
            let mut pending_media = Vec::new();
            for part in &message.parts {
                if let Part::Result { id, output } = part {
                    let parts = output_parts(output)?;
                    let texts = parts
                        .iter()
                        .filter_map(|p| {
                            if let Part::Text(t) = p {
                                Some(t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    messages.push(json!({"role":"tool","tool_call_id":id,"content":texts}));
                    let media = parts
                        .into_iter()
                        .filter(|p| !matches!(p, Part::Text(_)))
                        .collect::<Vec<_>>();
                    if !media.is_empty() {
                        pending_media.extend(media);
                    }
                }
            }
            if !pending_media.is_empty() {
                messages.push(json!({"role":"user","content":chat_content(&pending_media)?}));
            }
            continue;
        }
        let mut value = json!({"role":if message.role == "developer" {"system"} else {message.role.as_str()},"content":chat_content(&message.parts)?});
        let mut calls = Vec::new();
        let mut thinking = Vec::new();
        for part in &message.parts {
            match part {
                Part::Call {id, name, arguments} => calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":arguments}})),
                Part::Thinking(v) => thinking.push(thinking_text(v)),
                _ => {},
            }
        }
        if !calls.is_empty() {
            value["tool_calls"] = json!(calls);
        }
        if !thinking.is_empty() {
            value["reasoning_content"] = json!(thinking.join("\n"));
        }
        if let Some(last) = messages
            .last_mut()
            .filter(|v| v.get("role") == Some(&value["role"]) && value["role"] == "assistant")
        {
            if !calls.is_empty() {
                if last.get("tool_calls").is_none() {
                    last["tool_calls"] = json!([]);
                }
                last["tool_calls"].as_array_mut().unwrap().extend(calls);
            }
            if let Some(thinking) = value.get("reasoning_content").and_then(Value::as_str) {
                let old = last
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                last["reasoning_content"] = json!(old + thinking);
            }
            let existing = last["content"].clone();
            let next = value["content"].clone();
            if existing.is_string() && next.is_string() {
                last["content"] =
                    json!(existing.as_str().unwrap().to_owned() + next.as_str().unwrap());
            } else {
                let mut parts = if existing.is_array() {
                    existing.as_array().unwrap().clone()
                } else if existing.as_str().is_some_and(|s| !s.is_empty()) {
                    vec![json!({"type":"text","text":existing})]
                } else {
                    vec![]
                };
                if next.is_array() {
                    parts.extend(next.as_array().unwrap().clone())
                } else if next.as_str().is_some_and(|s| !s.is_empty()) {
                    parts.push(json!({"type":"text","text":next}));
                }
                last["content"] = json!(parts);
            }
        } else {
            messages.push(value);
        }
    }
    let mut result = json!({"model":body["model"],"messages":messages,"stream":body.get("stream").and_then(Value::as_bool).unwrap_or(false)});
    for field in [
        "temperature",
        "top_p",
        "parallel_tool_calls",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "stop",
        "response_format",
        "service_tier",
    ] {
        if let Some(value) = body.get(field) {
            result[field] = value.clone();
        }
    }
    if let Some(max) = body.get("max_output_tokens") {
        result["max_tokens"] = max.clone();
    }
    if let Some(effort) = body.pointer("/reasoning/effort") {
        result["reasoning_effort"] = effort.clone();
    }
    apply_text_format(UpstreamApi::ChatCompletions, body, &mut result)?;
    if !context.declarations.is_empty() {
        result["tools"] = json!(context
            .declarations
            .iter()
            .map(|d| json!({"type":"function","function":d}))
            .collect::<Vec<_>>());
        if let Some(choice) = body.get("tool_choice") {
            result["tool_choice"] = chat_tool_choice(choice, context)?;
        }
    } else {
        result
            .as_object_mut()
            .unwrap()
            .remove("parallel_tool_calls");
    }
    if result["stream"] == true {
        result["stream_options"] = json!({"include_usage":true});
    }
    Ok(result)
}

fn chat_tool_choice(choice: &Value, context: &ToolContext) -> std::result::Result<Value, String> {
    if let Some(value) = choice.as_str() {
        return Ok(json!(value));
    }
    if matches!(
        choice.get("type").and_then(Value::as_str),
        Some("function" | "custom")
    ) {
        let name = choice
            .get("name")
            .and_then(Value::as_str)
            .ok_or("tool_choice 缺少名称")?;
        return Ok(
            json!({"type":"function","function":{"name":context.upstream_name(name, choice.get("namespace").and_then(Value::as_str))}}),
        );
    }
    Err("该上游协议不支持此 tool_choice".into())
}

fn anthropic_part(part: &Part) -> std::result::Result<Option<Value>, String> {
    Ok(Some(match part {
        Part::Text(text) => json!({"type":"text","text":text}),
        Part::Image(url, detail) => {
            check_native_image_detail(detail.as_deref())?;
            json!({"type":"image","source":anthropic_source(url)?})
        }
        Part::File(file) => {
            let data = file
                .get("file_data")
                .or_else(|| file.get("file_url"))
                .and_then(Value::as_str)
                .ok_or("文件缺少数据或 URL")?;
            json!({"type":"document","source":if data.starts_with("data:") || data.starts_with("http") {anthropic_source(data)?} else {json!({"type":"base64","media_type":"application/pdf","data":data})}})
        }
        Part::Call {
            id,
            name,
            arguments,
        } => json!({"type":"tool_use","id":id,"name":name,"input":parse_arguments(arguments)?}),
        Part::Result { id, output } => {
            json!({"type":"tool_result","tool_use_id":id,"content":output_parts(output)?.iter().map(anthropic_part).collect::<std::result::Result<Vec<_>, _>>()?.into_iter().flatten().collect::<Vec<_>>()})
        }
        Part::Thinking(v) => {
            if v.get("protocol").and_then(Value::as_str) != Some("anthropic") {
                return Err(
                    "该推理历史缺少 Anthropic 签名，无法转发；请新建会话或使用原协议供应商".into(),
                );
            }
            v.get("block").cloned().ok_or("Anthropic 推理历史无效")?
        }
    }))
}

fn anthropic_request(
    body: &Value,
    input: &[Message],
    context: &ToolContext,
) -> std::result::Result<Value, String> {
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in input {
        if matches!(message.role.as_str(), "system" | "developer") {
            for part in &message.parts {
                if let Part::Text(text) = part {
                    system.push(text.clone())
                } else {
                    return Err("Anthropic system 仅支持文字".into());
                }
            }
            continue;
        }
        let role = if message.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        let content = message
            .parts
            .iter()
            .map(anthropic_part)
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if content.is_empty() {
            continue;
        }
        if let Some(last) = messages.last_mut().filter(|v| v["role"] == role) {
            last["content"].as_array_mut().unwrap().extend(content)
        } else {
            messages.push(json!({"role":role,"content":content}));
        }
    }
    if messages.is_empty() {
        return Err("Anthropic 对话不能为空".into());
    }
    if messages[0]["role"] != "user" {
        return Err("Anthropic 对话须包含首条用户消息；请新建会话并发送完整对话".into());
    }
    let max = body
        .get("max_output_tokens")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .unwrap_or(8192);
    let mut result = json!({"model":body["model"],"messages":messages,"max_tokens":max,"stream":body.get("stream").and_then(Value::as_bool).unwrap_or(false)});
    if !system.is_empty() {
        result["system"] = json!(system.join("\n\n"));
    }
    for field in ["temperature", "top_p"] {
        if let Some(value) = body.get(field) {
            result[field] = value.clone();
        }
    }
    if let Some(stop) = body.get("stop") {
        result["stop_sequences"] = if stop.is_string() {
            json!([stop])
        } else {
            stop.clone()
        };
    }
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(Value::as_str) {
        let budget: u64 = match effort {
            "minimal" | "low" => 1024,
            "medium" => 4096,
            "high" => 8192,
            "xhigh" | "max" => 16384,
            "ultra" => 32768,
            "none" => 0,
            _ => return Err(format!("Anthropic 无法映射 reasoning.effort={effort}")),
        };
        let budget = budget.min(max / 2);
        if budget >= 1024 {
            if body.get("temperature").is_some() || body.get("top_p").is_some() {
                return Err(
                    "Anthropic thinking 与显式 temperature/top_p 不兼容；请移除采样参数或关闭推理"
                        .into(),
                );
            }
            result["thinking"] = json!({"type":"enabled","budget_tokens":budget});
            result.as_object_mut().unwrap().remove("temperature");
            result.as_object_mut().unwrap().remove("top_p");
        }
    }
    if !context.declarations.is_empty() {
        result["tools"] = json!(context.declarations.iter().map(|d| {let mut tool=json!({"name":d["name"],"description":d["description"],"input_schema":d["parameters"]});if let Some(strict)=d.get("strict"){tool["strict"]=strict.clone();}tool}).collect::<Vec<_>>());
        if let Some(choice) = body.get("tool_choice") {
            result["tool_choice"] = match choice.as_str() {
                Some("auto") => json!({"type":"auto"}),
                Some("required") => json!({"type":"any"}),
                Some("none") => json!({"type":"none"}),
                Some(_) => return Err("不支持的 Anthropic tool_choice".into()),
                None => {
                    let chat = chat_tool_choice(choice, context)?;
                    json!({"type":"tool","name":chat["function"]["name"]})
                }
            };
            if matches!(result["tool_choice"]["type"].as_str(), Some("any" | "tool")) {
                if result.get("thinking").is_some() {
                    return Err("Anthropic thinking 与强制 tool_choice 不兼容；请使用 auto 工具选择或关闭推理".into());
                }
                result.as_object_mut().unwrap().remove("thinking");
            }
        }
        if body.get("parallel_tool_calls").and_then(Value::as_bool) == Some(false) {
            if result.get("tool_choice").is_none() {
                result["tool_choice"] = json!({"type":"auto"});
            }
            result["tool_choice"]["disable_parallel_tool_use"] = json!(true);
        }
    }
    apply_text_format(UpstreamApi::AnthropicMessages, body, &mut result)?;
    Ok(result)
}

fn data_url(url: &str) -> std::result::Result<Option<(String, String)>, String> {
    if !url.starts_with("data:") {
        return Ok(None);
    }
    let (metadata, data) = url[5..].split_once(',').ok_or("无效的 data URL")?;
    let mime = metadata
        .strip_suffix(";base64")
        .filter(|v| !v.is_empty())
        .ok_or("多模态 data URL 必须使用 base64 编码")?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| "多模态数据不是有效的 base64")?;
    Ok(Some((mime.into(), data.into())))
}

fn anthropic_source(url: &str) -> std::result::Result<Value, String> {
    if let Some((mime, data)) = data_url(url)? {
        return Ok(json!({"type":"base64","media_type":mime,"data":data}));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("多模态 URL 必须是 HTTP URL 或 base64 data URL".into());
    }
    Ok(json!({"type":"url","url":url}))
}

fn gemini_media(url: &str, mime: &str) -> std::result::Result<Value, String> {
    if let Some((mime, data)) = data_url(url)? {
        return Ok(json!({"inlineData":{"mimeType":mime,"data":data}}));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") && !url.starts_with("gs://") {
        return Err("Gemini 文件须是 URL 或 base64 data URL".into());
    }
    Ok(json!({"fileData":{"mimeType":mime,"fileUri":url}}))
}

fn gemini_request(
    body: &Value,
    input: &[Message],
    context: &ToolContext,
) -> std::result::Result<Value, String> {
    let mut system = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    if context.single_tool {
        system.push(json!({"text":"Call at most one function per response. Wait for its result before calling another function."}));
    }
    let mut calls: HashMap<String, String> = HashMap::new();
    let mut signed_calls: HashMap<String, Value> = HashMap::new();
    let mut signed_text: HashMap<(String, String), Value> = HashMap::new();
    for message in input {
        for part in &message.parts {
            if let Part::Thinking(value) = part {
                if value.get("protocol").and_then(Value::as_str) == Some("gemini") {
                    for replay in value
                        .get("parts")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(id) = replay.pointer("/functionCall/id").and_then(Value::as_str)
                        {
                            signed_calls.insert(id.to_owned(), replay.clone());
                        } else if replay.get("thought").and_then(Value::as_bool) != Some(true) {
                            if let Some(text) = replay.get("text").and_then(Value::as_str) {
                                let id = replay
                                    .get("codex_item_id")
                                    .and_then(Value::as_str)
                                    .ok_or("Gemini 文字签名缺少对应的消息 ID；请新建会话")?;
                                let mut replay = replay.clone();
                                replay.as_object_mut().unwrap().remove("codex_item_id");
                                if signed_text
                                    .insert((id.to_owned(), text.to_owned()), replay)
                                    .is_some()
                                {
                                    return Err("重复的 Gemini 文字签名历史".into());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for message in input {
        if matches!(message.role.as_str(), "system" | "developer") {
            for part in &message.parts {
                if let Part::Text(text) = part {
                    system.push(json!({"text":text}))
                } else {
                    return Err("Gemini systemInstruction 仅支持文字".into());
                }
            }
            continue;
        }
        let role = if message.role == "assistant" {
            "model"
        } else {
            "user"
        };
        let mut parts = Vec::new();
        for part in &message.parts {
            match part {
                Part::Text(text) => parts.push(
                    if message.role == "assistant" {
                        signed_text.remove(&(message.id.clone().unwrap_or_default(), text.clone()))
                    } else {
                        None
                    }
                    .unwrap_or_else(|| json!({"text":text})),
                ),
                Part::Image(url, detail) => {
                    check_native_image_detail(detail.as_deref())?;
                    parts.push(gemini_media(url, "image/jpeg")?);
                }
                Part::File(file) => {
                    let data = file
                        .get("file_data")
                        .or_else(|| file.get("file_url"))
                        .and_then(Value::as_str)
                        .ok_or("文件缺少数据或 URL")?;
                    parts.push(
                        if data.starts_with("data:")
                            || data.starts_with("http")
                            || data.starts_with("gs://")
                        {
                            gemini_media(data, "application/pdf")?
                        } else {
                            json!({"inlineData":{"mimeType":"application/pdf","data":data}})
                        },
                    );
                }
                Part::Call {
                    id,
                    name,
                    arguments,
                } => {
                    calls.insert(id.clone(), name.clone());
                    parts.push(signed_calls.remove(id).unwrap_or(json!({"functionCall":{"id":id,"name":name,"args":parse_arguments(arguments)?}})));
                }
                Part::Result { id, output } => {
                    let name = calls
                        .get(id)
                        .ok_or("Gemini 工具结果缺少对应的 function_call；请发送完整工具对话")?;
                    let result_parts = output_parts(output)?;
                    let text = result_parts
                        .iter()
                        .filter_map(|p| {
                            if let Part::Text(t) = p {
                                Some(t.as_str())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    parts.push(json!({"functionResponse":{"id":id,"name":name,"response":{"output":text}}}));
                    for media in result_parts.iter().filter(|p| !matches!(p, Part::Text(_))) {
                        match media {
                            Part::Image(url, detail) => {
                                check_native_image_detail(detail.as_deref())?;
                                parts.push(gemini_media(url, "image/jpeg")?);
                            }
                            Part::File(file) => {
                                let data = file
                                    .get("file_data")
                                    .or_else(|| file.get("file_url"))
                                    .and_then(Value::as_str)
                                    .ok_or("工具文件缺少数据")?;
                                parts.push(gemini_media(data, "application/pdf")?);
                            }
                            _ => {}
                        }
                    }
                }
                Part::Thinking(value) => {
                    if value.get("protocol").and_then(Value::as_str) != Some("gemini") {
                        return Err(
                            "该推理历史缺少 Gemini thoughtSignature；请新建会话或使用原协议供应商"
                                .into(),
                        );
                    }
                    parts.extend(
                        value
                            .get("parts")
                            .and_then(Value::as_array)
                            .ok_or("Gemini 推理历史无效")?
                            .iter()
                            .filter(|part| {
                                part.get("thought").and_then(Value::as_bool) == Some(true)
                            })
                            .cloned(),
                    );
                }
            }
        }
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = contents.last_mut().filter(|v| v["role"] == role) {
            last["parts"].as_array_mut().unwrap().extend(parts)
        } else {
            contents.push(json!({"role":role,"parts":parts}));
        }
    }
    if !signed_calls.is_empty() || !signed_text.is_empty() {
        return Err("Gemini 签名历史缺少对应的工具调用或消息；请发送完整对话或新建会话".into());
    }
    if contents.is_empty() {
        return Err("Gemini 对话不能为空".into());
    }
    let mut result = json!({"contents":contents});
    if !system.is_empty() {
        result["systemInstruction"] = json!({"parts":system});
    }
    let mut generation = Map::new();
    for (from, to) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("max_output_tokens", "maxOutputTokens"),
        ("stop", "stopSequences"),
    ] {
        if let Some(v) = body.get(from) {
            generation.insert(
                to.into(),
                if from == "stop" && v.is_string() {
                    json!([v])
                } else {
                    v.clone()
                },
            );
        }
    }
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(Value::as_str) {
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim_start_matches("models/");
        let config = if model.starts_with("gemini-3") {
            if effort == "none" {
                return Err(
                    "Gemini 3 的 thinkingLevel 无法保证完全关闭推理；请选择 minimal/low 或其它模型"
                        .into(),
                );
            }
            let level = match effort {
                "minimal" if model.contains("flash") => "MINIMAL",
                "minimal" | "low" => "LOW",
                "medium" if model.contains("flash") => "MEDIUM",
                _ => "HIGH",
            };
            json!({"includeThoughts":true,"thinkingLevel":level})
        } else {
            json!({"includeThoughts":true,"thinkingBudget":if effort=="none"{0}else{-1}})
        };
        generation.insert("thinkingConfig".into(), config);
    }
    if !generation.is_empty() {
        result["generationConfig"] = json!(generation);
    }
    if !context.declarations.is_empty() {
        if context
            .declarations
            .iter()
            .any(|d| d.get("strict").and_then(Value::as_bool) == Some(true))
        {
            return Err(
                "Gemini 原生函数声明没有 strict 保证；请关闭工具 strict 或使用其它上游协议".into(),
            );
        }
        result["tools"] = json!([{"functionDeclarations":context.declarations.iter().map(|d|json!({"name":d["name"],"description":d["description"],"parametersJsonSchema":d["parameters"]})).collect::<Vec<_>>()}]);
        if let Some(choice) = body.get("tool_choice") {
            let config = match choice.as_str() {
                Some("auto") => json!({"mode":"AUTO"}),
                Some("required") => json!({"mode":"ANY"}),
                Some("none") => json!({"mode":"NONE"}),
                Some(_) => return Err("不支持的 Gemini tool_choice".into()),
                None => {
                    let chat = chat_tool_choice(choice, context)?;
                    json!({"mode":"ANY","allowedFunctionNames":[chat["function"]["name"]]})
                }
            };
            result["toolConfig"] = json!({"functionCallingConfig":config});
        }
    }
    apply_text_format(UpstreamApi::Gemini, body, &mut result)?;
    Ok(result)
}

fn check_native_image_detail(detail: Option<&str>) -> std::result::Result<(), String> {
    if detail.is_some_and(|detail| detail != "auto") {
        return Err(
            "该原生上游没有等价的 Responses 图片 detail 参数；请使用 auto 或 Chat/Responses 上游"
                .into(),
        );
    }
    Ok(())
}

fn validate_fields(api: UpstreamApi, body: &Value) -> std::result::Result<(), String> {
    const KNOWN: &[&str] = &[
        "model",
        "input",
        "instructions",
        "tools",
        "tool_choice",
        "stream",
        "reasoning",
        "temperature",
        "top_p",
        "max_output_tokens",
        "parallel_tool_calls",
        "metadata",
        "store",
        "include",
        "truncation",
        "text",
        "service_tier",
        "prompt_cache_key",
        "prompt_cache_retention",
        "safety_identifier",
        "user",
        "previous_response_id",
        "conversation",
        "background",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "stop",
        "response_format",
    ];
    if let Some(fields) = body.as_object() {
        for name in fields.keys() {
            if !KNOWN.contains(&name.as_str()) {
                return Err(format!(
                    "该上游协议无法转换 Responses 参数 {name}；请移除此参数或使用 Responses 供应商"
                ));
            }
        }
    }
    if body
        .get("tools")
        .is_some_and(|v| !v.is_null() && !v.is_array())
    {
        return Err("tools 必须是数组".into());
    }
    if body
        .get("tool_choice")
        .is_some_and(|v| !v.is_null() && v.as_str() != Some("auto"))
        && body
            .get("tools")
            .and_then(Value::as_array)
            .is_none_or(|tools| tools.is_empty())
    {
        return Err("指定 tool_choice 时必须提供 tools".into());
    }
    if body.get("store").and_then(Value::as_bool) == Some(true) {
        return Err("转换上游不支持 Responses 服务端存储，请设置 store=false".into());
    }
    if body
        .get("truncation")
        .and_then(Value::as_str)
        .is_some_and(|v| v != "disabled")
    {
        return Err("转换上游不支持 Responses 自动截断，请使用本地压缩或 Responses 供应商".into());
    }
    if let Some(includes) = body.get("include").and_then(Value::as_array) {
        for include in includes {
            if include.as_str() != Some("reasoning.encrypted_content") {
                return Err(format!("转换上游不支持 Responses include={include}"));
            }
        }
    }
    if api != UpstreamApi::ChatCompletions {
        for key in [
            "frequency_penalty",
            "presence_penalty",
            "seed",
            "response_format",
        ] {
            if body.get(key).is_some_and(|v| !v.is_null()) {
                return Err(format!("该上游协议无法转换参数 {key}"));
            }
        }
    }
    if let Some(tier) = body
        .get("service_tier")
        .and_then(Value::as_str)
        .filter(|v| *v != "auto")
    {
        if api != UpstreamApi::ChatCompletions {
            return Err(format!("该上游协议不支持 Responses service_tier={tier}"));
        }
    }
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(Value::as_str) {
        if !matches!(
            effort,
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
        ) {
            return Err(format!("未知的 reasoning.effort={effort}"));
        }
    }
    Ok(())
}

fn apply_text_format(
    api: UpstreamApi,
    body: &Value,
    result: &mut Value,
) -> std::result::Result<(), String> {
    if let Some(verbosity) = body.pointer("/text/verbosity") {
        if api == UpstreamApi::ChatCompletions {
            result["verbosity"] = verbosity.clone();
        } else {
            return Err("该上游协议没有 Responses text.verbosity 参数；请移除此参数或使用 Responses/Chat 供应商".into());
        }
    }
    let Some(format) = body.pointer("/text/format") else {
        return Ok(());
    };
    match format.get("type").and_then(Value::as_str).unwrap_or("text") {
        "text" => Ok(()),
        "json_object" => match api {
            UpstreamApi::ChatCompletions => {
                result["response_format"] = json!({"type":"json_object"});
                Ok(())
            }
            UpstreamApi::Gemini => {
                if result.get("generationConfig").is_none() {
                    result["generationConfig"] = json!({});
                }
                result["generationConfig"]["responseMimeType"] = json!("application/json");
                Ok(())
            }
            _ => {
                Err("Anthropic 结构化输出需要 JSON Schema，不能转换 text.format=json_object".into())
            }
        },
        "json_schema" => {
            let schema = format
                .get("schema")
                .filter(|v| v.is_object())
                .ok_or("text.format=json_schema 缺少有效的 schema")?;
            match api {
                UpstreamApi::ChatCompletions => {
                    result["response_format"] = json!({"type":"json_schema","json_schema":{"name":format.get("name").and_then(Value::as_str).unwrap_or("response"),"schema":schema,"strict":format.get("strict").and_then(Value::as_bool).unwrap_or(false)}});
                }
                UpstreamApi::AnthropicMessages => {
                    if result.get("output_config").is_none() {
                        result["output_config"] = json!({});
                    }
                    result["output_config"]["format"] =
                        json!({"type":"json_schema","schema":schema});
                }
                UpstreamApi::Gemini => {
                    if result.get("generationConfig").is_none() {
                        result["generationConfig"] = json!({});
                    }
                    result["generationConfig"]["responseMimeType"] = json!("application/json");
                    result["generationConfig"]["responseJsonSchema"] = schema.clone();
                }
                _ => unreachable!(),
            };
            Ok(())
        }
        kind => Err(format!("无法转换 text.format={kind}")),
    }
}

fn parse_arguments(arguments: &str) -> std::result::Result<Value, String> {
    let value: Value =
        serde_json::from_str(arguments).map_err(|_| "工具 arguments 不是有效的 JSON")?;
    if !value.is_object() {
        return Err("工具 arguments 必须是 JSON 对象".into());
    }
    Ok(value)
}

pub(super) fn encode_thinking(value: &Value) -> String {
    format!(
        "codex-x-bridge:{}",
        base64::engine::general_purpose::STANDARD.encode(value.to_string())
    )
}
fn decode_thinking(value: &str) -> std::result::Result<Value, String> {
    let encoded = value
        .strip_prefix("codex-x-bridge:")
        .ok_or("该上游协议无法解密 Responses 推理或压缩状态；请新建会话或使用 Responses 供应商")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "桥接推理状态损坏")?;
    serde_json::from_slice(&bytes).map_err(|_| "桥接推理状态损坏".into())
}
fn thinking_text(value: &Value) -> String {
    value
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/block/thinking").and_then(Value::as_str))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            value
                .get("parts")
                .and_then(Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        })
}
