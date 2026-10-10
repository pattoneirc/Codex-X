use super::*;
use std::io::{self, Read};

fn prepared(api: UpstreamApi, body: Value) -> PreparedRequest {
    prepare(api, &serde_json::to_vec(&body).unwrap(), false).unwrap()
}
fn request(api: UpstreamApi, body: Value) -> Value {
    serde_json::from_slice(&prepared(api, body).bytes).unwrap()
}
fn events(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

struct ByteReader(Vec<u8>, usize);
impl Read for ByteReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.1 == self.0.len() {
            return Ok(0);
        }
        output[0] = self.0[self.1];
        self.1 += 1;
        Ok(1)
    }
}
fn streamed(api: UpstreamApi, input: &str, context: ToolContext) -> String {
    let mut reader = ResponseStream::new(
        ByteReader(input.as_bytes().to_vec(), 0),
        api,
        context,
        "fixture".into(),
    );
    let mut text = String::new();
    reader.read_to_string(&mut text).unwrap();
    text
}

#[test]
fn request_conversions_preserve_roles_media_tools_and_parallel_results() {
    let body = json!({"model":"fixture","instructions":"system instruction","max_output_tokens":7000,"input":[
        {"role":"developer","content":"developer instruction"},
        {"role":"user","content":[{"type":"input_text","text":"inspect this"},{"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}]},
        {"type":"function_call","call_id":"call-1","namespace":"functions","name":"read","arguments":"{\"path\":\"x\"}"},
        {"type":"custom_tool_call","call_id":"call-2","name":"apply_patch","input":"*** Begin Patch\n*** End Patch"},
        {"type":"function_call_output","call_id":"call-1","output":"file text"},
        {"type":"custom_tool_call_output","call_id":"call-2","output":[{"type":"input_text","text":"patched"},{"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}]}
    ],"tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"read","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}]},{"type":"custom","name":"apply_patch","description":"Apply a patch","format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}]});
    let chat = request(UpstreamApi::ChatCompletions, body.clone());
    assert_eq!(chat["messages"][0]["role"], "system");
    assert_eq!(chat["messages"][1]["role"], "system");
    assert_eq!(
        chat["messages"][2]["content"][1]["image_url"]["url"],
        "data:image/png;base64,aGVsbG8="
    );
    assert_eq!(
        chat["messages"][3]["tool_calls"].as_array().unwrap().len(),
        2
    );
    assert_eq!(chat["messages"][4]["tool_call_id"], "call-1");
    assert_eq!(chat["messages"][5]["tool_call_id"], "call-2");
    assert_eq!(chat["messages"][6]["role"], "user");
    assert!(chat["tools"][0]["function"]["name"]
        .as_str()
        .unwrap()
        .starts_with("functions__read_"));
    assert!(chat["tools"][1]["function"]["description"]
        .as_str()
        .unwrap()
        .contains("grammar"));
    let anthropic = request(UpstreamApi::AnthropicMessages, body.clone());
    assert!(anthropic["system"]
        .as_str()
        .unwrap()
        .contains("developer instruction"));
    assert_eq!(anthropic["max_tokens"], 7000);
    assert_eq!(
        anthropic["messages"][0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(
        anthropic["messages"][1]["content"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        anthropic["messages"][2]["content"][1]["content"][1]["type"],
        "image"
    );
    let gemini = request(UpstreamApi::Gemini, body);
    assert_eq!(
        gemini["systemInstruction"]["parts"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        gemini["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        gemini["contents"][1]["parts"][0]["functionCall"]["id"],
        "call-1"
    );
    assert_eq!(
        gemini["contents"][2]["parts"][0]["functionResponse"]["id"],
        "call-1"
    );
    assert_eq!(
        gemini["contents"][2]["parts"][0]["functionResponse"]["name"],
        gemini["contents"][1]["parts"][0]["functionCall"]["name"]
    );
}

#[test]
fn unsupported_responses_server_state_is_explicit_and_native_bytes_are_unchanged() {
    for api in [
        UpstreamApi::ChatCompletions,
        UpstreamApi::AnthropicMessages,
        UpstreamApi::Gemini,
    ] {
        for body in [
            json!({"model":"fixture","input":"x","previous_response_id":"resp-remote"}),
            json!({"model":"fixture","input":[{"type":"reasoning","encrypted_content":"opaque-upstream-ciphertext"}]}),
            json!({"model":"fixture","input":[{"type":"item_reference","id":"remote-item"}]}),
        ] {
            let bytes = serde_json::to_vec(&body).unwrap();
            assert!(prepare(api, &bytes, false).is_err());
            assert_eq!(
                prepare(UpstreamApi::Responses, &bytes, false)
                    .unwrap()
                    .bytes,
                bytes
            );
        }
        assert!(prepare(api, br#"{"model":"fixture","input":"x"}"#, true)
            .unwrap_err()
            .contains("compact"));
    }
}

#[test]
fn namespace_and_custom_tools_restore_original_codex_shapes() {
    let prepared = prepared(
        UpstreamApi::ChatCompletions,
        json!({"model":"fixture","input":"x","tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"read"}]},{"type":"custom","name":"apply_patch"}]}),
    );
    let name = prepared.context.upstream_name("read", Some("functions"));
    let body = json!({"id":"chat-1","choices":[{"finish_reason":"tool_calls","message":{"tool_calls":[{"id":"a","function":{"name":name,"arguments":"{}"}},{"id":"b","function":{"name":"apply_patch","arguments":"{\"input\":\"patch text\"}"}}]}}],"usage":{"prompt_tokens":3,"completion_tokens":4}});
    let converted = convert_response(
        UpstreamApi::ChatCompletions,
        &body,
        &prepared.context,
        "fixture",
    )
    .unwrap();
    assert_eq!(converted["output"][0]["namespace"], "functions");
    assert_eq!(converted["output"][0]["name"], "read");
    assert_eq!(converted["output"][1]["type"], "custom_tool_call");
    assert_eq!(converted["output"][1]["input"], "patch text");
    assert_eq!(converted["usage"]["total_tokens"], 7);
}

#[test]
fn chat_sse_crosses_utf8_crlf_and_fragmented_function_json() {
    let context = prepared(
        UpstreamApi::ChatCompletions,
        json!({"model":"fixture","input":"x","tools":[{"type":"function","name":"read"}]}),
    )
    .context;
    let input=concat!("data: {\"id\":\"c\",\"choices\":[{\"delta\":{\"content\":\"中文🙂\"}}]}\r\n\r\n", "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-x\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"x\\\":\"}}]}}]}\r\n\r\n", "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"1}\"}}]},\"finish_reason\":\"tool_calls\"}]}\r\n\r\n", "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\r\n\r\n", "data: [DONE]\r\n\r\n");
    let output = streamed(UpstreamApi::ChatCompletions, input, context);
    let events = events(&output);
    assert!(events
        .iter()
        .any(|v| v["type"] == "response.output_text.delta" && v["delta"] == "中文🙂"));
    let response = &events.last().unwrap()["response"];
    assert_eq!(response["status"], "completed");
    assert_eq!(response["output"][1]["call_id"], "call-x");
    assert_eq!(response["output"][1]["arguments"], "{\"x\":1}");
    assert_eq!(response["usage"]["total_tokens"], 7);
}

#[test]
fn stream_error_before_output_returns_error_and_after_output_emits_failed() {
    let error = "event: error\ndata: {\"error\":{\"message\":\"fixture outage\"}}\n\n";
    let mut early = ResponseStream::new(
        io::Cursor::new(error),
        UpstreamApi::ChatCompletions,
        ToolContext::default(),
        "fixture".into(),
    );
    let mut bytes = Vec::new();
    assert!(early.read_to_end(&mut bytes).is_err());
    assert!(bytes.is_empty());
    let output = streamed(
        UpstreamApi::ChatCompletions,
        &format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"partial\"}}}}]}}\n\n{error}"),
        ToolContext::default(),
    );
    let events = events(&output);
    assert_eq!(events.last().unwrap()["type"], "response.failed");
    assert!(events.iter().all(|v| v["type"] != "response.completed"));
}

#[test]
fn stream_eof_without_terminal_signal_is_incomplete() {
    for (api, input) in [
        (
            UpstreamApi::ChatCompletions,
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ),
        (
            UpstreamApi::Gemini,
            "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"partial\"}]}}]}\n\n",
        ),
    ] {
        let output = streamed(api, input, ToolContext::default());
        let events = events(&output);
        assert_eq!(events.last().unwrap()["type"], "response.incomplete");
        assert_eq!(
            events.last().unwrap()["response"]["incomplete_details"]["reason"],
            "stream_truncated"
        );
    }
}

#[test]
fn anthropic_thinking_signature_and_gemini_tool_signature_round_trip() {
    let ctx = prepared(
        UpstreamApi::Gemini,
        json!({"model":"fixture","input":"x","tools":[{"type":"function","name":"read"}]}),
    )
    .context;
    let anthropic = json!({"id":"m","type":"message","content":[{"type":"thinking","thinking":"analysis","signature":"signed-thinking"},{"type":"tool_use","id":"call-a","name":"read","input":{}}],"stop_reason":"tool_use","usage":{"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":5}});
    let response =
        convert_response(UpstreamApi::AnthropicMessages, &anthropic, &ctx, "fixture").unwrap();
    assert_eq!(response["usage"]["input_tokens"], 7);
    let mut history = vec![json!({"role":"user","content":"inspect"})];
    history.extend(response["output"].as_array().unwrap().clone());
    history.push(json!({"type":"function_call_output","call_id":"call-a","output":"done"}));
    let anthropic_req = request(
        UpstreamApi::AnthropicMessages,
        json!({"model":"fixture","input":history}),
    );
    assert_eq!(
        anthropic_req["messages"][1]["content"][0]["signature"],
        "signed-thinking"
    );
    let gemini = json!({"responseId":"g","candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}},"thoughtSignature":"signed-function"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":3,"thoughtsTokenCount":4}});
    let response = convert_response(UpstreamApi::Gemini, &gemini, &ctx, "fixture").unwrap();
    let call = response["output"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "function_call")
        .unwrap();
    let mut history = vec![json!({"role":"user","content":"inspect"})];
    history.extend(response["output"].as_array().unwrap().clone());
    history.push(json!({"type":"function_call_output","call_id":call["call_id"],"output":"done"}));
    let request = request(
        UpstreamApi::Gemini,
        json!({"model":"fixture","input":history}),
    );
    assert_eq!(request["contents"][1]["parts"].as_array().unwrap().len(), 1);
    assert_eq!(
        request["contents"][1]["parts"][0]["thoughtSignature"],
        "signed-function"
    );
    assert_eq!(response["usage"]["output_tokens"], 7);
}

#[test]
fn stream_json_fallback_and_length_reason_are_truthful() {
    let input = r#"{"id":"chat","choices":[{"message":{"content":"truncated"},"finish_reason":"length"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#;
    let output = streamed(UpstreamApi::ChatCompletions, input, ToolContext::default());
    let events = events(&output);
    assert_eq!(events.last().unwrap()["type"], "response.incomplete");
    assert_eq!(
        events.last().unwrap()["response"]["incomplete_details"]["reason"],
        "max_output_tokens"
    );
}

#[test]
fn gemini_signed_text_chunks_replay_on_the_matching_assistant_message() {
    let input="data: {\"responseId\":\"g\",\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hello \"}]}}]}\n\ndata: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"world\",\"thoughtSignature\":\"signature\"}]},\"finishReason\":\"STOP\"}]}\n\n";
    let output = streamed(UpstreamApi::Gemini, input, ToolContext::default());
    let ev = events(&output);
    let response = &ev.last().unwrap()["response"];
    let mut history = vec![json!({"role":"user","content":"hello world"})];
    history.extend(response["output"].as_array().unwrap().clone());
    history.push(json!({"role":"user","content":"continue"}));
    let converted = request(
        UpstreamApi::Gemini,
        json!({"model":"gemini-2.5-pro","input":history}),
    );
    assert!(converted["contents"][0]["parts"][0]
        .get("thoughtSignature")
        .is_none());
    assert_eq!(converted["contents"][1]["parts"][0]["text"], "hello world");
    assert_eq!(
        converted["contents"][1]["parts"][0]["thoughtSignature"],
        "signature"
    );
    let mut history = vec![json!({"role":"user","content":"OK"})];
    for (id, sig) in [("g1", "sig1"), ("g2", "sig2")] {
        let converted=convert_response(UpstreamApi::Gemini,&json!({"responseId":id,"candidates":[{"content":{"parts":[{"text":"OK","thoughtSignature":sig}]},"finishReason":"STOP"}]}),&ToolContext::default(),"fixture").unwrap();
        history.extend(converted["output"].as_array().unwrap().clone());
        history.push(json!({"role":"user","content":"OK"}));
    }
    let converted = request(
        UpstreamApi::Gemini,
        json!({"model":"fixture","input":history}),
    );
    assert_eq!(
        converted["contents"][1]["parts"][0]["thoughtSignature"],
        "sig1"
    );
    assert_eq!(
        converted["contents"][3]["parts"][0]["thoughtSignature"],
        "sig2"
    );
}

#[test]
fn gemini_single_tool_constraint_buffers_tools_and_rejects_parallel_execution() {
    let prepared = prepared(
        UpstreamApi::Gemini,
        json!({"model":"gemini-2.5-pro","input":"x","parallel_tool_calls":false,"tools":[{"type":"function","name":"read"}]}),
    );
    let request: Value = serde_json::from_slice(&prepared.bytes).unwrap();
    assert!(request["systemInstruction"]["parts"][0]["text"]
        .as_str()
        .unwrap()
        .contains("at most one"));
    let response = json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{}}},{"functionCall":{"name":"read","args":{}}}]},"finishReason":"STOP"}]});
    assert!(
        convert_response(UpstreamApi::Gemini, &response, &prepared.context, "fixture").is_err()
    );
    let input = format!("data: {response}\n\n");
    let mut stream = ResponseStream::new(
        ByteReader(input.into_bytes(), 0),
        UpstreamApi::Gemini,
        prepared.context.clone(),
        "fixture".into(),
    );
    let mut bytes = Vec::new();
    assert!(stream.read_to_end(&mut bytes).is_err());
    assert!(bytes.is_empty());
    let input=format!("data: {{\"candidates\":[{{\"content\":{{\"parts\":[{{\"text\":\"planning\"}}]}}}}]}}\n\ndata: {response}\n\n");
    let output = streamed(UpstreamApi::Gemini, &input, prepared.context.clone());
    let ev = events(&output);
    assert_eq!(ev.last().unwrap()["type"], "response.failed");
    assert!(ev
        .iter()
        .all(|v| v["type"] != "response.output_item.done" || v["item"]["type"] != "function_call"));
    let single="data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read\",\"args\":{}}}]},\"finishReason\":\"STOP\"}]}\n\n";
    let output = streamed(UpstreamApi::Gemini, single, prepared.context);
    assert_eq!(
        events(&output).last().unwrap()["type"],
        "response.completed"
    );
}

#[test]
fn chat_parallel_results_keep_media_after_all_tool_responses_and_reasoning_on_call() {
    let body = json!({"model":"fixture","input":[{"role":"user","content":"x"},{"type":"reasoning","summary":[{"text":"analysis"}]},{"role":"assistant","content":"Let me check"},{"type":"function_call","call_id":"a","name":"read","arguments":"{}"},{"type":"function_call","call_id":"b","name":"read","arguments":"{}"},{"type":"function_call_output","call_id":"a","output":[{"type":"input_image","image_url":"data:image/png;base64,aGVsbG8="}]},{"type":"function_call_output","call_id":"b","output":"done"}]});
    let converted = request(UpstreamApi::ChatCompletions, body);
    let messages = converted["messages"].as_array().unwrap();
    assert_eq!(messages[1]["reasoning_content"], "analysis");
    assert_eq!(messages[1]["content"][0]["text"], "Let me check");
    assert_eq!(messages[1]["tool_calls"].as_array().unwrap().len(), 2);
    assert_eq!(messages[2]["tool_call_id"], "a");
    assert_eq!(messages[3]["tool_call_id"], "b");
    assert_eq!(messages[4]["role"], "user");
}

#[test]
fn structured_output_and_unsupported_parameters_are_explicit() {
    let body = json!({"model":"fixture","input":"x","text":{"format":{"type":"json_schema","name":"test","strict":true,"schema":{"type":"object","properties":{"ok":{"type":"boolean"}}}}}});
    let chat = request(UpstreamApi::ChatCompletions, body.clone());
    assert_eq!(chat["response_format"]["json_schema"]["strict"], true);
    let anth = request(UpstreamApi::AnthropicMessages, body.clone());
    assert_eq!(anth["output_config"]["format"]["schema"]["type"], "object");
    let gem = request(UpstreamApi::Gemini, body);
    assert_eq!(
        gem["generationConfig"]["responseMimeType"],
        "application/json"
    );
    assert_eq!(
        gem["generationConfig"]["responseJsonSchema"]["type"],
        "object"
    );
    for body in [
        json!({"model":"fixture","input":"x","unknown_setting":1}),
        json!({"model":"fixture","input":"x","tools":{}}),
        json!({"model":"fixture","input":"x","tool_choice":"required"}),
        json!({"model":"fixture","input":"x","tools":[{"type":"function","name":"read"},{"type":"function","name":"read"}]}),
    ] {
        assert!(prepare(
            UpstreamApi::ChatCompletions,
            &serde_json::to_vec(&body).unwrap(),
            false
        )
        .is_err());
    }
    assert!(prepare(
        UpstreamApi::AnthropicMessages,
        &serde_json::to_vec(
            &json!({"model":"fixture","input":"x","temperature":0.2,"reasoning":{"effort":"high"}})
        )
        .unwrap(),
        false
    )
    .unwrap_err()
    .contains("temperature"));
    let ultra = request(
        UpstreamApi::AnthropicMessages,
        json!({"model":"fixture","input":"x","max_output_tokens":80000,"reasoning":{"effort":"ultra"}}),
    );
    assert_eq!(ultra["thinking"]["budget_tokens"], 32768);
}

#[test]
fn terminal_sse_does_not_consume_late_error_or_transport_reset() {
    struct ResetAfter(Vec<Vec<u8>>, usize);
    impl Read for ResetAfter {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.1 == self.0.len() {
                return Err(io::Error::new(io::ErrorKind::ConnectionReset, "late reset"));
            }
            let chunk = &self.0[self.1];
            bytes[..chunk.len()].copy_from_slice(chunk);
            self.1 += 1;
            Ok(chunk.len())
        }
    }
    let chunks = vec![
        b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n".to_vec(),
        b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
            .to_vec(),
        b"event: error\ndata: {\"error\":{\"message\":\"late\"}}\n\n".to_vec(),
    ];
    let mut stream = ResponseStream::new(
        ResetAfter(chunks, 0),
        UpstreamApi::ChatCompletions,
        ToolContext::default(),
        "fixture".into(),
    );
    let mut output = String::new();
    stream.read_to_string(&mut output).unwrap();
    let ev = events(&output);
    assert_eq!(
        ev.iter()
            .filter(|v| v["type"] == "response.completed")
            .count(),
        1
    );
    assert!(ev.iter().all(|v| v["type"] != "response.failed"));
}

#[test]
fn json_stream_fallback_has_empty_added_items_before_deltas() {
    let input = r#"{"id":"chat","choices":[{"message":{"content":"hello","tool_calls":[{"id":"a","function":{"name":"read","arguments":"{\"x\":1}"}}]},"finish_reason":"tool_calls"}]}"#;
    let output = streamed(UpstreamApi::ChatCompletions, input, ToolContext::default());
    let ev = events(&output);
    let added = ev
        .iter()
        .filter(|v| v["type"] == "response.output_item.added")
        .collect::<Vec<_>>();
    assert_eq!(added[0]["item"]["status"], "in_progress");
    assert_eq!(added[0]["item"]["content"], json!([]));
    assert_eq!(added[1]["item"]["arguments"], "");
    let part = ev
        .iter()
        .find(|v| v["type"] == "response.content_part.added")
        .unwrap();
    assert_eq!(part["part"]["text"], "");
}
