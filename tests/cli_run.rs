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
