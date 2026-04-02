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
fn read_file_truncates_at_default_line_cap_and_reports_total() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let mut content = String::new();
    for i in 1..=3000 {
        content.push_str(&format!("line {i}\n"));
    }
    fs::write(cwd.join("big.txt"), &content).expect("big file");

    let config = large_output_config(cwd);
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
    let execution = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "big.txt" }),
    )
    .expect("read execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["total_lines"], 3000);
    assert_eq!(payload["end_line"], 2000);
    assert_eq!(payload["truncated"], true);
    assert!(payload["note"].as_str().unwrap().contains("3000 lines"));
    assert!(
        payload["note"]
            .as_str()
            .unwrap()
            .contains("start_line/end_line")
    );
}

#[test]
fn read_file_explicit_range_bypasses_default_cap() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let mut content = String::new();
    for i in 1..=3000 {
        content.push_str(&format!("line {i}\n"));
    }
    fs::write(cwd.join("big.txt"), &content).expect("big file");

    let config = large_output_config(cwd);
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
    let execution = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "big.txt", "start_line": 2900, "end_line": 3000 }),
    )
    .expect("read execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["total_lines"], 3000);
    assert_eq!(payload["start_line"], 2900);
    assert_eq!(payload["end_line"], 3000);
    assert!(payload.get("truncated").is_none());
    let text = payload["content"].as_str().expect("content");
    assert!(text.contains("line 2900"));
    assert!(text.contains("line 3000"));
}

#[test]
fn read_file_explicit_range_beyond_eof_clamps_end_line() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let mut content = String::new();
    for i in 1..=50 {
        content.push_str(&format!("line {i}\n"));
    }
    fs::write(cwd.join("small.txt"), &content).expect("small file");

    let config = large_output_config(cwd);
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
    let execution = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "small.txt", "start_line": 45, "end_line": 80 }),
    )
    .expect("read execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["total_lines"], 50);
    assert_eq!(payload["start_line"], 45);
    assert_eq!(payload["end_line"], 50);
    assert!(payload.get("truncated").is_none());
    let text = payload["content"].as_str().expect("content");
    assert!(text.contains("line 45"));
    assert!(text.contains("line 50"));
    assert!(!text.contains("line 44"));
}

#[test]
fn read_file_small_file_returns_total_lines_without_truncation() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let mut content = String::new();
    for i in 1..=100 {
        content.push_str(&format!("line {i}\n"));
    }
    fs::write(cwd.join("small.txt"), &content).expect("small file");

    let config = large_output_config(cwd);
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
    let execution = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "small.txt" }),
    )
    .expect("read execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["total_lines"], 100);
    assert_eq!(payload["start_line"], 1);
    assert_eq!(payload["end_line"], 100);
    assert!(payload.get("truncated").is_none());
    let text = payload["content"].as_str().expect("content");
    assert!(text.contains("line 1"));
    assert!(text.contains("line 100"));
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

fn large_output_config(cwd: &std::path::Path) -> GlobalConfig {
    GlobalConfig {
        artifact_preview_bytes: 1024 * 1024,
        catastrophic_output_bytes: 16 * 1024 * 1024,
        ..test_config(cwd)
    }
}
