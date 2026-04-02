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
fn glob_skips_ignored_directories() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let target_dir = cwd.join("target").join("debug");
    fs::create_dir_all(&target_dir).expect("target dir");
    fs::write(target_dir.join("bin.txt"), "x").expect("target file");

    fs::write(cwd.join("src.txt"), "x").expect("src file");

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
    let execution = execute_tool(
        &context,
        &["glob".to_string()],
        "glob",
        &json!({ "pattern": "**/*.txt" }),
    )
    .expect("glob execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0], "src.txt");
}

#[test]
fn glob_truncation_includes_result_limit_and_note() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    // Create 10001 files to trigger the 10000 limit
    for i in 0..10_001 {
        fs::write(cwd.join(format!("file_{i:05}.txt")), "x").expect("file");
    }

    let config = large_output_config(cwd);
    let run_control = new_run_control(&config, Duration::from_secs(30));
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
        &["glob".to_string()],
        "glob",
        &json!({ "pattern": "*.txt" }),
    )
    .expect("glob execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["result_limit"], 10_000);
    assert!(payload["note"].as_str().unwrap().contains("10000 paths"));
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
