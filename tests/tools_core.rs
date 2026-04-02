mod common;

use std::{fs, time::Duration};

use serde_json::{Value, json};
use tempfile::TempDir;

use common::new_run_control;
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
fn tool_allowlist_rejects_disabled_tools() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

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

    let denied = execute_tool(
        &context,
        &["read_file".to_string()],
        "bash",
        &json!({ "command": "pwd" }),
    )
    .expect("allowlist response");
    let payload: Value = serde_json::from_str(&denied.content).expect("denied payload");
    assert_eq!(payload["ok"], false);
    assert!(payload["error"].as_str().unwrap().contains("not enabled"));
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
