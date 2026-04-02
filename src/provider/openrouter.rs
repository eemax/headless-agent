use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    error::AppError,
    tools::ToolSpec,
    types::{Effort, MessageRole, PromptMessage, ToolCallRecord},
};

#[derive(Debug, Clone)]
pub struct OpenRouterClient {
    base_url: String,
    api_key: String,
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
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
        Self { base_url, api_key }
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
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(request.timeout)
            .timeout_read(request.timeout)
            .timeout_write(request.timeout)
            .build();

        let response = agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(payload);

        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
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
                        "OpenRouter request timed out after {:?}",
                        request.timeout
                    )));
                }
                return Err(AppError::Provider(format!(
                    "failed to reach OpenRouter: {error}"
                )));
            }
        };

        let parsed: ChatResponse = response.into_json().map_err(|err| {
            AppError::Provider(format!("failed to decode OpenRouter response: {err}"))
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
            .map(|tool_call| ToolCallRecord {
                id: tool_call.id,
                name: tool_call.function.name,
                arguments: serde_json::from_str(&tool_call.function.arguments)
                    .unwrap_or(Value::String(tool_call.function.arguments)),
            })
            .collect();

        Ok(ProviderResponse {
            content,
            tool_calls,
        })
    }
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

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
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
