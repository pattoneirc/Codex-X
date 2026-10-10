//! Incremental SSE reader. Frames are split as bytes before JSON decoding, so
//! UTF-8 and CRLF split across HTTP chunks never corrupt text or tool arguments.
use super::response::{
    chat_reasoning, envelope, reasoning_item, response_id, terminal, text_item, tool_item, usage,
};
use super::{convert_response, error_message, ToolContext, UpstreamApi};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Reasoning,
    Tool,
}

struct Item {
    kind: Kind,
    id: String,
    text: String,
    call_id: String,
    name: String,
    arguments: String,
    source: Value,
    added: bool,
    done: bool,
    output: Option<Value>,
}

struct State {
    api: UpstreamApi,
    context: ToolContext,
    id: String,
    model: String,
    started: bool,
    substantive: bool,
    completed: bool,
    reason: Option<String>,
    usage: Value,
    items: Vec<Item>,
    keys: HashMap<String, usize>,
    sequence: u64,
    signed_gemini_parts: Vec<Value>,
    gemini_segment: usize,
}

impl State {
    fn new(api: UpstreamApi, context: ToolContext, model: String) -> Self {
        Self {
            api,
            context,
            id: response_id(None),
            model,
            started: false,
            substantive: false,
            completed: false,
            reason: None,
            usage: json!({}),
            items: Vec::new(),
            keys: HashMap::new(),
            sequence: 0,
            signed_gemini_parts: Vec::new(),
            gemini_segment: 0,
        }
    }
    fn event(&mut self, kind: &str, mut value: Value, output: &mut Vec<u8>) {
        value["type"] = json!(kind);
        value["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        output.extend_from_slice(format!("event: {kind}\ndata: {value}\n\n").as_bytes());
    }
    fn begin(&mut self, output: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        let response = envelope(
            &self.id,
            &self.model,
            "in_progress",
            Vec::new(),
            usage(self.api, Some(&self.usage)),
            None,
        );
        self.event("response.created", json!({"response":response}), output);
        self.event("response.in_progress", json!({"response":response}), output);
    }
    fn item(&mut self, key: String, kind: Kind) -> usize {
        if let Some(index) = self.keys.get(&key) {
            return *index;
        }
        let index = self.items.len();
        self.items.push(Item {
            kind,
            id: format!(
                "{}_{}_{}",
                if kind == Kind::Text {
                    "msg"
                } else if kind == Kind::Reasoning {
                    "rs"
                } else {
                    "fc"
                },
                self.id,
                index
            ),
            text: String::new(),
            call_id: format!("call_{}_{}", self.id, index),
            name: String::new(),
            arguments: String::new(),
            source: json!({}),
            added: false,
            done: false,
            output: None,
        });
        self.keys.insert(key, index);
        index
    }
    fn start_item(
        &mut self,
        index: usize,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), String> {
        if self.items[index].added {
            return Ok(());
        }
        self.begin(output);
        let item = &self.items[index];
        let initial = match item.kind {
            Kind::Text => {
                json!({"id":item.id,"type":"message","role":"assistant","status":"in_progress","content":[]})
            }
            Kind::Reasoning => json!({"id":item.id,"type":"reasoning","summary":[]}),
            Kind::Tool => {
                let mut value = tool_item(
                    &self.context,
                    &item.id,
                    &item.call_id,
                    &item.name,
                    if item.arguments.is_empty() {
                        "{}"
                    } else {
                        &item.arguments
                    },
                )?;
                if value["type"] == "custom_tool_call" {
                    value["input"] = json!("");
                } else {
                    value["arguments"] = json!("");
                }
                value["status"] = json!("in_progress");
                value
            }
        };
        self.event(
            "response.output_item.added",
            json!({"output_index":index,"item":initial}),
            output,
        );
        match self.items[index].kind {
            Kind::Text=>self.event("response.content_part.added",json!({"item_id":self.items[index].id,"output_index":index,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}),output),
            Kind::Reasoning=>self.event("response.reasoning_summary_part.added",json!({"item_id":self.items[index].id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":""}}),output),
            _=>{},
        }
        self.items[index].added = true;
        Ok(())
    }
    fn text(
        &mut self,
        index: usize,
        delta: &str,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), String> {
        if delta.is_empty() {
            return Ok(());
        }
        self.substantive = true;
        self.start_item(index, output)?;
        self.items[index].text.push_str(delta);
        let item = &self.items[index];
        if item.kind == Kind::Text {
            self.event(
                "response.output_text.delta",
                json!({"item_id":item.id,"output_index":index,"content_index":0,"delta":delta}),
                output,
            )
        } else {
            self.event(
                "response.reasoning_summary_text.delta",
                json!({"item_id":item.id,"output_index":index,"summary_index":0,"delta":delta}),
                output,
            )
        }
        Ok(())
    }
    fn finish_item(
        &mut self,
        index: usize,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), String> {
        if self.items[index].done {
            return Ok(());
        }
        self.start_item(index, output)?;
        let item = &self.items[index];
        let final_item = match item.kind {
            Kind::Text => text_item(&item.id, &item.text),
            Kind::Reasoning => {
                let state = if self.api == UpstreamApi::AnthropicMessages {
                    Some(json!({"protocol":"anthropic","block":item.source}))
                } else if self.api == UpstreamApi::Gemini {
                    let mut part = item.source.clone();
                    part["text"] = json!(item.text);
                    part["thought"] = json!(true);
                    Some(json!({"protocol":"gemini","parts":[part]}))
                } else {
                    None
                };
                reasoning_item(&item.id, &item.text, state)
            }
            Kind::Tool => tool_item(
                &self.context,
                &item.id,
                &item.call_id,
                &item.name,
                if item.arguments.is_empty() {
                    "{}"
                } else {
                    &item.arguments
                },
            )?,
        };
        let id = self.items[index].id.clone();
        match self.items[index].kind {
            Kind::Text => {
                self.event("response.output_text.done",json!({"item_id":id,"output_index":index,"content_index":0,"text":self.items[index].text}),output);
                self.event("response.content_part.done",json!({"item_id":id,"output_index":index,"content_index":0,"part":final_item["content"][0]}),output);
            }
            Kind::Reasoning => {
                self.event("response.reasoning_summary_text.done",json!({"item_id":id,"output_index":index,"summary_index":0,"text":self.items[index].text}),output);
                self.event("response.reasoning_summary_part.done",json!({"item_id":id,"output_index":index,"summary_index":0,"part":{"type":"summary_text","text":self.items[index].text}}),output);
            }
            Kind::Tool => {
                self.substantive = true;
                if final_item["type"] == "custom_tool_call" {
                    self.event(
                        "response.custom_tool_call_input.delta",
                        json!({"item_id":id,"output_index":index,"delta":final_item["input"]}),
                        output,
                    );
                    self.event(
                        "response.custom_tool_call_input.done",
                        json!({"item_id":id,"output_index":index,"input":final_item["input"]}),
                        output,
                    );
                } else {
                    self.event(
                        "response.function_call_arguments.delta",
                        json!({"item_id":id,"output_index":index,"delta":final_item["arguments"]}),
                        output,
                    );
                    self.event("response.function_call_arguments.done",json!({"item_id":id,"output_index":index,"arguments":final_item["arguments"]}),output);
                }
            }
        }
        self.event(
            "response.output_item.done",
            json!({"output_index":index,"item":final_item}),
            output,
        );
        self.items[index].done = true;
        self.items[index].output = Some(final_item);
        Ok(())
    }
    fn finish(&mut self, truncated: bool, output: &mut Vec<u8>) -> std::result::Result<(), String> {
        if self.completed {
            return Ok(());
        }
        if self.api == UpstreamApi::Gemini
            && self.context.single_tool
            && self
                .items
                .iter()
                .filter(|item| item.kind == Kind::Tool)
                .count()
                > 1
        {
            return Err("Gemini 返回了多个并行工具调用，而本次请求禁止并行；未向 Codex 返回任何可执行工具，请重试".into());
        }
        if !self.substantive
            && self.items.iter().all(|i| i.kind != Kind::Tool)
            && self.reason.is_none()
        {
            return Err("上游流在输出和完成信号之前结束".into());
        }
        self.begin(output);
        for index in 0..self.items.len() {
            self.finish_item(index, output)?;
        }
        if self.api == UpstreamApi::Gemini && !self.signed_gemini_parts.is_empty() {
            let index = self.items.len();
            let item = reasoning_item(
                &format!("rs_sig_{}", self.id),
                "",
                Some(json!({"protocol":"gemini","parts":self.signed_gemini_parts})),
            );
            self.event(
                "response.output_item.added",
                json!({"output_index":index,"item":item}),
                output,
            );
            self.event(
                "response.output_item.done",
                json!({"output_index":index,"item":item}),
                output,
            );
            self.items.push(Item {
                kind: Kind::Reasoning,
                id: String::new(),
                text: String::new(),
                call_id: String::new(),
                name: String::new(),
                arguments: String::new(),
                source: Value::Null,
                added: true,
                done: true,
                output: Some(item),
            });
        }
        let (status, reason) = if truncated {
            ("incomplete", Some("stream_truncated"))
        } else {
            terminal(self.api, self.reason.as_deref())
        };
        let response = envelope(
            &self.id,
            &self.model,
            status,
            self.items.iter().filter_map(|i| i.output.clone()).collect(),
            usage(self.api, Some(&self.usage)),
            reason,
        );
        self.event(
            if status == "completed" {
                "response.completed"
            } else {
                "response.incomplete"
            },
            json!({"response":response}),
            output,
        );
        self.completed = true;
        Ok(())
    }
    fn failed(&mut self, message: &str, output: &mut Vec<u8>) {
        self.begin(output);
        let mut response = envelope(
            &self.id,
            &self.model,
            "failed",
            self.items.iter().filter_map(|i| i.output.clone()).collect(),
            usage(self.api, Some(&self.usage)),
            None,
        );
        response["error"] = json!({"code":"upstream_error","message":message});
        self.event("response.failed", json!({"response":response}), output);
        self.completed = true;
    }
    fn merge_usage(&mut self, usage: &Value) {
        if let Some(map) = usage.as_object() {
            for (k, v) in map {
                self.usage[k] = v.clone();
            }
        }
    }
    fn chunk(&mut self, value: &Value, output: &mut Vec<u8>) -> std::result::Result<(), String> {
        if self.completed {
            return Ok(());
        }
        if let Some(message) = error_message(value) {
            return Err(message);
        }
        match self.api {
            UpstreamApi::ChatCompletions => self.chat(value, output),
            UpstreamApi::AnthropicMessages => self.anthropic(value, output),
            UpstreamApi::Gemini => self.gemini(value, output),
            UpstreamApi::Responses => unreachable!(),
        }
    }
    fn chat(&mut self, value: &Value, output: &mut Vec<u8>) -> std::result::Result<(), String> {
        if !self.started {
            if let Some(id) = value.get("id").and_then(Value::as_str) {
                self.id = response_id(Some(id));
            }
        }
        if let Some(model) = value.get("model").and_then(Value::as_str) {
            self.model = model.into();
        }
        if let Some(u) = value.get("usage") {
            self.merge_usage(u);
        }
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
        else {
            return Ok(());
        };
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(thinking) = chat_reasoning(delta) {
            let index = self.item("chat-thinking".into(), Kind::Reasoning);
            self.text(index, &thinking, output)?;
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            let index = self.item("chat-text".into(), Kind::Text);
            self.text(index, text, output)?;
        }
        if let Some(parts) = delta.get("content").and_then(Value::as_array) {
            for part in parts {
                if matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("text" | "output_text")
                ) {
                    let text = part
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or("Chat SSE 文字内容缺少 text")?;
                    let index = self.item("chat-text".into(), Kind::Text);
                    self.text(index, text, output)?;
                } else {
                    return Err("Chat SSE 含不能转换的非文字内容".into());
                }
            }
        }
        if let Some(refusal) = delta
            .get("refusal")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            return Err(format!("上游拒绝了本次回答：{refusal}"));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for (fallback, call) in calls.iter().enumerate() {
                let n = call
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(fallback as u64);
                let index = self.item(format!("chat-tool-{n}"), Kind::Tool);
                let item = &mut self.items[index];
                if let Some(id) = call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    item.call_id = id.into();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    item.name.push_str(name);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    item.arguments.push_str(args);
                }
            }
        }
        if let Some(call) = delta.get("function_call") {
            let index = self.item("chat-tool-legacy".into(), Kind::Tool);
            let item = &mut self.items[index];
            if let Some(name) = call.get("name").and_then(Value::as_str) {
                item.name.push_str(name);
            }
            if let Some(args) = call.get("arguments").and_then(Value::as_str) {
                item.arguments.push_str(args);
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.reason = Some(reason.into());
        }
        Ok(())
    }
    fn anthropic(
        &mut self,
        value: &Value,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), String> {
        match value.get("type").and_then(Value::as_str).unwrap_or("") {
            "ping" => {}
            "message_start" => {
                if let Some(message) = value.get("message") {
                    if let Some(id) = message.get("id").and_then(Value::as_str) {
                        self.id = response_id(Some(id));
                    }
                    if let Some(model) = message.get("model").and_then(Value::as_str) {
                        self.model = model.into();
                    }
                    if let Some(u) = message.get("usage") {
                        self.merge_usage(u);
                    }
                }
            }
            "content_block_start" => {
                let n = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Anthropic SSE 缺少内容索引")?;
                let block = value
                    .get("content_block")
                    .ok_or("Anthropic SSE 缺少 content_block")?;
                let kind = match block.get("type").and_then(Value::as_str) {
                    Some("text") => Kind::Text,
                    Some("thinking" | "redacted_thinking") => Kind::Reasoning,
                    Some("tool_use") => Kind::Tool,
                    _ => return Err("无法转换 Anthropic SSE 内容类型".into()),
                };
                let index = self.item(format!("anth-{n}"), kind);
                self.items[index].source = block.clone();
                if kind == Kind::Tool {
                    self.items[index].call_id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or("Anthropic SSE 工具缺少 id")?
                        .into();
                    self.items[index].name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into();
                    if block
                        .get("input")
                        .and_then(Value::as_object)
                        .is_some_and(|m| !m.is_empty())
                    {
                        self.items[index].arguments = block["input"].to_string();
                    }
                } else {
                    let text = block
                        .get(if kind == Kind::Text {
                            "text"
                        } else {
                            "thinking"
                        })
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    self.text(index, text, output)?;
                }
            }
            "content_block_delta" => {
                let n = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Anthropic SSE 缺少内容索引")?;
                let index = *self
                    .keys
                    .get(&format!("anth-{n}"))
                    .ok_or("Anthropic SSE delta 缺少对应 start")?;
                let delta = value.get("delta").ok_or("Anthropic SSE 缺少 delta")?;
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta" | "thinking_delta") => {
                        let text = delta
                            .get(if self.items[index].kind == Kind::Text {
                                "text"
                            } else {
                                "thinking"
                            })
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        self.text(index, text, output)?;
                        let key = if self.items[index].kind == Kind::Text {
                            "text"
                        } else {
                            "thinking"
                        };
                        let text = self.items[index].text.clone();
                        self.items[index].source[key] = json!(text);
                    }
                    Some("input_json_delta") => {
                        self.items[index].arguments.push_str(
                            delta
                                .get("partial_json")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                        );
                    }
                    Some("signature_delta") => {
                        let old = self.items[index]
                            .source
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_owned();
                        self.items[index].source["signature"] = json!(
                            old + delta.get("signature").and_then(Value::as_str).unwrap_or("")
                        );
                    }
                    _ => return Err("无法转换 Anthropic SSE delta 类型".into()),
                }
            }
            "content_block_stop" => {
                let n = value
                    .get("index")
                    .and_then(Value::as_u64)
                    .ok_or("Anthropic SSE 缺少索引")?;
                let index = *self
                    .keys
                    .get(&format!("anth-{n}"))
                    .ok_or("Anthropic SSE stop 缺少 start")?;
                self.finish_item(index, output)?;
            }
            "message_delta" => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.reason = Some(reason.into());
                }
                if let Some(u) = value.get("usage") {
                    self.merge_usage(u);
                }
            }
            "message_stop" => {
                if self.reason.is_none() {
                    return Err("Anthropic SSE 缺少 stop_reason".into());
                }
                self.finish(false, output)?;
            }
            "error" => {
                return Err(error_message(value).unwrap_or_else(|| "Anthropic 流错误".into()))
            }
            _ => return Err("无法识别 Anthropic SSE 事件".into()),
        }
        Ok(())
    }
    fn gemini(&mut self, value: &Value, output: &mut Vec<u8>) -> std::result::Result<(), String> {
        if let Some(block) = value
            .pointer("/promptFeedback/blockReason")
            .and_then(Value::as_str)
        {
            return Err(format!("Gemini 拒绝了输入：{block}"));
        }
        if !self.started {
            if let Some(id) = value.get("responseId").and_then(Value::as_str) {
                self.id = response_id(Some(id));
            }
        }
        if let Some(model) = value.get("modelVersion").and_then(Value::as_str) {
            self.model = model.into();
        }
        if let Some(u) = value.get("usageMetadata") {
            self.merge_usage(u);
        }
        let Some(candidate) = value
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|v| v.first())
        else {
            return Ok(());
        };
        if let Some(parts) = candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
                    let index = self.item(
                        format!(
                            "{}-{}",
                            if thought { "gem-thinking" } else { "gem-text" },
                            self.gemini_segment
                        ),
                        if thought { Kind::Reasoning } else { Kind::Text },
                    );
                    if thought {
                        self.items[index].source = part.clone();
                    }
                    self.text(index, text, output)?;
                    if part.get("thoughtSignature").is_some() {
                        if !thought {
                            let mut signed = part.clone();
                            signed["text"] = json!(self.items[index].text);
                            signed["codex_item_id"] = json!(self.items[index].id);
                            self.signed_gemini_parts.push(signed);
                        }
                        self.finish_item(index, output)?;
                        self.gemini_segment += 1;
                    }
                } else if let Some(call) = part.get("functionCall") {
                    let n = self.items.len();
                    let index = self.item(format!("gem-tool-{n}"), Kind::Tool);
                    self.items[index].name = call
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into();
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        self.items[index].call_id = id.into();
                    }
                    self.items[index].arguments = call
                        .get("args")
                        .cloned()
                        .unwrap_or_else(|| json!({}))
                        .to_string();
                    // Give signature replay the same call id as Codex's tool item.
                    if part.get("thoughtSignature").is_some() {
                        let mut signed = part.clone();
                        signed["functionCall"]["id"] = json!(self.items[index].call_id);
                        self.signed_gemini_parts.push(signed);
                    }
                    if !self.context.single_tool {
                        self.finish_item(index, output)?;
                    }
                } else {
                    return Err("Gemini 流返回了无法转换的多模态输出".into());
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            self.reason = Some(reason.into());
            self.finish(false, output)?;
        }
        Ok(())
    }
}

pub(crate) struct ResponseStream<R: Read> {
    upstream: R,
    state: State,
    input: Vec<u8>,
    pending: Vec<u8>,
    offset: usize,
    eof: bool,
    emitted: bool,
    failed: Arc<AtomicBool>,
    json_mode: Option<bool>,
}

impl<R: Read> ResponseStream<R> {
    pub(crate) fn new(upstream: R, api: UpstreamApi, context: ToolContext, model: String) -> Self {
        Self {
            upstream,
            state: State::new(api, context, model),
            input: Vec::new(),
            pending: Vec::new(),
            offset: 0,
            eof: false,
            emitted: false,
            failed: Arc::new(AtomicBool::new(false)),
            json_mode: None,
        }
    }
    pub(crate) fn failure_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.failed)
    }
    fn failure(&mut self, error: String) -> io::Result<()> {
        if self.state.completed {
            self.eof = true;
            return Ok(());
        }
        self.eof = true;
        self.failed.store(true, Ordering::Release);
        if !self.emitted {
            self.pending.clear();
            return Err(io::Error::new(io::ErrorKind::InvalidData, error));
        }
        self.state.failed(&error, &mut self.pending);
        // A failed Responses event is a complete transport response. Close the
        // HTTP chunks cleanly so Codex can consume that explicit error event.
        Ok(())
    }
    fn block(&mut self, bytes: &[u8]) -> std::result::Result<(), String> {
        if self.state.completed {
            return Ok(());
        }
        let block = std::str::from_utf8(bytes).map_err(|_| "上游 SSE 不是有效的 UTF-8")?;
        let mut data = Vec::new();
        let mut event = None;
        for line in block.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(v) = line.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v));
            }
            if let Some(v) = line.strip_prefix("event:") {
                event = Some(v.trim());
            }
        }
        if data.is_empty() {
            return Ok(());
        }
        let data = data.join("\n");
        if data.trim() == "[DONE]" {
            if self.state.reason.is_none() {
                return Err("Chat SSE [DONE] 前缺少 finish_reason".into());
            }
            return self.state.finish(false, &mut self.pending);
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|_| "上游 SSE data 不是有效的 JSON")?;
        if event == Some("error") {
            return Err(error_message(&value).unwrap_or_else(|| value.to_string()));
        }
        self.state.chunk(&value, &mut self.pending)
    }
    fn json(&mut self) -> std::result::Result<(), String> {
        let value: Value =
            serde_json::from_slice(&self.input).map_err(|_| "上游未返回有效的 JSON 或 SSE")?;
        let response = convert_response(
            self.state.api,
            &value,
            &self.state.context,
            &self.state.model,
        )?;
        self.state.id = response["id"].as_str().unwrap_or(&self.state.id).into();
        self.state.model = response["model"]
            .as_str()
            .unwrap_or(&self.state.model)
            .into();
        self.state.begin(&mut self.pending);
        for (index, item) in response["output"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut initial = item.clone();
            match item["type"].as_str() {
                Some("message") => {
                    initial["status"] = json!("in_progress");
                    initial["content"] = json!([]);
                }
                Some("function_call") => {
                    initial["status"] = json!("in_progress");
                    initial["arguments"] = json!("");
                }
                Some("custom_tool_call") => {
                    initial["status"] = json!("in_progress");
                    initial["input"] = json!("");
                }
                Some("reasoning") => {
                    initial["summary"] = json!([]);
                    initial.as_object_mut().unwrap().remove("encrypted_content");
                }
                _ => {}
            }
            self.state.event(
                "response.output_item.added",
                json!({"output_index":index,"item":initial}),
                &mut self.pending,
            );
            match item["type"].as_str() {
                Some("message") => {
                    for (content_index, part) in
                        item["content"].as_array().into_iter().flatten().enumerate()
                    {
                        let mut initial_part = part.clone();
                        if part["type"] == "output_text" {
                            initial_part["text"] = json!("");
                        }
                        if part["type"] == "refusal" {
                            initial_part["refusal"] = json!("");
                        }
                        self.state.event("response.content_part.added",json!({"item_id":item["id"],"output_index":index,"content_index":content_index,"part":initial_part}),&mut self.pending);
                        if part["type"] == "output_text" {
                            self.state.event("response.output_text.delta",json!({"item_id":item["id"],"output_index":index,"content_index":content_index,"delta":part["text"]}),&mut self.pending);
                            self.state.event("response.output_text.done",json!({"item_id":item["id"],"output_index":index,"content_index":content_index,"text":part["text"]}),&mut self.pending);
                        }
                        self.state.event("response.content_part.done",json!({"item_id":item["id"],"output_index":index,"content_index":content_index,"part":part}),&mut self.pending);
                    }
                }
                Some("function_call") => {
                    self.state.event("response.function_call_arguments.delta",json!({"item_id":item["id"],"output_index":index,"delta":item["arguments"]}),&mut self.pending);
                    self.state.event("response.function_call_arguments.done",json!({"item_id":item["id"],"output_index":index,"arguments":item["arguments"]}),&mut self.pending);
                }
                Some("custom_tool_call") => {
                    self.state.event(
                        "response.custom_tool_call_input.delta",
                        json!({"item_id":item["id"],"output_index":index,"delta":item["input"]}),
                        &mut self.pending,
                    );
                    self.state.event(
                        "response.custom_tool_call_input.done",
                        json!({"item_id":item["id"],"output_index":index,"input":item["input"]}),
                        &mut self.pending,
                    );
                }
                _ => {}
            }
            self.state.event(
                "response.output_item.done",
                json!({"output_index":index,"item":item}),
                &mut self.pending,
            );
        }
        self.state.event(
            if response["status"] == "completed" {
                "response.completed"
            } else {
                "response.incomplete"
            },
            json!({"response":response}),
            &mut self.pending,
        );
        self.state.completed = true;
        self.state.substantive = true;
        Ok(())
    }
}

impl<R: Read> Read for ResponseStream<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if self.offset < self.pending.len()
                && (self.state.substantive || self.state.completed || self.emitted)
            {
                let count = (self.pending.len() - self.offset).min(output.len());
                output[..count].copy_from_slice(&self.pending[self.offset..self.offset + count]);
                self.offset += count;
                self.emitted = true;
                return Ok(count);
            }
            if self.offset == self.pending.len() {
                self.pending.clear();
                self.offset = 0;
            }
            if self.state.completed {
                return Ok(0);
            }
            if self.eof {
                return Ok(0);
            }
            let mut buffer = [0u8; 16 * 1024];
            let count = match self.upstream.read(&mut buffer) {
                Ok(count) => count,
                Err(error) => {
                    self.failure(format!("上游流读取失败：{error}"))?;
                    continue;
                }
            };
            if count == 0 {
                self.eof = true;
                let result = if self.json_mode == Some(true) {
                    self.json()
                } else {
                    let tail = std::mem::take(&mut self.input);
                    if !tail.iter().all(u8::is_ascii_whitespace) {
                        self.block(&tail)
                    } else {
                        Ok(())
                    }
                    .and_then(|()| {
                        self.state
                            .finish(self.state.reason.is_none(), &mut self.pending)
                    })
                };
                if let Err(error) = result {
                    self.failure(error)?;
                }
                continue;
            }
            self.input.extend_from_slice(&buffer[..count]);
            if self.input.len() > MAX_FRAME {
                self.failure("上游 SSE 单个事件超过 64 MiB".into())?;
                continue;
            }
            if self.json_mode.is_none() {
                if let Some(first) = self
                    .input
                    .iter()
                    .copied()
                    .find(|b| !b.is_ascii_whitespace())
                {
                    self.json_mode = Some(matches!(first, b'{' | b'['));
                }
            }
            if self.json_mode == Some(true) {
                continue;
            }
            while let Some((end, skip)) = frame_end(&self.input) {
                let bytes = self.input[..end].to_vec();
                self.input.drain(..end + skip);
                if let Err(error) = self.block(&bytes) {
                    self.failure(error)?;
                    break;
                }
            }
        }
    }
}

fn frame_end(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in 0..bytes.len() {
        if bytes.get(index..index + 2) == Some(b"\n\n") {
            return Some((index, 2));
        }
        if bytes.get(index..index + 4) == Some(b"\r\n\r\n") {
            return Some((index, 4));
        }
    }
    None
}
