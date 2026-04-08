mod common;

use std::{
    fs,
    io::{BufRead, BufReader, Read},
    process::Stdio,
};

use serde_json::json;

use common::{FakeOpenRouter, ResponseSpec, TestWorkspace};

#[test]
fn verbose_reports_reasoning_and_tool_calls() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning_details": [{
                        "type": "reasoning.summary",
                        "text": "inspect repo status"
                    }],
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }, {
                        "id": "call_2",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"printf 'hello'\"}"
                        }
                    }]
                }
            }]
        })),
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

    let output = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--verbose",
            "inspect",
        ])
        .output()
        .expect("run output");

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).expect("stdout"), "done");
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("created session "));
    assert!(stderr.contains("step 1 reasoning: inspect repo status"));
    assert!(stderr.contains("step 1 tool 1/2: bash pwd"));
    assert!(stderr.contains("step 1 tool 2/2: bash printf 'hello'"));
    assert!(!stderr.contains("session="));
    assert!(!stderr.contains("new session initialized"));
}

#[test]
fn verbose_redacts_write_file_content() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "write_file",
                            "arguments": json!({
                                "path": "note.txt",
                                "content": "SUPER SECRET BODY"
                            }).to_string()
                        }
                    }]
                }
            }]
        })),
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

    let output = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--verbose",
            "write the file",
        ])
        .output()
        .expect("run output");

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains(r#"step 1 tool 1/1: write_file path="note.txt""#));
    assert!(!stderr.contains("SUPER SECRET BODY"));
    assert_eq!(
        fs::read_to_string(workspace.worktree.join("note.txt")).expect("note"),
        "SUPER SECRET BODY"
    );
}

#[test]
fn verbose_redacts_apply_patch_body_and_shows_paths() {
    let patch = "\
*** Begin Patch
*** Update File: note.txt
@@
-before
+SECRET_PATCH_TEXT
*** End Patch";
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "apply_patch",
                            "arguments": json!({ "patch": patch }).to_string()
                        }
                    }]
                }
            }]
        })),
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
    fs::write(workspace.worktree.join("note.txt"), "before\n").expect("seed note");

    let output = workspace
        .command()
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--verbose",
            "patch the file",
        ])
        .output()
        .expect("run output");

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("step 1 tool 1/1: apply_patch note.txt"));
    assert!(!stderr.contains("SECRET_PATCH_TEXT"));
    assert_eq!(
        fs::read_to_string(workspace.worktree.join("note.txt")).expect("note"),
        "SECRET_PATCH_TEXT\n"
    );
}

#[test]
fn verbose_progress_is_emitted_before_process_exit() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::json(json!({
            "choices": [{
                "message": {
                    "content": null,
                    "reasoning_details": [{
                        "type": "reasoning.summary",
                        "text": "inspect the workspace"
                    }],
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {
                            "name": "bash",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                }
            }]
        })),
        ResponseSpec::delayed_json(
            json!({
                "choices": [{
                    "message": {
                        "content": "done"
                    }
                }]
            }),
            1200,
        ),
    ]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let mut command = workspace.std_command();
    command
        .args([
            "--session",
            "new",
            "--agent",
            "coder",
            "--verbose",
            "inspect",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn child");

    let stderr_handle = child.stderr.take().expect("stderr handle");
    let mut stderr_reader = BufReader::new(stderr_handle);
    let mut line = String::new();
    let mut saw_tool_line = false;
    while stderr_reader
        .read_line(&mut line)
        .expect("read stderr line")
        > 0
    {
        if line.contains("step 1 tool 1/1: bash pwd") {
            saw_tool_line = true;
            assert!(child.try_wait().expect("try_wait").is_none());
            break;
        }
        line.clear();
    }
    assert!(
        saw_tool_line,
        "expected a live verbose tool line before exit"
    );

    let mut remaining_stderr = String::new();
    stderr_reader
        .read_to_string(&mut remaining_stderr)
        .expect("read remaining stderr");
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout handle")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let status = child.wait().expect("wait child");
    assert!(status.success());
    assert_eq!(stdout, "done");
    assert!(!remaining_stderr.contains("session="));
}

#[test]
fn debug_reports_session_metadata_without_verbose_progress() {
    let server = FakeOpenRouter::start(vec![ResponseSpec::json(json!({
        "choices": [{
            "message": {
                "content": "done"
            }
        }]
    }))]);
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets(&server.url());

    let output = workspace
        .command()
        .args(["--session", "new", "--agent", "coder", "--debug", "inspect"])
        .output()
        .expect("run output");

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).expect("stdout"), "done");
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(stderr.contains("created session "));
    assert!(stderr.contains("session="));
    assert!(stderr.contains("new session initialized"));
    assert!(!stderr.contains("step 1 reasoning:"));
    assert!(!stderr.contains("step 1 tool"));
}
