use super::{encode_thinking, ToolContext, UpstreamApi};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) fn error_message(value: &Value) -> Option<String> {
    let error = value.get("error").filter(|v| !v.is_null()).or_else(|| {
        (value.get("type").and_then(Value::as_str) == Some("error")).then_some(value)
    })?;
    Some(
        error
            .as_str()
            .map(str::to_owned)
            .or_else(|| {
                error
                    .get("message")
                    .or_else(|| error.get("detail"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| error.to_string()),
    )
}

pub(crate) fn convert_response(
    api: UpstreamApi,
    body: &Value,
    context: &ToolContext,
    requested_model: &str,
) -> std::result::Result<Value, String> {
    if let Some(error) = error_message(body) {
        return Err(error);
    }
    match api {
        UpstreamApi::Responses => Ok(body.clone()),
        UpstreamApi::ChatCompletions => chat_response(body, context, requested_model),
        UpstreamApi::AnthropicMessages => anthropic_response(body, context, requested_model),
        UpstreamApi::Gemini => gemini_response(body, context, requested_model),
    }
}

pub(super) fn response_id(id: Option<&str>) -> String {
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        let digest = Sha256::digest(id.as_bytes());
        return format!(
            "resp_codexx_{}",
            digest
                .iter()
                .take(12)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
    }
    format!(
        "resp_codexx_{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

pub(super) fn envelope(
    id: &str,
    model: &str,
    status: &str,
    output: Vec<Value>,
    usage: Value,
    reason: Option<&str>,
) -> Value {
    let mut value = json!({"id":id,"object":"response","created_at":chrono::Utc::now().timestamp(),"model":model,"status":status,"output":output,"usage":usage,"error":null,"incomplete_details":null});
    if let Some(reason) = reason {
        value["incomplete_details"] = json!({"reason":reason});
    }
    value
}

pub(super) fn text_item(id: &str, text: &str) -> Value {
    json!({"id":id,"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]})
}

pub(super) fn reasoning_item(id: &str, text: &str, state: Option<Value>) -> Value {
    let mut value = json!({"id":id,"type":"reasoning","summary":if text.is_empty(){json!([])}else{json!([{"type":"summary_text","text":text}])}});
    if let Some(state) = state {
        value["encrypted_content"] = json!(encode_thinking(&state));
    }
    value
}

pub(super) fn tool_item(
    context: &ToolContext,
    id: &str,
    call_id: &str,
    upstream_name: &str,
    arguments: &str,
) -> std::result::Result<Value, String> {
    if upstream_name.is_empty() {
        return Err("上游工具调用缺少名称".into());
    }
    let spec = context.names.get(upstream_name);
    let mut value = if spec.is_some_and(|s| s.custom) {
        let arguments: Value =
            serde_json::from_str(arguments).map_err(|_| "上游 custom 工具参数不是有效的 JSON")?;
        let input = arguments
            .get("input")
            .and_then(Value::as_str)
            .ok_or("上游 custom 工具参数缺少 input 文字")?;
        json!({"id":id,"type":"custom_tool_call","call_id":call_id,"name":spec.unwrap().name,"input":input,"status":"completed"})
    } else {
        let parsed: Value =
            serde_json::from_str(arguments).map_err(|_| "上游工具参数不是有效的 JSON")?;
        if !parsed.is_object() {
            return Err("上游工具参数须是 JSON 对象".into());
        }
        json!({"id":id,"type":"function_call","call_id":call_id,"name":spec.map_or(upstream_name,|s|s.name.as_str()),"arguments":arguments,"status":"completed"})
    };
    if let Some(namespace) = spec.and_then(|s| s.namespace.as_deref()) {
        value["namespace"] = json!(namespace);
    }
    Ok(value)
}

pub(super) fn terminal(
    api: UpstreamApi,
    reason: Option<&str>,
) -> (&'static str, Option<&'static str>) {
    match (api, reason) {
        (_, Some("length" | "max_tokens" | "MAX_TOKENS" | "model_context_window_exceeded")) => {
            ("incomplete", Some("max_output_tokens"))
        }
        (
            _,
            Some(
                "content_filter" | "refusal" | "SAFETY" | "BLOCKLIST" | "PROHIBITED_CONTENT"
                | "RECITATION" | "SPII" | "IMAGE_SAFETY",
            ),
        ) => ("incomplete", Some("content_filter")),
        (_, Some("MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL")) => {
            ("incomplete", Some("tool_call_error"))
        }
        _ => ("completed", None),
    }
}

fn tokens(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or(0)
}
pub(super) fn usage(api: UpstreamApi, value: Option<&Value>) -> Value {
    let empty = json!({});
    let u = value.unwrap_or(&empty);
    let (input, output, cached, reasoning, write) = match api {
        UpstreamApi::ChatCompletions => (
            tokens(u.get("prompt_tokens")),
            tokens(u.get("completion_tokens")),
            tokens(u.pointer("/prompt_tokens_details/cached_tokens"))
                .max(tokens(u.get("prompt_cache_hit_tokens"))),
            tokens(u.pointer("/completion_tokens_details/reasoning_tokens")),
            tokens(u.get("cache_creation_input_tokens")),
        ),
        UpstreamApi::AnthropicMessages => {
            let cached = tokens(u.get("cache_read_input_tokens"));
            let write = tokens(u.get("cache_creation_input_tokens"));
            (
                tokens(u.get("input_tokens"))
                    .saturating_add(cached)
                    .saturating_add(write),
                tokens(u.get("output_tokens")),
                cached,
                tokens(u.get("thinking_tokens")),
                write,
            )
        }
        UpstreamApi::Gemini => {
            let reasoning = tokens(u.get("thoughtsTokenCount"));
            (
                tokens(u.get("promptTokenCount")),
                tokens(u.get("candidatesTokenCount")).saturating_add(reasoning),
                tokens(u.get("cachedContentTokenCount")),
                reasoning,
                0,
            )
        }
        UpstreamApi::Responses => return u.clone(),
    };
    json!({"input_tokens":input,"output_tokens":output,"total_tokens":input.saturating_add(output),"input_tokens_details":{"cached_tokens":cached,"cache_write_tokens":write},"output_tokens_details":{"reasoning_tokens":reasoning}})
}

fn chat_response(
    body: &Value,
    context: &ToolContext,
    model: &str,
) -> std::result::Result<Value, String> {
    let choice = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .ok_or("Chat 响应缺少 choices")?;
    let message = choice.get("message").ok_or("Chat 响应缺少 message")?;
    let id = response_id(body.get("id").and_then(Value::as_str));
    let mut output = Vec::new();
    if let Some(thinking) = chat_reasoning(message) {
        output.push(reasoning_item(&format!("rs_{id}"), &thinking, None));
    }
    let text = match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => {
            let mut text = String::new();
            for part in parts {
                if matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "output_text")
                ) {
                    text.push_str(
                        part.get("text")
                            .and_then(Value::as_str)
                            .ok_or("Chat 文字内容缺少 text")?,
                    );
                } else {
                    return Err("Chat 响应含不能转换的非文字内容".into());
                }
            }
            text
        }
        _ => String::new(),
    };
    if !text.is_empty() {
        output.push(text_item(&format!("msg_{id}"), &text));
    }
    if let Some(refusal) = message.get("refusal").and_then(Value::as_str) {
        output.push(json!({"id":format!("msg_refusal_{id}"),"type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":refusal}]}));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for (index, call) in calls.iter().enumerate() {
            let function = call.get("function").ok_or("Chat 工具调用缺少 function")?;
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{id}_{index}"));
            output.push(tool_item(
                context,
                &format!("fc_{id}_{index}"),
                &call_id,
                function.get("name").and_then(Value::as_str).unwrap_or(""),
                function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}"),
            )?);
        }
    }
    if let Some(call) = message.get("function_call") {
        output.push(tool_item(
            context,
            &format!("fc_{id}_legacy"),
            &format!("call_{id}_legacy"),
            call.get("name").and_then(Value::as_str).unwrap_or(""),
            call.get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}"),
        )?);
    }
    let reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .ok_or("Chat 响应缺少 finish_reason，无法确认完整完成")?;
    let (status, incomplete) = terminal(UpstreamApi::ChatCompletions, Some(reason));
    Ok(envelope(
        &id,
        body.get("model").and_then(Value::as_str).unwrap_or(model),
        status,
        output,
        usage(UpstreamApi::ChatCompletions, body.get("usage")),
        incomplete,
    ))
}

pub(super) fn chat_reasoning(value: &Value) -> Option<String> {
    value
        .get("reasoning_content")
        .or_else(|| value.get("reasoning"))
        .and_then(|v| match v {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(|v| v.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            _ => None,
        })
}

fn anthropic_response(
    body: &Value,
    context: &ToolContext,
    model: &str,
) -> std::result::Result<Value, String> {
    let blocks = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or("Anthropic 响应缺少 content")?;
    let id = response_id(body.get("id").and_then(Value::as_str));
    let mut output = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => output.push(text_item(
                &format!("msg_{id}_{index}"),
                block.get("text").and_then(Value::as_str).unwrap_or(""),
            )),
            Some("thinking" | "redacted_thinking") => output.push(reasoning_item(
                &format!("rs_{id}_{index}"),
                block.get("thinking").and_then(Value::as_str).unwrap_or(""),
                Some(json!({"protocol":"anthropic","block":block})),
            )),
            Some("tool_use") => {
                let call_id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("Anthropic tool_use 缺少 id")?;
                output.push(tool_item(
                    context,
                    &format!("fc_{id}_{index}"),
                    call_id,
                    block.get("name").and_then(Value::as_str).unwrap_or(""),
                    &block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string(),
                )?);
            }
            Some(other) => return Err(format!("无法转换 Anthropic 输出类型 {other}")),
            None => return Err("Anthropic content 缺少类型".into()),
        }
    }
    let reason = body
        .get("stop_reason")
        .and_then(Value::as_str)
        .ok_or("Anthropic 响应缺少 stop_reason，无法确认完整完成")?;
    let (status, incomplete) = terminal(UpstreamApi::AnthropicMessages, Some(reason));
    Ok(envelope(
        &id,
        body.get("model").and_then(Value::as_str).unwrap_or(model),
        status,
        output,
        usage(UpstreamApi::AnthropicMessages, body.get("usage")),
        incomplete,
    ))
}

fn gemini_response(
    body: &Value,
    context: &ToolContext,
    model: &str,
) -> std::result::Result<Value, String> {
    if let Some(block) = body
        .pointer("/promptFeedback/blockReason")
        .and_then(Value::as_str)
    {
        return Err(format!("Gemini 拒绝了输入：{block}"));
    }
    let candidate = body
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|v| v.first())
        .ok_or("Gemini 响应缺少 candidates")?;
    let parts = candidate
        .pointer("/content/parts")
        .and_then(Value::as_array);
    if context.single_tool
        && parts
            .into_iter()
            .flatten()
            .filter(|part| part.get("functionCall").is_some())
            .count()
            > 1
    {
        return Err("Gemini 返回了多个并行工具调用，而本次请求禁止并行；未向 Codex 返回任何可执行工具，请重试".into());
    }
    let id = response_id(body.get("responseId").and_then(Value::as_str));
    let mut output = Vec::new();
    let signed: Vec<Value> = parts
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, part)| {
            part.get("thoughtSignature").is_some()
                && part.get("thought").and_then(Value::as_bool) != Some(true)
        })
        .map(|(index, part)| {
            let mut part = part.clone();
            if part.get("text").is_some() {
                part["codex_item_id"] = json!(format!("msg_{id}_{index}"));
            }
            if part.get("functionCall").is_some() && part.pointer("/functionCall/id").is_none() {
                part["functionCall"]["id"] = json!(format!("call_{id}_{index}"));
            }
            part
        })
        .collect();
    if !signed.is_empty() {
        output.push(reasoning_item(
            &format!("rs_sig_{id}"),
            "",
            Some(json!({"protocol":"gemini","parts":signed})),
        ));
    }
    for (index, part) in parts.into_iter().flatten().enumerate() {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            output.push(
                if part.get("thought").and_then(Value::as_bool) == Some(true) {
                    reasoning_item(
                        &format!("rs_{id}_{index}"),
                        text,
                        Some(json!({"protocol":"gemini","parts":[part]})),
                    )
                } else {
                    text_item(&format!("msg_{id}_{index}"), text)
                },
            );
        } else if let Some(call) = part.get("functionCall") {
            let call_id = call
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("call_{id}_{index}"));
            output.push(tool_item(
                context,
                &format!("fc_{id}_{index}"),
                &call_id,
                call.get("name").and_then(Value::as_str).unwrap_or(""),
                &call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string(),
            )?);
        } else if part.get("thoughtSignature").is_none() {
            return Err("Gemini 返回了不能转换为 Codex 输出的多模态内容".into());
        }
    }
    let reason = candidate
        .get("finishReason")
        .and_then(Value::as_str)
        .ok_or("Gemini 响应缺少 finishReason，无法确认完整完成")?;
    let (status, incomplete) = terminal(UpstreamApi::Gemini, Some(reason));
    Ok(envelope(
        &id,
        body.get("modelVersion")
            .and_then(Value::as_str)
            .unwrap_or(model),
        status,
        output,
        usage(UpstreamApi::Gemini, body.get("usageMetadata")),
        incomplete,
    ))
}
