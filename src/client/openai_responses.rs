use super::*;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::RequestBuilder;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

const API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_TIMEOUT_SECONDS: u64 = 180;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenAIResponsesConfig {
    pub name: Option<String>,
    pub api_base: Option<String>,
    pub api_key: Option<String>,
    pub organization_id: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelData>,
    pub extra: Option<ExtraConfig>,
    pub patch: Option<RequestPatch>,
    pub timeout_seconds: Option<u64>,
}

impl OpenAIResponsesClient {
    fn get_api_base(&self) -> Option<String> {
        let name = Self::name(&self.config);
        std::env::var(crate::utils::get_env_name(&format!("{name}_api_base")))
            .ok()
            .or_else(|| self.config.api_base.clone())
    }

    fn get_api_key(&self) -> Result<String> {
        let name = Self::name(&self.config);
        std::env::var(format!("{name}_API_KEY").to_ascii_uppercase())
            .ok()
            .or_else(|| self.config.api_key.clone())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| anyhow!("Miss 'api_key'"))
    }

    pub const PROMPTS: [PromptAction<'static>; 1] = [("api_key", "API Key", None)];
}

#[async_trait::async_trait]
impl Client for OpenAIResponsesClient {
    client_common_fns!();

    async fn chat_completions_inner(
        &self,
        client: &reqwest::Client,
        data: ChatCompletionsData,
    ) -> Result<ChatCompletionsOutput> {
        let request = prepare_chat_completions(self, data)?;
        let builder = self.build_responses_request(client, request)?;
        responses_chat_completions(builder, self.model()).await
    }

    async fn chat_completions_streaming_inner(
        &self,
        client: &reqwest::Client,
        handler: &mut SseHandler,
        data: ChatCompletionsData,
    ) -> Result<()> {
        let request = prepare_chat_completions(self, data)?;
        let builder = self.build_responses_request(client, request)?;
        responses_chat_completions_streaming(builder, handler, self.model()).await
    }
}
impl OpenAIResponsesClient {
    fn organization_id(&self) -> Option<String> {
        let name = Self::name(&self.config);
        std::env::var(crate::utils::get_env_name(&format!(
            "{name}_organization_id"
        )))
        .ok()
        .or_else(|| self.config.organization_id.clone())
    }

    fn timeout_seconds(&self) -> Result<u64> {
        let name = Self::name(&self.config);
        let env_name = crate::utils::get_env_name(&format!("{name}_timeout_seconds"));
        let timeout = match std::env::var(&env_name) {
            Ok(value) => value
                .parse::<u64>()
                .with_context(|| format!("{env_name} must be an integer number of seconds"))?,
            Err(std::env::VarError::NotPresent) => self
                .config
                .timeout_seconds
                .unwrap_or(DEFAULT_TIMEOUT_SECONDS),
            Err(error) => return Err(error).with_context(|| format!("Failed reading {env_name}")),
        };
        if !(1..=3600).contains(&timeout) {
            bail!("{env_name} must be between 1 and 3600 seconds");
        }
        Ok(timeout)
    }

    fn build_responses_request(
        &self,
        client: &reqwest::Client,
        mut request: RequestData,
    ) -> Result<RequestBuilder> {
        let timeout = self.timeout_seconds()?;
        let protected_fields = [
            "store",
            "input",
            "instructions",
            "stream",
            "model",
            "tools",
            "previous_response_id",
            "conversation",
            "background",
        ];
        let mut protected_values: [Option<Value>; 9] = std::array::from_fn(|_| None);
        {
            let body = request
                .body
                .as_object_mut()
                .ok_or_else(|| anyhow!("Responses request body must be an object"))?;
            for (index, field) in protected_fields.iter().enumerate() {
                protected_values[index] = body.remove(*field);
            }
        }
        <Self as Client>::patch_request_data(self, &mut request);
        {
            let body = request
                .body
                .as_object_mut()
                .ok_or_else(|| anyhow!("Request patch replaced the Responses request body"))?;
            if let Some(field) = protected_fields
                .iter()
                .find(|field| body.contains_key(**field))
            {
                bail!("Request patches cannot change Responses field '{field}'");
            }
            for (index, field) in protected_fields.iter().enumerate() {
                if let Some(value) = protected_values[index].take() {
                    body.insert((*field).to_owned(), value);
                }
            }
        }
        if request.body["store"] != false {
            bail!("Responses requests must use store:false");
        }
        let mut builder = client.post(request.url);
        for (name, value) in request.headers {
            builder = builder.header(name, value);
        }
        Ok(builder
            .json(&request.body)
            .timeout(Duration::from_secs(timeout)))
    }
}

async fn responses_chat_completions(
    builder: RequestBuilder,
    _model: &Model,
) -> Result<ChatCompletionsOutput> {
    let response = builder.send().await.context("Responses request failed")?;
    let status = response.status();
    let data: Value = response
        .json()
        .await
        .context("Invalid Responses API JSON response")?;
    catch_error(&data, status.as_u16())?;
    extract_response(&data)
}

async fn responses_chat_completions_streaming(
    builder: RequestBuilder,
    handler: &mut SseHandler,
    _model: &Model,
) -> Result<()> {
    let mut state = StreamState::default();
    sse_stream(builder, |event| state.process(event, handler)).await?;
    if !state.completed {
        bail!("Responses API stream ended without response.completed")
    }
    let mut final_response = state
        .final_response
        .take()
        .ok_or_else(|| anyhow!("Responses API stream completed without a final response object"))?;
    if !final_response["output"].is_array() {
        let mut items: Vec<_> = state.output_items.into_iter().collect();
        items.sort_unstable_by_key(|(index, _)| *index);
        final_response["output"] = Value::Array(
            items
                .into_iter()
                .map(|(index, mut item)| {
                    if item["type"] == "function_call" && !item["arguments"].is_string() {
                        if let Some(arguments) = state.argument_deltas.remove(&index) {
                            item["arguments"] = Value::String(arguments);
                        }
                    }
                    item
                })
                .collect(),
        );
    }
    let output = extract_response(&final_response)?;
    handler.set_response_metadata(output.id, output.input_tokens, output.output_tokens);
    // Streamed output text has already been sent; only deliver it here when the
    // server supplied no text deltas (some compatible endpoints only send final items).
    if !state.sent_text && !output.text.is_empty() {
        handler.text(&output.text)?;
    }
    for call in output.tool_calls {
        handler.tool_call(call)?;
    }
    Ok(())
}
fn prepare_chat_completions(
    self_: &OpenAIResponsesClient,
    data: ChatCompletionsData,
) -> Result<RequestData> {
    let api_base = self_.get_api_base().unwrap_or_else(|| API_BASE.to_string());
    let url = format!("{}/responses", api_base.trim_end_matches('/'));
    let api_key = self_.get_api_key().ok();
    if reqwest::Url::parse(&api_base)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.eq_ignore_ascii_case("api.openai.com"))
        })
        .unwrap_or(false)
        && api_key.is_none()
    {
        bail!("OPENAI_API_KEY is required for the official OpenAI Responses endpoint");
    }
    let body = build_responses_body(data, &self_.model)?;
    let mut request = RequestData::new(url, body);
    if let Some(api_key) = api_key {
        request.bearer_auth(api_key);
    }
    if let Some(org) = self_.organization_id() {
        request.header("OpenAI-Organization", org);
    }
    Ok(request)
}

fn build_responses_body(data: ChatCompletionsData, model: &Model) -> Result<Value> {
    let ChatCompletionsData {
        messages,
        temperature,
        top_p,
        functions,
        stream,
    } = data;
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in messages {
        match (message.role, message.content) {
            (MessageRole::System, MessageContent::Text(text)) => instructions.push(text),
            (MessageRole::System, _) => bail!("Responses API only supports text system instructions"),
            (MessageRole::Tool, _) => bail!("Responses API tool results require a function call_id; found an uncorrelated tool message"),
            (role, MessageContent::Text(text)) => {
                let text = if role.is_assistant() {
                    crate::utils::strip_think_tag(&text)
                } else {
                    std::borrow::Cow::Borrowed(text.as_str())
                };
                input.push(json!({"role": role, "content": text}));
            }
            (role, MessageContent::Array(parts)) => {
                let mut content = Vec::with_capacity(parts.len());
                for part in parts {
                    match part {
                        MessageContentPart::Text { text } => {
                            let text = if role.is_assistant() {
                                crate::utils::strip_think_tag(&text)
                            } else {
                                std::borrow::Cow::Borrowed(text.as_str())
                            };
                            content.push(json!({"type":"input_text", "text":text}));
                        }
                        MessageContentPart::ImageUrl { .. } => {
                            bail!("Responses adapter currently supports text-only message content; image input is unsupported")
                        }
                    }
                }
                input.push(json!({"role": role, "content": content}));
            }
            (MessageRole::Assistant, MessageContent::ToolCalls(calls)) => {
                    let text = crate::utils::strip_think_tag(&calls.text);
                    if !text.is_empty() {
                        input.push(json!({"role":"assistant", "content":text}));
                    }
                let mut call_items = Vec::with_capacity(calls.tool_results.len());
                let mut output_items = Vec::with_capacity(calls.tool_results.len());
                for result in calls.tool_results {
                    let id = result.call.id.as_deref().filter(|id| !id.is_empty()).ok_or_else(|| {
                        anyhow!("Responses API function result for '{}' is missing its call_id", result.call.name)
                    })?;
                    call_items.push(json!({
                        "type":"function_call",
                        "call_id": id,
                        "name": result.call.name,
                        "arguments": result.call.arguments.to_string()
                    }));
                    output_items.push(json!({
                        "type":"function_call_output",
                        "call_id": id,
                        "output": tool_output_text(&result.output)
                    }));
                }
                if calls.sequence {
                    for (call, output) in call_items.into_iter().zip(output_items) {
                        input.push(call);
                        input.push(output);
                    }
                } else {
                    input.extend(call_items);
                    input.extend(output_items);
                }
            }
            (_, MessageContent::ToolCalls(_)) => {
                bail!("Responses API function calls are only valid in assistant messages")
            }
        }
    }

    let mut body = json!({
        "model": model.real_name(),
        "input": input,
        "store": false,
    });
    if !instructions.is_empty() {
        body["instructions"] = Value::String(instructions.join("\n\n"));
    }
    if let Some(v) = model.max_tokens_param() {
        body["max_output_tokens"] = json!(v);
    }
    if let Some(v) = temperature {
        body["temperature"] = json!(v);
    }
    if let Some(v) = top_p {
        body["top_p"] = json!(v);
    }
    if stream {
        body["stream"] = Value::Bool(true);
    }
    if let Some(functions) = functions {
        body["tools"] = Value::Array(
            functions
                .into_iter()
                .map(|function| {
                    json!({
                        "type":"function",
                        "name":function.name,
                        "description":function.description,
                        "parameters":function.parameters,
                        "strict":false
                    })
                })
                .collect(),
        );
    }
    Ok(body)
}

fn tool_output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[derive(Default)]
struct StreamState {
    completed: bool,
    sent_text: bool,
    final_response: Option<Value>,
    output_items: HashMap<usize, Value>,
    argument_deltas: HashMap<usize, String>,
}

impl StreamState {
    fn process(&mut self, event: SseMmessage, handler: &mut SseHandler) -> Result<bool> {
        let mut data: Value = serde_json::from_str(&event.data).with_context(|| {
            format!(
                "Invalid Responses SSE event '{}': {}",
                event.event, event.data
            )
        })?;
        let event_type = data["type"]
            .as_str()
            .or_else(|| (!event.event.is_empty()).then_some(event.event.as_str()))
            .unwrap_or_default();
        match event_type {
            "response.output_text.delta" => {
                let delta = required_str(&data, "delta", event_type)?;
                handler.text(delta)?;
                self.sent_text |= !delta.is_empty();
            }
            "response.function_call_arguments.delta" => {
                let index = required_index(&data, "output_index", event_type)?;
                let delta = required_str(&data, "delta", event_type)?;
                self.argument_deltas
                    .entry(index)
                    .or_default()
                    .push_str(delta);
            }
            "response.output_item.done" => {
                let index = required_index(&data, "output_index", event_type)?;
                data.get("item")
                    .filter(|item| item.is_object())
                    .ok_or_else(|| anyhow!("Responses SSE event '{event_type}' omitted item"))?;
                self.output_items.insert(index, data["item"].take());
            }
            "response.completed" => {
                let response = data
                    .get("response")
                    .filter(|response| response.is_object())
                    .ok_or_else(|| anyhow!("response.completed omitted response object"))?;
                ensure_response_success(response)?;
                self.final_response = Some(data["response"].take());
                self.completed = true;
                return Ok(true);
            }
            "response.failed" => {
                let response = data.get("response").unwrap_or(&data);
                let error = response.get("error").unwrap_or(response);
                bail!("Responses API response failed: {}", error_message(error))
            }
            "response.incomplete" => {
                let response = data.get("response").unwrap_or(&data);
                let reason = response["incomplete_details"]["reason"]
                    .as_str()
                    .unwrap_or("unspecified");
                bail!("Responses API response incomplete ({reason})")
            }
            "error" => bail!("Responses API stream error: {}", error_message(&data)),
            "response.output_item.added"
            | "response.created"
            | "response.in_progress"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done" => {}
            "response.function_call_arguments.done" => {
                let index = required_index(&data, "output_index", event_type)?;
                let arguments = required_str(&data, "arguments", event_type)?;
                self.argument_deltas.insert(index, arguments.to_owned());
            }
            _ => {
                if event_type.starts_with("response.") && event_type.ends_with(".failed") {
                    bail!(
                        "Responses API stream terminal failure ({event_type}): {}",
                        error_message(&data)
                    );
                }
            }
        }
        Ok(false)
    }
}

fn extract_response(data: &Value) -> Result<ChatCompletionsOutput> {
    ensure_response_success(data)?;
    let id = data["id"].as_str().map(str::to_owned);
    let mut text = String::new();
    let mut calls = Vec::new();
    for item in data["output"]
        .as_array()
        .ok_or_else(|| anyhow!("Responses API response omitted output items"))?
    {
        match item["type"].as_str().unwrap_or_default() {
            "message" => {
                if let Some(parts) = item["content"].as_array() {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("output_text") => {
                                text.push_str(required_str(part, "text", "output_text")?);
                            }
                            Some("refusal") => {
                                text.push_str(required_str(part, "refusal", "refusal")?);
                            }
                            Some("audio") | Some("image") | Some("summary_text") => {
                                bail!(
                                    "Responses API produced unsupported message content type '{}'",
                                    part["type"].as_str().unwrap_or("unknown")
                                );
                            }
                            Some(other) => bail!(
                                "Responses API produced unsupported message content type '{other}'"
                            ),
                            None => bail!("Responses API message content omitted type"),
                        }
                    }
                }
            }
            "function_call" => {
                let name = required_str(item, "name", "function_call")?;
                let id = item["call_id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| anyhow!("Responses function call '{name}' omitted call_id"))?;
                let arguments = item["arguments"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Responses function call '{name}' omitted arguments"))?;
                let arguments: Value = serde_json::from_str(arguments).with_context(|| {
                    format!("Responses function call '{name}' returned invalid JSON arguments")
                })?;
                calls.push(ToolCall::new(
                    name.to_owned(),
                    arguments,
                    Some(id.to_owned()),
                ));
            }
            "reasoning" => {} // Never expose reasoning in user-visible text.
            "" => bail!("Responses API output item omitted type"),
            other => bail!("Responses API produced unsupported output item '{other}'"),
        }
    }
    if text.is_empty() && calls.is_empty() {
        bail!(
            "Responses API completed without text or function calls (response id: {})",
            id.as_deref().unwrap_or("unknown")
        );
    }
    Ok(ChatCompletionsOutput {
        text,
        tool_calls: calls,
        id,
        input_tokens: data["usage"]["input_tokens"].as_u64(),
        output_tokens: data["usage"]["output_tokens"].as_u64(),
    })
}

fn ensure_response_success(response: &Value) -> Result<()> {
    if let Some(error) = response.get("error").filter(|value| !value.is_null()) {
        bail!("Responses API error: {}", error_message(error));
    }
    match response["status"].as_str() {
        Some("completed") => Ok(()),
        Some("incomplete") => bail!(
            "Responses API response incomplete ({})",
            response["incomplete_details"]["reason"]
                .as_str()
                .unwrap_or("unspecified")
        ),
        Some("failed") => bail!(
            "Responses API response failed: {}",
            error_message(&response["error"])
        ),
        Some(status) => bail!("Responses API returned nonterminal status '{status}'"),
        None => bail!("Responses API response omitted status"),
    }
}

fn error_message(error: &Value) -> String {
    error["message"]
        .as_str()
        .or_else(|| error["code"].as_str())
        .unwrap_or("unspecified error")
        .to_owned()
}

fn required_str<'a>(value: &'a Value, field: &str, event: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .ok_or_else(|| anyhow!("Responses {event} omitted string field '{field}'"))
}

fn required_index(value: &Value, field: &str, event: &str) -> Result<usize> {
    value[field]
        .as_u64()
        .map(|v| v as usize)
        .ok_or_else(|| anyhow!("Responses {event} omitted numeric field '{field}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::function::{FunctionDeclaration, JsonSchema, ToolResult};

    fn model() -> Model {
        Model::from_config("openai-responses", &[ModelData::new("fixture-model")]).remove(0)
    }

    fn completed(output: Value) -> Value {
        json!({"id":"resp_fixture","status":"completed","output":output,"usage":{"input_tokens":17,"output_tokens":9}})
    }

    #[test]
    fn builds_stateless_responses_history_instructions_and_functions() {
        let call = ToolCall::new("lookup".into(), json!({"q":"café"}), Some("call_17".into()));
        let messages = vec![
            Message::new(
                MessageRole::System,
                MessageContent::Text("Answer briefly".into()),
            ),
            Message::new(MessageRole::User, MessageContent::Text("Find it".into())),
            Message::new(
                MessageRole::Assistant,
                MessageContent::ToolCalls(MessageContentToolCalls::new(
                    vec![ToolResult::new(call, json!({"ok":true}))],
                    String::new(),
                )),
            ),
        ];
        let functions = vec![FunctionDeclaration {
            name: "lookup".into(),
            description: "Lookup data".into(),
            parameters: JsonSchema {
                type_value: Some("object".into()),
                description: None,
                properties: None,
                items: None,
                any_of: None,
                enum_value: None,
                default: None,
                required: None,
            },
            agent: false,
        }];
        let body = build_responses_body(
            ChatCompletionsData {
                messages,
                temperature: None,
                top_p: None,
                functions: Some(functions),
                stream: true,
            },
            &model(),
        )
        .unwrap();
        assert_eq!(body["store"], false);
        assert_eq!(body["instructions"], "Answer briefly");
        assert_eq!(body["input"][0]["content"], "Find it");
        assert_eq!(body["input"][1]["call_id"], "call_17");
        assert_eq!(body["input"][2]["call_id"], "call_17");
        assert_eq!(body["input"][2]["output"], "{\"ok\":true}");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn extracts_final_response_without_reasoning_and_preserves_usage_and_ids() {
        let response = completed(json!([
            {"type":"reasoning","summary":[{"text":"private thought"}]},
            {"type":"message","content":[{"type":"output_text","text":"Hello 🌍"}]},
            {"type":"function_call","name":"lookup","call_id":"call_X","arguments":"{\"x\":1}"}
        ]));
        let output = extract_response(&response).unwrap();
        assert_eq!(output.text, "Hello 🌍");
        assert_eq!(output.id.as_deref(), Some("resp_fixture"));
        assert_eq!(output.input_tokens, Some(17));
        assert_eq!(output.output_tokens, Some(9));
        assert_eq!(output.tool_calls[0].id.as_deref(), Some("call_X"));
        assert_eq!(output.tool_calls[0].arguments, json!({"x":1}));
    }

    #[test]
    fn rejects_incomplete_missing_correlation_and_multimodal_content() {
        assert!(extract_response(
            &json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}})
        )
        .unwrap_err()
        .to_string()
        .contains("incomplete"));
        assert!(extract_response(
            &json!({"status":"failed","error":{"code":"server_error","message":"fixture failure"}})
        )
        .unwrap_err()
        .to_string()
        .contains("fixture failure"));
        assert!(extract_response(&completed(
            json!([{"type":"function_call","name":"f","arguments":"{}"}])
        ))
        .unwrap_err()
        .to_string()
        .contains("call_id"));
        assert!(extract_response(&completed(
            json!([{"type":"message","content":[{"type":"audio","audio":"unsupported"}]}])
        ))
        .unwrap_err()
        .to_string()
        .contains("unsupported message content type"));
        let image_message = vec![Message::new(
            MessageRole::User,
            MessageContent::Array(vec![MessageContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.invalid/image.png".into(),
                },
            }]),
        )];
        assert!(build_responses_body(
            ChatCompletionsData {
                messages: image_message,
                temperature: None,
                top_p: None,
                functions: None,
                stream: false,
            },
            &model()
        )
        .unwrap_err()
        .to_string()
        .contains("image input is unsupported"));
        let messages = vec![Message::new(
            MessageRole::Tool,
            MessageContent::Text("orphan".into()),
        )];
        assert!(build_responses_body(
            ChatCompletionsData {
                messages,
                temperature: None,
                top_p: None,
                functions: None,
                stream: false
            },
            &model()
        )
        .unwrap_err()
        .to_string()
        .contains("uncorrelated"));
    }

    #[test]
    fn stream_rejects_missing_terminal_completion_and_uses_final_response() {
        let mut state = StreamState::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handler = SseHandler::new(tx, crate::utils::AbortSignalInner::new());
        let event = SseMmessage {
            event: String::new(),
            data: json!({"type":"response.output_text.delta","delta":"hi"}).to_string(),
        };
        state.process(event, &mut handler).unwrap();
        assert!(state.sent_text);
        assert!(!state.completed);
        let terminal = SseMmessage { event: String::new(), data: json!({"type":"response.completed","response":completed(json!([{"type":"message","content":[{"type":"output_text","text":"hi"}]}]))}).to_string() };
        state.process(terminal, &mut handler).unwrap();
        assert!(state.completed);
        assert_eq!(
            extract_response(state.final_response.as_ref().unwrap())
                .unwrap()
                .text,
            "hi"
        );
    }
    #[test]
    fn patches_apply_url_headers_and_safe_body_settings_but_reject_state_changes() {
        let mut client = fixture_client("http://127.0.0.1:1".into());
        client.config.patch = Some(
            serde_json::from_value(json!({
                "chat_completions": {
                    "fixture-model": {
                        "url": "http://127.0.0.1:1/custom",
                        "headers": {"x-mode":"fixture"},
                        "body": {"temperature":0.2}
                    }
                }
            }))
            .unwrap(),
        );
        let mut request = RequestData::new(
            "http://127.0.0.1:1/responses",
            json!({
                "store": false,
                "model": "fixture-model",
                "input": [{"role":"user","content":"real history"}],
                "instructions": "real instructions",
                "stream": true
            }),
        );
        request.bearer_auth("fixture-secret");
        client.patch_request_data(&mut request);
        assert_eq!(request.url, "http://127.0.0.1:1/custom");
        assert_eq!(
            request.headers.get("authorization").unwrap().as_str(),
            "Bearer fixture-secret"
        );
        assert_eq!(request.headers.get("x-mode").unwrap().as_str(), "fixture");
        assert_eq!(request.body["temperature"], 0.2);

        client.config.patch = Some(
            serde_json::from_value(json!({
                "chat_completions": {
                    "fixture-model": {
                        "body": {
                            "store": true,
                            "input": "replaced",
                            "instructions": "replaced",
                            "previous_response_id": "resp_old",
                            "stream": false,
                            "model": "other-model",
                            "tools": [{"type":"web_search"}]
                        }
                    }
                }
            }))
            .unwrap(),
        );
        let rejected = client.build_responses_request(
            &reqwest::Client::new(),
            RequestData::new(
                "http://127.0.0.1:1/responses",
                json!({
                    "store": false,
                    "model": "fixture-model",
                    "input": [{"role":"user","content":"real history"}],
                    "instructions": "real instructions",
                    "stream": true
                }),
            ),
        );
        let error = match rejected {
            Err(error) => error.to_string(),
            Ok(_) => panic!("stateful patch unexpectedly accepted"),
        };
        assert!(error.contains("store"));
    }
    async fn fixture_server(
        response: String,
        content_type: &'static str,
    ) -> (String, tokio::sync::oneshot::Receiver<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            let body_start;
            let content_length;
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    return;
                }
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    body_start = offset + 4;
                    let headers = String::from_utf8_lossy(&bytes[..offset]);
                    content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    break;
                }
            }
            while bytes.len() < body_start + content_length {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
            let _ = request_tx.send(String::from_utf8_lossy(&bytes).into_owned());
            if content_type == "text/event-stream" {
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                for bytes in response.as_bytes().chunks(7) {
                    socket
                        .write_all(format!("{:X}\r\n", bytes.len()).as_bytes())
                        .await
                        .unwrap();
                    socket.write_all(bytes).await.unwrap();
                    socket.write_all(b"\r\n").await.unwrap();
                }
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            } else {
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.len()
                );
                socket.write_all(header.as_bytes()).await.unwrap();
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}"), request_rx)
    }

    fn fixture_client(api_base: String) -> OpenAIResponsesClient {
        let name = format!("responses_fixture_{}", uuid::Uuid::new_v4());
        let model = Model::from_config(&name, &[ModelData::new("fixture-model")]).remove(0);
        OpenAIResponsesClient {
            global_config: std::sync::Arc::new(parking_lot::RwLock::new(
                crate::config::Config::default(),
            )),
            config: OpenAIResponsesConfig {
                name: Some(name),
                api_base: Some(api_base),
                api_key: Some("fixture-secret".into()),
                organization_id: Some("org-fixture".into()),
                models: vec![ModelData::new("fixture-model")],
                extra: None,
                patch: None,
                timeout_seconds: Some(5),
            },
            model,
        }
    }

    async fn stalled_fixture_server(
        wait_for_disconnect: bool,
    ) -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        Option<tokio::sync::oneshot::Receiver<bool>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0_u8; 1024];
            let body_start;
            let content_length;
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    return;
                }
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    body_start = offset + 4;
                    let headers = String::from_utf8_lossy(&bytes[..offset]);
                    content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    break;
                }
            }
            while bytes.len() < body_start + content_length {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    return;
                }
                bytes.extend_from_slice(&chunk[..read]);
            }
            let _ = request_tx.send(());
            if wait_for_disconnect {
                let disconnected = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        if socket.read(&mut chunk).await.unwrap_or(0) == 0 {
                            return true;
                        }
                    }
                })
                .await
                .unwrap_or(false);
                let _ = disconnect_tx.send(disconnected);
            } else {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let body = completed(json!([
                    {"type":"message","content":[{"type":"output_text","text":"late"}]}
                ]))
                .to_string();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
            }
        });
        (
            format!("http://{address}"),
            request_rx,
            wait_for_disconnect.then_some(disconnect_rx),
        )
    }
    fn fixture_data(stream: bool) -> ChatCompletionsData {
        ChatCompletionsData {
            messages: vec![
                Message::new(
                    MessageRole::System,
                    MessageContent::Text("Instruction".into()),
                ),
                Message::new(MessageRole::User, MessageContent::Text("Question".into())),
            ],
            temperature: None,
            top_p: None,
            functions: None,
            stream,
        }
    }

    #[tokio::test]
    async fn nonstreaming_http_fixture_preserves_request_and_response_metadata() {
        let payload = completed(json!([
            {"type":"message","content":[{"type":"output_text","text":"Ready"}]}
        ]))
        .to_string();
        let (api_base, request) = fixture_server(payload, "application/json").await;
        let client = fixture_client(api_base);
        let output = client
            .chat_completions_inner(&reqwest::Client::new(), fixture_data(false))
            .await
            .unwrap();
        let request = request.await.unwrap();
        assert!(request.starts_with("POST /responses "));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-secret"));
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        assert!(!body.to_string().contains("fixture-secret"));
        assert_eq!(body["store"], false);
        assert_eq!(body["instructions"], "Instruction");
        assert_eq!(body["input"][0]["content"], "Question");
        assert_eq!(output.text, "Ready");
        assert_eq!(output.input_tokens, Some(17));
        assert_eq!(output.output_tokens, Some(9));
        assert_eq!(output.id.as_deref(), Some("resp_fixture"));
    }
    #[tokio::test]
    async fn nonstreaming_http_fixture_surfaces_failed_and_incomplete_responses() {
        for response in [
            json!({"id":"resp_failed","status":"failed","error":{"code":"server_error","message":"fixture failed"}}),
            json!({"id":"resp_incomplete","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"}}),
        ] {
            let status = response["status"].as_str().unwrap().to_owned();
            let (api_base, request) =
                fixture_server(response.to_string(), "application/json").await;
            let client = fixture_client(api_base);
            let error = client
                .chat_completions_inner(&reqwest::Client::new(), fixture_data(false))
                .await
                .unwrap_err()
                .to_string();
            let _request = request.await.unwrap();
            assert!(error.contains(&status));
            assert!(!error.contains("fixture-secret"));
        }
    }
    #[tokio::test]
    async fn http_fixture_enforces_bounded_request_timeout() {
        let (api_base, request, _) = stalled_fixture_server(false).await;
        let mut client = fixture_client(api_base);
        client.config.timeout_seconds = Some(1);
        let result = client
            .chat_completions_inner(&reqwest::Client::new(), fixture_data(false))
            .await;
        request.await.unwrap();
        let error = result.unwrap_err();
        assert!(
            error
                .chain()
                .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
                .any(reqwest::Error::is_timeout),
            "{error:?}"
        );
        assert!(!error.to_string().contains("fixture-secret"));
    }

    #[tokio::test]
    async fn aborting_public_stream_method_closes_the_inflight_http_stream() {
        let (api_base, request, disconnected) = stalled_fixture_server(true).await;
        let client = fixture_client(api_base);
        let input = crate::config::Input::from_str(&client.global_config, "Question", None);
        let abort_signal = crate::utils::AbortSignalInner::new();
        let signal = abort_signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.set_ctrlc();
        });
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handler = SseHandler::new(tx, abort_signal);
        Client::chat_completions_streaming(&client, &input, &mut handler)
            .await
            .unwrap();
        request.await.unwrap();
        assert!(disconnected.unwrap().await.unwrap());
    }

    #[tokio::test]
    async fn streaming_http_fixture_handles_utf8_interleaved_function_events() {
        let final_response = completed(json!([
            {"type":"message","content":[{"type":"output_text","text":"Café 🌍"}]},
            {"type":"function_call","name":"lookup","call_id":"call_1","arguments":"{\"q\":\"x\"}"}
        ]));
        let events = [
            json!({"type":"response.output_text.delta","delta":"Caf"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"q\":"}),
            json!({"type":"response.output_text.delta","delta":"é 🌍"}),
            json!({"type":"response.function_call_arguments.delta","output_index":1,"delta":"\"x\"}"}),
            json!({"type":"response.output_item.done","output_index":1,"item":{"type":"function_call","name":"lookup","call_id":"call_1","arguments":"{\"q\":\"x\"}"}}),
            json!({"type":"response.completed","response":final_response}),
        ];
        let body = events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {}\n\n",
                    event["type"].as_str().unwrap(),
                    event
                )
            })
            .collect::<String>();
        let (api_base, request) = fixture_server(body, "text/event-stream").await;
        let client = fixture_client(api_base);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handler = SseHandler::new(tx, crate::utils::AbortSignalInner::new());
        client
            .chat_completions_streaming_inner(
                &reqwest::Client::new(),
                &mut handler,
                fixture_data(true),
            )
            .await
            .unwrap();
        let metadata = handler.take_response_metadata();
        assert_eq!(metadata.id.as_deref(), Some("resp_fixture"));
        assert_eq!(metadata.input_tokens, Some(17));
        assert_eq!(metadata.output_tokens, Some(9));
        let _request = request.await.unwrap();
        let (_, calls) = handler.take();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_deref(), Some("call_1"));
        assert_eq!(calls[0].arguments, json!({"q":"x"}));
        let mut text = String::new();
        while let Ok(event) = rx.try_recv() {
            if let SseEvent::Text(part) = event {
                text.push_str(&part);
            }
        }
        assert_eq!(text, "Café 🌍");
    }

    #[tokio::test]
    async fn streaming_http_fixture_requires_terminal_completion() {
        let event = format!(
            "event: response.output_text.delta\ndata: {}\n\n",
            json!({"type":"response.output_text.delta","delta":"partial"})
        );
        let (api_base, request) = fixture_server(event, "text/event-stream").await;
        let client = fixture_client(api_base);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut handler = SseHandler::new(tx, crate::utils::AbortSignalInner::new());
        let result = client
            .chat_completions_streaming_inner(
                &reqwest::Client::new(),
                &mut handler,
                fixture_data(true),
            )
            .await;
        let _request = request.await.unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("without response.completed"));
    }
}
