mod common;

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use serde_json::Value;
use serde_json::json;

use common::{FakeOpenRouter, ResponseSpec, TestWorkspace, extract_created_session_id};

#[test]
fn session_new_convenience_keeps_stdout_clean_and_binds_the_session() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "hello back"
                }
            }
        ]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let output = workspace
        .command()
        .args(["--session", "new", "--agent", "coder", "hello"])
        .output()
        .expect("run output");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout"),
        "hello back"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("created session "));
    let session_id = extract_created_session_id(&stderr);

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["agent_name"], "coder");
    assert_eq!(meta["model"], "openai/gpt-4.1");
    assert_eq!(meta["effort"], "medium");
    assert!(meta.get("plan_enabled").is_none());
    assert_eq!(meta["revision"], 1);
}

#[test]
fn new_session_uses_config_default_agent_when_agent_is_omitted() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "hello from default"
                }
            }
        ]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");

    let output = workspace
        .command()
        .args(["--session", "new", "hello"])
        .output()
        .expect("run output");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout"),
        "hello from default"
    );
    let session_id = extract_created_session_id(&String::from_utf8(output.stderr).expect("stderr"));

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["agent_name"], "coder");
}

#[test]
fn new_alias_matches_session_new_behavior() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "hello from alias"
                }
            }
        ]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");

    let output = workspace
        .command()
        .args(["--new", "hello"])
        .output()
        .expect("run output");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("stdout"),
        "hello from alias"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("created session "));
    let session_id = extract_created_session_id(&stderr);

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["agent_name"], "coder");
}

#[test]
fn unbound_existing_session_uses_config_default_agent_when_agent_is_omitted() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "bound later"
                }
            }
        ]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");

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
        .args(["--session", &session_id, "hello"])
        .output()
        .expect("run output");

    assert!(output.status.success());
    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["agent_name"], "coder");
}

#[test]
fn explicit_agent_overrides_default_agent_for_unbound_sessions() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "hello from reviewer"
                }
            }
        ]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");
    workspace.write_named_agent(
        &workspace.repo_root,
        "reviewer",
        &server.url(),
        "repo reviewer",
    );

    let output = workspace
        .command()
        .args(["--session", "new", "--agent", "reviewer", "hello"])
        .output()
        .expect("run output");

    assert!(output.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(output.stderr).expect("stderr"));

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["agent_name"], "reviewer");
}

#[test]
fn bound_sessions_still_reject_mismatched_explicit_agent() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [
            {
                "message": {
                    "content": "first"
                }
            }
        ]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");
    workspace.write_named_agent(
        &workspace.repo_root,
        "reviewer",
        &server.url(),
        "repo reviewer",
    );

    let first = workspace
        .command()
        .args(["--new", "hello"])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    let second = workspace
        .command()
        .args([
            "--session",
            &session_id,
            "--agent",
            "reviewer",
            "should fail",
        ])
        .output()
        .expect("second run");

    assert_eq!(second.status.code(), Some(4));
    assert!(
        String::from_utf8(second.stderr)
            .expect("stderr")
            .contains("bound to agent `coder`, not `reviewer`")
    );
}

#[test]
fn later_run_overrides_do_not_mutate_sticky_session_defaults() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "first" } }] })),
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "second" } }] })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let worktree_one = workspace.worktree.join("one");
    let worktree_two = workspace.worktree.join("two");
    fs::create_dir_all(&worktree_one).expect("worktree one");
    fs::create_dir_all(&worktree_two).expect("worktree two");

    let first = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--model",
            "model/one",
            "--effort",
            "high",
            "--cwd",
            worktree_one.to_str().expect("cwd"),
            "first run",
        ])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    let second = workspace
        .command()
        .args([
            "--session",
            &session_id,
            "--model",
            "model/two",
            "--effort",
            "low",
            "--cwd",
            worktree_two.to_str().expect("cwd"),
            "second run",
        ])
        .output()
        .expect("second run");
    assert!(second.status.success());

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["model"], "model/one");
    assert_eq!(meta["effort"], "high");
    assert_eq!(meta["cwd"], Value::String(path_string(&worktree_one)));
}

#[test]
fn fork_creates_branch_session_and_preserves_source_session() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "first answer" } }] })),
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "forked answer" } }] })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");

    let first = workspace
        .command()
        .args(["--new", "first prompt"])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let source_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    let second = workspace
        .command()
        .args(["--session", &source_id, "--fork", "continue"])
        .output()
        .expect("fork run");
    assert!(second.status.success());
    assert_eq!(
        String::from_utf8(second.stdout).expect("stdout"),
        "forked answer"
    );
    let stderr = String::from_utf8(second.stderr).expect("stderr");
    let fork_prefix = format!("forked session ");
    let fork_line = stderr
        .lines()
        .find(|line| line.starts_with(&fork_prefix))
        .expect("fork stderr line");
    let fork_id = fork_line
        .split_whitespace()
        .nth(2)
        .expect("forked session id")
        .to_string();
    assert_ne!(fork_id, source_id);
    assert!(fork_line.ends_with(&format!("from {source_id}")));

    let source_meta_output = workspace
        .command()
        .args(["session", "show", &source_id])
        .output()
        .expect("source show");
    let source_meta: Value = serde_json::from_slice(&source_meta_output.stdout).expect("meta");
    assert_eq!(source_meta["revision"], 1);

    let fork_meta_output = workspace
        .command()
        .args(["session", "show", &fork_id])
        .output()
        .expect("fork show");
    let fork_meta: Value = serde_json::from_slice(&fork_meta_output.stdout).expect("meta");
    assert_eq!(fork_meta["revision"], 2);
    assert_eq!(fork_meta["agent_name"], "coder");

    let source_messages = fs::read_to_string(
        workspace
            .sessions_dir
            .join(&source_id)
            .join("messages.jsonl"),
    )
    .expect("source messages")
    .lines()
    .count();
    let fork_messages =
        fs::read_to_string(workspace.sessions_dir.join(&fork_id).join("messages.jsonl"))
            .expect("fork messages")
            .lines()
            .count();
    assert_eq!(source_messages, 2);
    assert_eq!(fork_messages, 4);

    let source_run_count = fs::read_dir(workspace.sessions_dir.join(&source_id).join("runs"))
        .expect("source runs")
        .count();
    let fork_run_count = fs::read_dir(workspace.sessions_dir.join(&fork_id).join("runs"))
        .expect("fork runs")
        .count();
    assert_eq!(source_run_count, 1);
    assert_eq!(fork_run_count, 1);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1]["messages"][1]["content"], "first prompt");
    assert_eq!(requests[1]["messages"][2]["content"], "first answer");
    assert_eq!(requests[1]["messages"][3]["content"], "continue");
}

#[test]
fn forking_a_stopped_session_creates_an_active_branch() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "first answer" } }] })),
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "forked answer" } }] })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_default_agent(&server.url(), "coder");

    let first = workspace
        .command()
        .args(["--new", "first prompt"])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let source_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    workspace
        .command()
        .args(["session", "stop", &source_id])
        .assert()
        .success();

    let second = workspace
        .command()
        .args(["--session", &source_id, "--fork", "continue"])
        .output()
        .expect("fork run");
    assert!(second.status.success());

    let stderr = String::from_utf8(second.stderr).expect("stderr");
    let fork_id = stderr
        .lines()
        .find(|line| line.starts_with("forked session "))
        .and_then(|line| line.split_whitespace().nth(2))
        .expect("fork session id")
        .to_string();

    let source_meta_output = workspace
        .command()
        .args(["session", "show", &source_id])
        .output()
        .expect("source show");
    let source_meta: Value = serde_json::from_slice(&source_meta_output.stdout).expect("meta");
    assert!(source_meta["stopped_at"].is_string());

    let fork_meta_output = workspace
        .command()
        .args(["session", "show", &fork_id])
        .output()
        .expect("fork show");
    let fork_meta: Value = serde_json::from_slice(&fork_meta_output.stdout).expect("meta");
    assert!(fork_meta["stopped_at"].is_null());
}

#[test]
fn role_selected_on_new_session_persists_system_injection_and_uses_user_injection_once() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "first" } }] })),
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "second" } }] })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let first = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--role",
            "auditor",
            "audit this change",
        ])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    let second = workspace
        .command()
        .args(["--session", &session_id, "follow up"])
        .output()
        .expect("second run");
    assert!(second.status.success());

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]["messages"][0]["content"],
        "repo coder\n\nauditor system"
    );
    assert_eq!(
        requests[0]["messages"][1]["content"],
        "risk-focused user prefix\naudit this change"
    );
    assert_eq!(
        requests[1]["messages"][0]["content"],
        "repo coder\n\nauditor system"
    );
    assert_eq!(
        requests[1]["messages"][1]["content"],
        "risk-focused user prefix\naudit this change"
    );
    assert_eq!(requests[1]["messages"][3]["content"], "follow up");

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["initial_role"], "auditor");
}

#[test]
fn role_can_be_added_once_to_an_existing_bound_session() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "plain" } }] })),
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "audited" } }] })),
        ResponseSpec::json(json!({ "choices": [{ "message": { "content": "done" } }] })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let first = workspace
        .command()
        .args(["--session", "new", "--agent", "coder", "plain start"])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    let second = workspace
        .command()
        .args([
            "--session",
            &session_id,
            "--role",
            "auditor",
            "switch to audit",
        ])
        .output()
        .expect("second run");
    assert!(second.status.success());

    let third = workspace
        .command()
        .args(["--session", &session_id, "after audit"])
        .output()
        .expect("third run");
    assert!(third.status.success());

    let requests = server.requests();
    assert_eq!(requests[0]["messages"][0]["content"], "repo coder");
    assert_eq!(requests[0]["messages"][1]["content"], "plain start");
    assert_eq!(
        requests[1]["messages"][0]["content"],
        "repo coder\n\nauditor system"
    );
    assert_eq!(
        requests[1]["messages"][3]["content"],
        "risk-focused user prefix\nswitch to audit"
    );
    assert_eq!(
        requests[2]["messages"][0]["content"],
        "repo coder\n\nauditor system"
    );
    assert_eq!(requests[2]["messages"][5]["content"], "after audit");

    let show_output = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show_output.stdout).expect("meta json");
    assert_eq!(meta["initial_role"], "auditor");
}

#[test]
fn role_cannot_be_selected_more_than_once_per_session() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [{ "message": { "content": "first" } }]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let first = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--role",
            "auditor",
            "audit this change",
        ])
        .output()
        .expect("first run");
    assert!(first.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));

    let second = workspace
        .command()
        .args(["--session", &session_id, "--role", "auditor", "should fail"])
        .output()
        .expect("second run");
    assert_eq!(second.status.code(), Some(4));
    assert!(
        String::from_utf8(second.stderr)
            .expect("stderr")
            .contains("already has role `auditor` active")
    );
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn plan_mode_is_per_run_instead_of_sticky_session_state() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "write_file",
                            "arguments": "{\"path\":\"planned.txt\",\"content\":\"from plan\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "planned run"
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
                            "arguments": "{\"path\":\"real.txt\",\"content\":\"real write\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": "real run"
                }
            }]
        })),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let first = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--plan",
            "--cwd",
            workspace.worktree.to_str().expect("cwd"),
            "plan run",
        ])
        .output()
        .expect("plan run");
    assert!(first.status.success());
    let session_id = extract_created_session_id(&String::from_utf8(first.stderr).expect("stderr"));
    assert!(!workspace.worktree.join("planned.txt").exists());

    let second = workspace
        .command()
        .args([
            "--session",
            &session_id,
            "--cwd",
            workspace.worktree.to_str().expect("cwd"),
            "real run",
        ])
        .output()
        .expect("real run");
    assert!(second.status.success());
    assert_eq!(
        fs::read_to_string(workspace.worktree.join("real.txt")).expect("real file"),
        "real write"
    );
}

#[test]
fn stopped_sessions_reject_new_prompt_runs() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout).expect("stdout");

    workspace
        .command()
        .args(["session", "stop", session_id.trim()])
        .assert()
        .success();

    let output = workspace
        .command()
        .args([
            "--session",
            session_id.trim(),
            "--agent",
            "coder",
            "should fail",
        ])
        .output()
        .expect("blocked run");
    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8(output.stderr)
            .expect("stderr")
            .contains("stopped")
    );
}

#[test]
fn session_stop_missing_id_returns_session_error_code() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    let output = workspace
        .command()
        .args(["session", "stop", "missing"])
        .output()
        .expect("stop missing");
    assert_eq!(output.status.code(), Some(4));
    assert!(
        String::from_utf8(output.stderr)
            .expect("stderr")
            .contains("does not exist")
    );
}

#[test]
fn provider_timeout_returns_timeout_exit_code() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::delayed_json(
        json!({
            "choices": [{
                "message": {
                    "content": "too slow"
                }
            }]
        }),
        1_500,
    )]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_timeout(&server.url(), "1s");

    let output = workspace
        .command()
        .args(["--session", "new", "--agent", "coder", "slow provider"])
        .output()
        .expect("provider timeout");
    assert_eq!(output.status.code(), Some(7));
}

#[test]
fn combined_tool_and_provider_time_budget_returns_timeout_exit_code() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"sleep 0.6\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": "too late"
                    }
                }]
            }),
            900,
        ),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets_with_timeout(&server.url(), "1s");

    let mut command = workspace.std_command();
    command.args([
        "--session",
        "new",
        "--agent",
        "coder",
        "--cwd",
        workspace.worktree.to_str().expect("cwd"),
        "mixed timeout",
    ]);
    let started = Instant::now();
    let output = command.output().expect("mixed timeout");
    let elapsed = started.elapsed();
    assert_eq!(output.status.code(), Some(7));
    assert!(
        elapsed < Duration::from_millis(1_900),
        "provider call should respect the remaining budget, elapsed={elapsed:?}"
    );
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}
