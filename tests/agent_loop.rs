mod common;

use std::{fs, thread, time::Duration};

use serde_json::{Value, json};

use common::{FakeOpenRouter, ResponseSpec, TestWorkspace};

#[test]
fn end_to_end_loop_can_write_read_and_shell_out() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"path\":\"note.txt\",\"content\":\"hello from tool\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_2",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"note.txt\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_3",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"cat note.txt\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "workflow complete"
                }
            }]
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let output = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--cwd",
            workspace.worktree.to_str().expect("cwd"),
            "make a note",
        ])
        .output()
        .expect("run output");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout"),
        "workflow complete"
    );
    assert_eq!(
        fs::read_to_string(workspace.worktree.join("note.txt")).expect("note"),
        "hello from tool"
    );

    let session_id =
        common::extract_created_session_id(&String::from_utf8(output.stderr).expect("stderr"));
    let messages = fs::read_to_string(
        workspace
            .sessions_dir
            .join(&session_id)
            .join("messages.jsonl"),
    )
    .expect("messages");
    assert!(!messages.contains("\"name\":\"write_file\""));
    assert!(!messages.contains("\"name\":\"read_file\""));
    assert!(!messages.contains("\"name\":\"bash\""));

    let session_records: Vec<Value> = messages
        .lines()
        .map(|line| serde_json::from_str(line).expect("message json"))
        .collect();
    assert_eq!(session_records.len(), 2);
    assert_eq!(session_records[0]["role"], "user");
    assert_eq!(session_records[1]["role"], "assistant");
    assert_eq!(session_records[1]["content"], "workflow complete");

    let run_dir = workspace.only_run_dir(&session_id);
    let transcript = fs::read_to_string(run_dir.join("transcript.jsonl")).expect("transcript");
    assert!(transcript.contains("\"name\":\"write_file\""));
    assert!(transcript.contains("\"name\":\"read_file\""));
    assert!(transcript.contains("\"name\":\"bash\""));
    let outcome: Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("outcome.json")).expect("outcome"))
            .expect("outcome json");
    assert_eq!(outcome["termination"], "complete");
}

#[test]
fn stopped_session_allows_in_flight_mutating_run_to_finish() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"path\":\"note.txt\",\"content\":\"persisted\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": "done after stop"
                    }
                }]
            }),
            500,
        ),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout)
        .expect("stdout")
        .trim()
        .to_string();

    let mut run = workspace.std_command();
    run.args([
        "--session",
        &session_id,
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "finish after stop",
    ]);
    let handle = thread::spawn(move || run.output().expect("run output"));

    for _ in 0..20 {
        if workspace.worktree.join("note.txt").exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(
        workspace.worktree.join("note.txt").exists(),
        "mutating tool should finish before session stop"
    );

    workspace
        .command()
        .args(["session", "stop", &session_id])
        .assert()
        .success();

    let output = handle.join().expect("join run");
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(workspace.worktree.join("note.txt")).expect("note"),
        "persisted"
    );

    let messages = fs::read_to_string(
        workspace
            .sessions_dir
            .join(&session_id)
            .join("messages.jsonl"),
    )
    .expect("messages");
    let session_records: Vec<Value> = messages
        .lines()
        .map(|line| serde_json::from_str(line).expect("message json"))
        .collect();
    assert_eq!(session_records.len(), 2);
    assert_eq!(session_records[1]["content"], "done after stop");

    let blocked = workspace
        .command()
        .args(["--session", &session_id, "--agent", "coder", "should fail"])
        .output()
        .expect("blocked run");
    assert_eq!(blocked.status.code(), Some(4));
    assert!(
        String::from_utf8(blocked.stderr)
            .expect("stderr")
            .contains("stopped")
    );
}

#[test]
fn stopped_session_allows_pre_stop_run_to_reach_first_mutating_tool() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"path\":\"note.txt\",\"content\":\"late persist\"}"
                            }
                        }]
                    }
                }]
            }),
            500,
        ),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "done after delayed mutate"
                }
            }]
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout)
        .expect("stdout")
        .trim()
        .to_string();

    let mut run = workspace.std_command();
    run.args([
        "--session",
        &session_id,
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "finish after delayed stop",
    ]);
    let handle = thread::spawn(move || run.output().expect("run output"));

    server.wait_for_requests(1, Duration::from_secs(2));

    workspace
        .command()
        .args(["session", "stop", &session_id])
        .assert()
        .success();

    let output = handle.join().expect("join run");
    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(workspace.worktree.join("note.txt")).expect("note"),
        "late persist"
    );

    let messages = fs::read_to_string(
        workspace
            .sessions_dir
            .join(&session_id)
            .join("messages.jsonl"),
    )
    .expect("messages");
    let session_records: Vec<Value> = messages
        .lines()
        .map(|line| serde_json::from_str(line).expect("message json"))
        .collect();
    assert_eq!(session_records.len(), 2);
    assert_eq!(session_records[1]["content"], "done after delayed mutate");

    let blocked = workspace
        .command()
        .args(["--session", &session_id, "--agent", "coder", "should fail"])
        .output()
        .expect("blocked run");
    assert_eq!(blocked.status.code(), Some(4));
    assert!(
        String::from_utf8(blocked.stderr)
            .expect("stderr")
            .contains("stopped")
    );
}

#[test]
fn same_session_conflict_allows_only_one_mutating_run_to_change_the_worktree() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"path\":\"one.txt\",\"content\":\"winner\"}"
                            }
                        }]
                    }
                }]
            }),
            150,
        ),
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call_2",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"path\":\"two.txt\",\"content\":\"loser\"}"
                            }
                        }]
                    }
                }]
            }),
            150,
        ),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "done"
                }
            }]
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout)
        .expect("stdout")
        .trim()
        .to_string();

    let mut first = workspace.std_command();
    first.args([
        "--session",
        &session_id,
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "race one",
    ]);
    let mut second = workspace.std_command();
    second.args([
        "--session",
        &session_id,
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "race two",
    ]);

    let first_handle = thread::spawn(move || first.output().expect("first output"));
    let second_handle = thread::spawn(move || second.output().expect("second output"));
    let first_output = first_handle.join().expect("join first");
    let second_output = second_handle.join().expect("join second");
    let codes = [first_output.status.code(), second_output.status.code()];
    assert!(codes.contains(&Some(0)));
    assert!(codes.contains(&Some(5)));

    let one_exists = workspace.worktree.join("one.txt").exists();
    let two_exists = workspace.worktree.join("two.txt").exists();
    assert_ne!(
        one_exists, two_exists,
        "only one mutating run should touch the worktree"
    );
}

#[test]
fn second_mutating_run_fails_fast_while_execution_lock_is_held() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"sleep 0.5\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_2",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"path\":\"blocked.txt\",\"content\":\"should not exist\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "first done"
                }
            }]
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout)
        .expect("stdout")
        .trim()
        .to_string();

    let mut first = workspace.std_command();
    first.args([
        "--session",
        &session_id,
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "hold the lock",
    ]);
    let first_handle = thread::spawn(move || first.output().expect("first output"));

    server.wait_for_requests(1, Duration::from_secs(1));
    thread::sleep(Duration::from_millis(50));

    let mut second = workspace.std_command();
    second.args([
        "--session",
        &session_id,
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "blocked run",
    ]);
    let started = std::time::Instant::now();
    let second_output = second.output().expect("second output");
    let elapsed = started.elapsed();
    let first_output = first_handle.join().expect("join first");

    assert!(first_output.status.success());
    assert_eq!(second_output.status.code(), Some(5));
    assert!(elapsed < Duration::from_millis(400));
    assert!(!workspace.worktree.join("blocked.txt").exists());
}

#[test]
fn timeout_after_tool_execution_writes_run_transcript_but_not_session_tool_history() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"path\":\"evidence.txt\",\"content\":\"tool ran\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": "should not arrive"
                    }
                }]
            }),
            2_000,
        ),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_timeout(&server.url(), "1s");

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout)
        .expect("stdout")
        .trim()
        .to_string();

    let output = workspace
        .command()
        .args([
            "--session",
            &session_id,
            "--agent",
            "coder",
            "--cwd",
            workspace.worktree.to_str().expect("cwd"),
            "partial test",
        ])
        .output()
        .expect("run output");

    assert_eq!(output.status.code(), Some(7));
    assert!(workspace.worktree.join("evidence.txt").exists());

    let messages = fs::read_to_string(
        workspace
            .sessions_dir
            .join(&session_id)
            .join("messages.jsonl"),
    )
    .expect("messages");
    assert!(!messages.contains("\"name\":\"write_file\""));
    assert_eq!(messages.lines().count(), 1);

    let run_dir = workspace.only_run_dir(&session_id);
    let transcript = fs::read_to_string(run_dir.join("transcript.jsonl")).expect("transcript");
    assert!(
        transcript.contains("\"name\":\"write_file\""),
        "tool execution should be recorded in the run transcript despite timeout"
    );
    let outcome: Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("outcome.json")).expect("outcome"))
            .expect("outcome json");
    assert_eq!(outcome["termination"], "timeout");
}

#[test]
fn runaway_loop_persists_partial_records_on_termination() {
    // Use a tool call followed by a delayed API response to trigger timeout.
    // The first tool call completes, then the second API call times out.
    // This validates that partial records are persisted.
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_0",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"dummy.txt\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"sleep 5\"}"
                        }
                    }]
                }
            }]
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_timeout(&server.url(), "1s");
    fs::write(workspace.worktree.join("dummy.txt"), "hello").expect("dummy file");

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout)
        .expect("stdout")
        .trim()
        .to_string();

    let output = workspace
        .command()
        .args([
            "--session",
            &session_id,
            "--agent",
            "coder",
            "--cwd",
            workspace.worktree.to_str().expect("cwd"),
            "loop until timeout",
        ])
        .output()
        .expect("run output");

    assert!(!output.status.success());

    let messages_path = workspace
        .sessions_dir
        .join(&session_id)
        .join("messages.jsonl");
    let messages = fs::read_to_string(&messages_path).expect("messages");
    assert_eq!(messages.lines().count(), 1);

    let run_dir = workspace.only_run_dir(&session_id);
    let transcript = fs::read_to_string(run_dir.join("transcript.jsonl")).expect("transcript");
    let line_count = transcript.lines().count();
    assert!(
        line_count >= 3,
        "expected at least 3 run transcript records, got {line_count}"
    );

    // Session should be bound
    let show = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show.stdout).expect("meta json");
    assert_eq!(meta["revision"], 1);
    assert_eq!(meta["agent_name"], "coder");
}

#[test]
fn token_usage_is_parsed_from_provider_response() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [{
            "message": {
                "content": "with usage"
            }
        }],
        "usage": {
            "prompt_tokens": 42,
            "completion_tokens": 10,
            "total_tokens": 52
        }
    }))]);
    let client =
        headless::provider::openrouter::OpenRouterClient::new(server.url(), "test-key".to_string());
    let response = client
        .send_chat(headless::provider::openrouter::ChatRequest {
            session_id: "s1",
            model: "test",
            effort: headless::types::Effort::Medium,
            messages: &[headless::types::PromptMessage {
                role: headless::types::MessageRole::User,
                content: Some("hi".to_string()),
                name: None,
                tool_call_id: None,
                tool_calls: Vec::new(),
                reasoning: None,
                reasoning_details: None,
            }],
            tools: &[],
            max_output_tokens: 128,
            timeout: Duration::from_secs(5),
        })
        .expect("response");

    let usage = response.usage.expect("usage should be present");
    assert_eq!(usage.prompt_tokens, 42);
    assert_eq!(usage.completion_tokens, 10);
    assert_eq!(usage.total_tokens, 52);
}

#[test]
fn reasoning_is_resent_within_a_run_and_provider_metadata_is_persisted() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning": {
                        "signature": "opaque-reasoning"
                    },
                    "reasoning_details": [
                        {
                            "type": "reasoning.summary",
                            "text": "inspect note"
                        }
                    ],
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"note.txt\"}"
                        }
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 40,
                "completion_tokens": 12,
                "total_tokens": 52,
                "prompt_tokens_details": {
                    "cached_tokens": 11,
                    "cache_write_tokens": 3
                },
                "completion_tokens_details": {
                    "reasoning_tokens": 5
                }
            }
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "done"
                }
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 4,
                "total_tokens": 24
            }
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());
    fs::write(workspace.worktree.join("note.txt"), "hello").expect("seed note");

    let output = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--cwd",
            workspace.worktree.to_str().expect("cwd"),
            "inspect the note",
        ])
        .output()
        .expect("run output");

    assert!(output.status.success());
    let session_id =
        common::extract_created_session_id(&String::from_utf8(output.stderr).expect("stderr"));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1]["messages"][2]["reasoning"]["signature"],
        "opaque-reasoning"
    );
    assert_eq!(
        requests[1]["messages"][2]["reasoning_details"][0]["type"],
        "reasoning.summary"
    );

    let run_dir = workspace.only_run_dir(&session_id);
    let provider_trace =
        fs::read_to_string(run_dir.join("provider.jsonl")).expect("provider trace should exist");
    let provider_records: Vec<Value> = provider_trace
        .lines()
        .map(|line| serde_json::from_str(line).expect("provider json"))
        .collect();
    assert_eq!(provider_records.len(), 2);
    assert_eq!(provider_records[0]["usage"]["cached_tokens"], 11);
    assert_eq!(provider_records[0]["usage"]["cache_write_tokens"], 3);
    assert_eq!(provider_records[0]["usage"]["reasoning_tokens"], 5);
    assert_eq!(
        provider_records[0]["reasoning"]["signature"],
        "opaque-reasoning"
    );
    assert_eq!(
        provider_records[0]["reasoning_details"][0]["type"],
        "reasoning.summary"
    );

    let outcome: Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("outcome.json")).expect("outcome"))
            .expect("outcome json");
    assert_eq!(outcome["provider_usage_summary"]["steps"], 2);
    assert_eq!(outcome["provider_usage_summary"]["steps_with_usage"], 2);
    assert_eq!(outcome["provider_usage_summary"]["prompt_tokens"], 60);
    assert_eq!(outcome["provider_usage_summary"]["completion_tokens"], 16);
    assert_eq!(outcome["provider_usage_summary"]["total_tokens"], 76);
    assert_eq!(outcome["provider_usage_summary"]["cached_tokens"], 11);
    assert_eq!(outcome["provider_usage_summary"]["cache_write_tokens"], 3);
    assert_eq!(outcome["provider_usage_summary"]["reasoning_tokens"], 5);

    let messages = fs::read_to_string(
        workspace
            .sessions_dir
            .join(&session_id)
            .join("messages.jsonl"),
    )
    .expect("messages");
    assert!(!messages.contains("opaque-reasoning"));
    assert!(!messages.contains("reasoning_details"));
}
