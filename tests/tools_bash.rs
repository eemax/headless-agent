mod common;

use std::fs;

use serde_json::{Value, json};
use tempfile::TempDir;

use common::{FakeOpenRouter, ResponseSpec, TestWorkspace};
use headless::{
    config::GlobalConfig,
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
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        true,
        &config.shell,
        &config.shell_args,
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
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
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
    let artifact_context = ToolContext::new(
        cwd,
        &run_dir,
        &artifact_config,
        false,
        &artifact_config.shell,
        &artifact_config.shell_args,
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
