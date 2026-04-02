mod common;

use std::{fs, thread};

use serde_json::Value;
use serde_json::json;

use common::{FakeOpenRouter, ResponseSpec, TestWorkspace};

#[test]
fn new_sessions_start_unbound_with_nullable_runtime_fields() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    let created = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new");
    let session_id = String::from_utf8(created.stdout).expect("stdout");

    let show = workspace
        .command()
        .args(["session", "show", session_id.trim()])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show.stdout).expect("meta");

    assert_eq!(meta["agent_name"], Value::Null);
    assert_eq!(meta["model"], Value::Null);
    assert_eq!(meta["effort"], Value::Null);
    assert_eq!(meta["cwd"], Value::Null);
    assert_eq!(meta["plan_enabled"], Value::Null);
}

#[test]
fn same_session_conflicts_fail_cleanly() {
    let server = FakeOpenRouter::start(vec![
        ResponseSpec::delayed_json(
            json!({ "choices": [{ "message": { "content": "one" } }] }),
            200,
        ),
        ResponseSpec::delayed_json(
            json!({ "choices": [{ "message": { "content": "two" } }] }),
            200,
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

    let mut command_one = workspace.std_command();
    command_one.args(["--session", &session_id, "--agent", "coder", "race one"]);
    let mut command_two = workspace.std_command();
    command_two.args(["--session", &session_id, "--agent", "coder", "race two"]);

    let one = thread::spawn(move || command_one.output().expect("command one"));
    let two = thread::spawn(move || command_two.output().expect("command two"));

    let output_one = one.join().expect("join one");
    let output_two = two.join().expect("join two");
    let codes = [output_one.status.code(), output_two.status.code()];
    assert!(codes.contains(&Some(0)));
    assert!(codes.contains(&Some(5)));

    let show = workspace
        .command()
        .args(["session", "show", &session_id])
        .output()
        .expect("session show");
    let meta: Value = serde_json::from_slice(&show.stdout).expect("meta");
    assert_eq!(meta["revision"], 1);

    let messages_path = workspace
        .sessions_dir
        .join(&session_id)
        .join("messages.jsonl");
    let line_count = fs::read_to_string(messages_path)
        .expect("messages")
        .lines()
        .count();
    assert_eq!(line_count, 2);
}
