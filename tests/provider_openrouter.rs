mod common;

use std::{env, time::Duration};

use serde_json::json;

use common::{FakeOpenRouter, ResponseSpec};
use headless::{
    error::AppError,
    provider::openrouter::{ChatRequest, OpenRouterClient},
    tools::bash::bash_spec,
    types::{Effort, MessageRole, PromptMessage},
};

#[test]
fn openrouter_request_includes_reasoning_and_tool_definitions() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "ok"
                }
            }
        ]
    }))]);
    let client = OpenRouterClient::new(server.url(), "test-key".to_string());
    let response = client
        .send_chat(ChatRequest {
            session_id: "session-1",
            model: "openai/gpt-4.1",
            effort: Effort::High,
            messages: &[
                PromptMessage {
                    role: MessageRole::System,
                    content: Some("system".to_string()),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                },
                PromptMessage {
                    role: MessageRole::User,
                    content: Some("hello".to_string()),
                    name: None,
                    tool_call_id: None,
                    tool_calls: Vec::new(),
                },
            ],
            tools: &[bash_spec()],
            max_output_tokens: 1234,
            timeout: Duration::from_secs(5),
        })
        .expect("provider response");

    assert_eq!(response.content.as_deref(), Some("ok"));
    let requests = server.requests();
    let request = &requests[0];
    assert_eq!(request["model"], "openai/gpt-4.1");
    assert_eq!(request["session_id"], "session-1");
    assert_eq!(request["max_completion_tokens"], 1234);
    assert_eq!(request["parallel_tool_calls"], false);
    assert_eq!(request["reasoning"]["effort"], "high");
    assert_eq!(request["tools"][0]["function"]["name"], "bash");
}

#[test]
fn openrouter_maps_timeout_status_to_timeout_errors() {
    let server = FakeOpenRouter::start(vec![ResponseSpec {
        status: 408,
        body: json!({
            "error": {
                "message": "slow provider"
            }
        }),
        delay_ms: 0,
    }]);
    let client = OpenRouterClient::new(server.url(), "test-key".to_string());
    let error = client
        .send_chat(ChatRequest {
            session_id: "session-1",
            model: "openai/gpt-4.1",
            effort: Effort::Medium,
            messages: &[PromptMessage {
                role: MessageRole::User,
                content: Some("hello".to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            }],
            tools: &[],
            max_output_tokens: 128,
            timeout: Duration::from_secs(5),
        })
        .expect_err("expected timeout error");

    match error {
        AppError::Timeout(message) => assert!(message.contains("slow provider")),
        other => panic!("expected timeout error, got {other:?}"),
    }
}

#[test]
fn non_timeout_http_error_maps_to_provider_error() {
    let server = FakeOpenRouter::start(vec![ResponseSpec {
        status: 500,
        body: json!({
            "error": {
                "message": "internal failure"
            }
        }),
        delay_ms: 0,
    }]);
    let client = OpenRouterClient::new(server.url(), "test-key".to_string());
    let error = client
        .send_chat(ChatRequest {
            session_id: "session-1",
            model: "openai/gpt-4.1",
            effort: Effort::Medium,
            messages: &[PromptMessage {
                role: MessageRole::User,
                content: Some("hello".to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            }],
            tools: &[],
            max_output_tokens: 128,
            timeout: Duration::from_secs(5),
        })
        .expect_err("expected provider error");

    match error {
        AppError::Provider(message) => assert!(message.contains("internal failure")),
        other => panic!("expected provider error, got {other:?}"),
    }
}

#[test]
fn malformed_tool_arguments_fall_back_to_raw_string() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [{
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "function": {
                        "name": "bash",
                        "arguments": "not json"
                    }
                }]
            }
        }]
    }))]);
    let client = OpenRouterClient::new(server.url(), "test-key".to_string());
    let response = client
        .send_chat(ChatRequest {
            session_id: "session-1",
            model: "openai/gpt-4.1",
            effort: Effort::Medium,
            messages: &[PromptMessage {
                role: MessageRole::User,
                content: Some("hello".to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            }],
            tools: &[bash_spec()],
            max_output_tokens: 128,
            timeout: Duration::from_secs(5),
        })
        .expect("provider response");

    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].name, "bash");
    assert_eq!(response.tool_calls[0].arguments, json!("not json"));
}

#[test]
#[ignore]
fn openrouter_live_smoke_test() {
    let api_key = match env::var("OPENROUTER_API_KEY") {
        Ok(value) if !value.is_empty() => value,
        _ => return,
    };
    let client = OpenRouterClient::new("https://openrouter.ai/api/v1".to_string(), api_key);
    let response = client
        .send_chat(ChatRequest {
            session_id: "live-smoke",
            model: "openai/gpt-4.1-mini",
            effort: Effort::Minimal,
            messages: &[PromptMessage {
                role: MessageRole::User,
                content: Some("Reply with the single word ok.".to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
            }],
            tools: &[],
            max_output_tokens: 32,
            timeout: Duration::from_secs(30),
        })
        .expect("live response");
    assert!(
        response
            .content
            .unwrap_or_default()
            .to_lowercase()
            .contains("ok")
    );
}
