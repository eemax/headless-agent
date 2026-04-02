mod common;

use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use common::{FakeOpenRouter, ResponseSpec, TestWorkspace, new_run_control};
use headless::{
    config::GlobalConfig,
    error::AppError,
    tools::{ToolContext, execute_tool},
};

#[test]
fn plan_mode_returns_non_executing_results_for_all_core_tools() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("existing.txt"), "original").expect("existing file");

    let config = GlobalConfig {
        sessions_dir: cwd.join("sessions"),
        shell: "/bin/bash".to_string(),
        shell_args: vec!["-lc".to_string()],
        max_stdin_bytes: 1024,
        artifact_preview_bytes: 256,
        catastrophic_output_bytes: 4096,
        log_level: "error".to_string(),
        api_key: None,
        api_key_env: None,
        source_path: None,
    };
    let enabled = vec![
        "read_file".to_string(),
        "edit_file".to_string(),
        "write_file".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "apply_patch".to_string(),
        "bash".to_string(),
    ];
    let run_control = new_run_control(&config, Duration::from_secs(5));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        true,
        &config.shell,
        &config.shell_args,
        &run_control,
    );

    let cases = vec![
        ("read_file", json!({ "path": "existing.txt" })),
        (
            "edit_file",
            json!({ "path": "existing.txt", "old_text": "original", "new_text": "changed" }),
        ),
        (
            "write_file",
            json!({ "path": "new.txt", "content": "hello" }),
        ),
        ("glob", json!({ "pattern": "*.txt" })),
        ("grep", json!({ "pattern": "orig", "path": "." })),
        (
            "apply_patch",
            json!({ "patch": "*** Begin Patch\n*** Add File: sample.txt\n+hello\n*** End Patch" }),
        ),
        ("bash", json!({ "command": "echo should-not-run" })),
    ];

    for (name, args) in cases {
        let execution = execute_tool(&context, &enabled, name, &args).expect("planned result");
        let payload: Value = serde_json::from_str(&execution.content).expect("json payload");
        assert_eq!(payload["planned"], true, "tool {name} should plan");
    }

    assert_eq!(
        fs::read_to_string(cwd.join("existing.txt")).expect("existing content"),
        "original"
    );
    assert!(!cwd.join("new.txt").exists());
    assert!(!cwd.join("sample.txt").exists());
}

#[test]
fn tool_allowlist_and_artifact_backing_behave_as_expected() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let config = GlobalConfig {
        sessions_dir: cwd.join("sessions"),
        ..test_config(cwd)
    };
    let run_control = new_run_control(&config, Duration::from_secs(5));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );

    let denied = execute_tool(
        &context,
        &["read_file".to_string()],
        "bash",
        &json!({ "command": "pwd" }),
    )
    .expect("allowlist response");
    let denied_payload: Value = serde_json::from_str(&denied.content).expect("denied payload");
    assert_eq!(denied_payload["ok"], false);
    assert!(
        denied_payload["error"]
            .as_str()
            .expect("error")
            .contains("not enabled")
    );

    let artifact_config = GlobalConfig {
        artifact_preview_bytes: 32,
        ..config.clone()
    };
    let artifact_run_control = new_run_control(&artifact_config, Duration::from_secs(5));
    let artifact_context = ToolContext::new(
        cwd,
        &run_dir,
        &artifact_config,
        false,
        &artifact_config.shell,
        &artifact_config.shell_args,
        &artifact_run_control,
    );
    let bash = execute_tool(
        &artifact_context,
        &["bash".to_string()],
        "bash",
        &json!({ "command": "printf 'abcdefghijklmnopqrstuvwxyz'" }),
    )
    .expect("bash execution");
    assert!(bash.artifact.is_some());
}

#[test]
fn bash_timeout_kills_the_process_group() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let config = GlobalConfig {
        artifact_preview_bytes: 4_096,
        ..test_config(cwd)
    };
    let run_control = new_run_control(&config, Duration::from_secs(1));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );
    let child_pid_path = cwd.join("child.pid");
    let command = format!(
        "sleep 5 & child=$!; echo $child > {}; wait $child",
        child_pid_path.display()
    );

    let started = Instant::now();
    let error = execute_tool(
        &context,
        &["bash".to_string()],
        "bash",
        &json!({ "command": command }),
    )
    .expect_err("bash timeout");
    assert!(matches!(error, AppError::Timeout(_)));
    assert!(started.elapsed() < Duration::from_secs(3));

    let child_pid = fs::read_to_string(&child_pid_path)
        .expect("child pid")
        .trim()
        .to_string();
    thread::sleep(Duration::from_millis(100));
    let status = Command::new("kill")
        .args(["-0", &child_pid])
        .stderr(Stdio::null())
        .status()
        .expect("kill -0");
    assert!(!status.success(), "timed out child process should be gone");
}

#[test]
fn invalid_multi_file_patch_leaves_existing_files_unchanged() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    let file_path = cwd.join("existing.txt");
    fs::write(&file_path, "before\n").expect("existing file");

    let config = test_config(cwd);
    let run_control = new_run_control(&config, Duration::from_secs(5));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );
    let patch = "\
*** Begin Patch
*** Update File: existing.txt
@@
-before
+after
*** Update File: missing.txt
@@
-nope
+still nope
*** End Patch";

    let error = execute_tool(
        &context,
        &["apply_patch".to_string()],
        "apply_patch",
        &json!({ "patch": patch }),
    )
    .expect_err("invalid patch");
    assert!(matches!(error, AppError::Tool(_)));
    assert_eq!(
        fs::read_to_string(&file_path).expect("existing content"),
        "before\n"
    );
}

#[test]
fn valid_multi_file_patch_commits_all_requested_changes() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("existing.txt"), "before\n").expect("existing file");
    fs::write(cwd.join("move_me.txt"), "hello\n").expect("move source");

    let config = test_config(cwd);
    let run_control = new_run_control(&config, Duration::from_secs(5));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );
    let patch = "\
*** Begin Patch
*** Update File: existing.txt
@@
-before
+after
*** Update File: move_me.txt
*** Move to: moved/move_me.txt
@@
 hello
*** Add File: added.txt
+new file
*** End Patch";

    let execution = execute_tool(
        &context,
        &["apply_patch".to_string()],
        "apply_patch",
        &json!({ "patch": patch }),
    )
    .expect("valid patch");
    assert!(!execution.content.is_empty());
    assert_eq!(
        fs::read_to_string(cwd.join("existing.txt")).expect("updated file"),
        "after\n"
    );
    assert_eq!(
        fs::read_to_string(cwd.join("moved/move_me.txt")).expect("moved file"),
        "hello\n"
    );
    assert!(!cwd.join("move_me.txt").exists());
    assert_eq!(
        fs::read_to_string(cwd.join("added.txt")).expect("added file"),
        "new file"
    );
}

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
            .join(session_id)
            .join("messages.jsonl"),
    )
    .expect("messages");
    assert!(messages.contains("\"name\":\"write_file\""));
    assert!(messages.contains("\"name\":\"read_file\""));
    assert!(messages.contains("\"name\":\"bash\""));
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

    thread::sleep(Duration::from_millis(100));

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
    let started = Instant::now();
    let second_output = second.output().expect("second output");
    let elapsed = started.elapsed();
    let first_output = first_handle.join().expect("join first");

    assert!(first_output.status.success());
    assert_eq!(second_output.status.code(), Some(5));
    assert!(elapsed < Duration::from_millis(400));
    assert!(!workspace.worktree.join("blocked.txt").exists());
}

fn test_config(cwd: &std::path::Path) -> GlobalConfig {
    GlobalConfig {
        sessions_dir: cwd.join("sessions"),
        shell: "/bin/bash".to_string(),
        shell_args: vec!["-lc".to_string()],
        max_stdin_bytes: 1024,
        artifact_preview_bytes: 256,
        catastrophic_output_bytes: 4096,
        log_level: "error".to_string(),
        api_key: None,
        api_key_env: None,
        source_path: None,
    }
}
