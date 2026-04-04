use headless::{
    agent_def::LoadedAgent,
    prompt::assemble_prompt,
    types::{MessageRole, ToolCallRecord, TranscriptRecord},
};
use serde_json::json;
use tempfile::TempDir;

fn make_agent(temp: &TempDir) -> LoadedAgent {
    let agents_dir = temp.path().join("agents");
    let prompts_dir = temp.path().join("prompts");
    std::fs::create_dir_all(&agents_dir).expect("agents dir");
    std::fs::create_dir_all(&prompts_dir).expect("prompts dir");
    std::fs::write(prompts_dir.join("test.md"), "system prompt").expect("prompt");
    std::fs::write(
        agents_dir.join("test.toml"),
        r#"
name = "test"
default_model = "test/model"
default_effort = "medium"
enabled_tools = ["bash"]
system_prompt_file = "../prompts/test.md"
"#,
    )
    .expect("agent");
    LoadedAgent::from_path(agents_dir.join("test.toml")).expect("load agent")
}

#[test]
fn history_filtering_keeps_only_user_and_final_assistant_from_prior_runs() {
    let temp = TempDir::new().expect("tempdir");
    let agent = make_agent(&temp);

    // Simulate prior run history: user prompt, intermediate assistant with tool_calls,
    // tool result, and final assistant text.
    let history = vec![
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:00:00Z".to_string(),
            run_id: "run_01".to_string(),
            role: MessageRole::User,
            content: Some("fix the bug".to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        },
        // Intermediate assistant: has tool_calls, should be filtered OUT
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:00:01Z".to_string(),
            run_id: "run_01".to_string(),
            role: MessageRole::Assistant,
            content: None,
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: Some(vec![ToolCallRecord {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                arguments: json!({"command": "ls"}),
            }]),
        },
        // Tool result: should be filtered OUT
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:00:02Z".to_string(),
            run_id: "run_01".to_string(),
            role: MessageRole::Tool,
            content: Some("file1.txt\nfile2.txt".to_string()),
            name: Some("bash".to_string()),
            tool_call_id: Some("call_1".to_string()),
            preview: None,
            artifact: None,
            tool_calls: None,
        },
        // Final assistant: no tool_calls, should be KEPT
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:00:03Z".to_string(),
            run_id: "run_01".to_string(),
            role: MessageRole::Assistant,
            content: Some("I fixed the bug by editing file1.txt".to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        },
    ];

    let result = assemble_prompt(&agent, None, false, &history, "what did you change?", None)
        .expect("assemble");

    // Expected: system prompt + user("fix the bug") + assistant("I fixed...") + user("what did you change?")
    assert_eq!(result.messages.len(), 4);
    assert_eq!(result.messages[0].role, MessageRole::System);
    assert_eq!(result.messages[0].content.as_deref(), Some("system prompt"));

    assert_eq!(result.messages[1].role, MessageRole::User);
    assert_eq!(result.messages[1].content.as_deref(), Some("fix the bug"));

    assert_eq!(result.messages[2].role, MessageRole::Assistant);
    assert_eq!(
        result.messages[2].content.as_deref(),
        Some("I fixed the bug by editing file1.txt")
    );
    assert!(
        result.messages[2].tool_calls.is_empty(),
        "tool_calls should be stripped from prior assistant messages"
    );

    assert_eq!(result.messages[3].role, MessageRole::User);
    assert_eq!(
        result.messages[3].content.as_deref(),
        Some("what did you change?")
    );
}

#[test]
fn history_filtering_handles_multiple_prior_runs() {
    let temp = TempDir::new().expect("tempdir");
    let agent = make_agent(&temp);

    let history = vec![
        // Run 1: simple exchange (no tools)
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:00:00Z".to_string(),
            run_id: "run_01".to_string(),
            role: MessageRole::User,
            content: Some("hello".to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        },
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:00:01Z".to_string(),
            run_id: "run_01".to_string(),
            role: MessageRole::Assistant,
            content: Some("hi there".to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        },
        // Run 2: tool-heavy (3 intermediate steps, only final kept)
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:01:00Z".to_string(),
            run_id: "run_02".to_string(),
            role: MessageRole::User,
            content: Some("refactor".to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        },
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:01:01Z".to_string(),
            run_id: "run_02".to_string(),
            role: MessageRole::Assistant,
            content: None,
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: Some(vec![ToolCallRecord {
                id: "c1".to_string(),
                name: "bash".to_string(),
                arguments: json!({}),
            }]),
        },
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:01:02Z".to_string(),
            run_id: "run_02".to_string(),
            role: MessageRole::Tool,
            content: Some("output".to_string()),
            name: Some("bash".to_string()),
            tool_call_id: Some("c1".to_string()),
            preview: None,
            artifact: None,
            tool_calls: None,
        },
        TranscriptRecord {
            v: 1,
            ts: "2026-01-01T00:01:03Z".to_string(),
            run_id: "run_02".to_string(),
            role: MessageRole::Assistant,
            content: Some("done refactoring".to_string()),
            name: None,
            tool_call_id: None,
            preview: None,
            artifact: None,
            tool_calls: None,
        },
    ];

    let result = assemble_prompt(&agent, None, false, &history, "next task", None)
        .expect("assemble");

    // system + user("hello") + assistant("hi there") + user("refactor") + assistant("done refactoring") + user("next task")
    assert_eq!(result.messages.len(), 6);
    assert_eq!(result.messages[1].content.as_deref(), Some("hello"));
    assert_eq!(result.messages[2].content.as_deref(), Some("hi there"));
    assert_eq!(result.messages[3].content.as_deref(), Some("refactor"));
    assert_eq!(
        result.messages[4].content.as_deref(),
        Some("done refactoring")
    );
    assert_eq!(result.messages[5].content.as_deref(), Some("next task"));
}
