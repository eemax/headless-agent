use std::{io, time::Duration};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::ToolSpec,
    types::{Effort, MessageRole, PromptMessage, ToolCallRecord},
};

const CONNECT_TIMEOUT_CAP: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub struct OpenRouterClient {
    base_url: String,
    api_key: String,
    agent: ureq::Agent,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub prompt_tokens: usize,
    #[serde(default)]
    pub completion_tokens: usize,
    #[serde(default)]
    pub total_tokens: usize,
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub usage: Option<TokenUsage>,
}

pub struct ChatRequest<'a> {
    pub session_id: &'a str,
    pub model: &'a str,
    pub effort: Effort,
    pub messages: &'a [PromptMessage],
    pub tools: &'a [ToolSpec],
    pub max_output_tokens: usize,
    pub timeout: Duration,
}

impl OpenRouterClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT_CAP)
            .build();
        Self {
            base_url,
            api_key,
            agent,
        }
    }

    pub fn send_chat(&self, request: ChatRequest<'_>) -> Result<ProviderResponse, AppError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let payload = build_payload(
            request.session_id,
            request.model,
            request.effort,
            request.messages,
            request.tools,
            request.max_output_tokens,
        );
        send_chat_blocking(&self.agent, url, &self.api_key, payload, request.timeout)
    }
}

fn send_chat_blocking(
    agent: &ureq::Agent,
    url: String,
    api_key: &str,
    payload: Value,
    timeout: Duration,
) -> Result<ProviderResponse, AppError> {
    let response = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", api_key))
        .set("Content-Type", "application/json")
        .timeout(timeout)
        .send_json(payload);

    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().map_err(|error| {
                if code == 408 {
                    AppError::Timeout(format!(
                        "OpenRouter returned HTTP {code}, but the error body could not be read: {error}"
                    ))
                } else {
                    AppError::Provider(format!(
                        "OpenRouter returned HTTP {code}, but the error body could not be read: {error}"
                    ))
                }
            })?;
            let message = extract_error_message(&body)
                .unwrap_or_else(|| format!("OpenRouter returned HTTP {code}"));
            return if code == 408 {
                Err(AppError::Timeout(message))
            } else {
                Err(AppError::Provider(message))
            };
        }
        Err(ureq::Error::Transport(error)) => {
            if error.to_string().to_lowercase().contains("timed out") {
                return Err(AppError::Timeout(format!(
                    "OpenRouter request timed out: {error}"
                )));
            }
            return Err(AppError::Provider(format!(
                "failed to reach OpenRouter: {error}"
            )));
        }
    };

    let status = response.status();
    let body = response
        .into_string()
        .map_err(|error| map_success_body_read_error(status, error))?;
    let parsed: ChatResponse = serde_json::from_str(&body).map_err(|error| {
        AppError::Provider(format!(
            "OpenRouter returned HTTP {status}, but the response body was not valid JSON: {error}. Body preview: {}",
            preview_body(&body)
        ))
    })?;
    let choice = parsed.choices.into_iter().next().ok_or_else(|| {
        AppError::Provider("OpenRouter response did not contain any choices".to_string())
    })?;

    let content = flatten_content(choice.message.content);
    let tool_calls = choice
        .message
        .tool_calls
        .unwrap_or_default()
        .into_iter()
        .map(|tool_call| {
            let fn_name = tool_call.function.name;
            let raw_args = tool_call.function.arguments;
            let arguments = serde_json::from_str(&raw_args).unwrap_or_else(|err| {
                eprintln!("warning: malformed tool arguments for `{fn_name}`: {err}");
                Value::String(raw_args)
            });
            ToolCallRecord {
                id: tool_call.id,
                name: fn_name,
                arguments,
            }
        })
        .collect();

    Ok(ProviderResponse {
        content,
        tool_calls,
        usage: parsed.usage,
    })
}

fn build_payload(
    session_id: &str,
    model: &str,
    effort: Effort,
    messages: &[PromptMessage],
    tools: &[ToolSpec],
    max_output_tokens: usize,
) -> Value {
    let messages = messages
        .iter()
        .map(|message| match message.role {
            MessageRole::System | MessageRole::User => json!({
                "role": role_string(message.role),
                "content": message.content.clone().unwrap_or_default(),
            }),
            MessageRole::Assistant => {
                let mut value = json!({
                    "role": "assistant",
                    "content": message.content.clone().map(Value::String).unwrap_or(Value::Null),
                });
                if !message.tool_calls.is_empty() {
                    value["tool_calls"] = Value::Array(
                        message
                            .tool_calls
                            .iter()
                            .map(|tool_call| {
                                json!({
                                    "id": tool_call.id,
                                    "type": "function",
                                    "function": {
                                        "name": tool_call.name,
                                        "arguments": serde_json::to_string(&tool_call.arguments).unwrap_or_else(|_| "{}".to_string()),
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                value
            }
            MessageRole::Tool => json!({
                "role": "tool",
                "tool_call_id": message.tool_call_id,
                "name": message.name,
                "content": message.content.clone().unwrap_or_default(),
            }),
        })
        .collect::<Vec<_>>();

    let mut payload = json!({
        "model": model,
        "messages": messages,
        "session_id": session_id,
        "stream": false,
        "parallel_tool_calls": false,
        "max_completion_tokens": max_output_tokens,
    });

    if effort != Effort::None {
        payload["reasoning"] = json!({ "effort": effort.to_string() });
    }

    if !tools.is_empty() {
        payload["tools"] = Value::Array(tools.iter().map(ToolSpec::as_json).collect());
        payload["tool_choice"] = Value::String("auto".to_string());
    }

    payload
}

fn role_string(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn flatten_content(content: Option<Value>) -> Option<String> {
    match content? {
        Value::Null => None,
        Value::String(value) => Some(value),
        Value::Array(items) => {
            let mut output = String::new();
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    output.push_str(text);
                }
            }
            if output.is_empty() {
                None
            } else {
                Some(output)
            }
        }
        other => Some(other.to_string()),
    }
}

fn extract_error_message(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct ErrorResponse {
        error: Option<InnerError>,
    }

    #[derive(Deserialize)]
    struct InnerError {
        message: Option<String>,
    }

    serde_json::from_str::<ErrorResponse>(body)
        .ok()
        .and_then(|response| response.error)
        .and_then(|error| error.message)
}

fn map_success_body_read_error(status: u16, error: io::Error) -> AppError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => AppError::Timeout(format!(
            "OpenRouter returned HTTP {status}, but reading the response body timed out: {error}"
        )),
        _ => AppError::Provider(format!(
            "OpenRouter returned HTTP {status}, but the response body could not be fully read: {error}"
        )),
    }
}

fn preview_body(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return "(empty response body)".to_string();
    }

    let mut preview = compact.chars();
    let snippet: String = preview.by_ref().take(200).collect();
    if preview.next().is_some() {
        format!("{snippet}...")
    } else {
        snippet
    }
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<TokenUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<Value>,
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseFunction {
    name: String,
    arguments: String,
}
