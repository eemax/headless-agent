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
fn grep_caps_results_and_reports_truncation() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    for i in 0..200 {
        let dir = cwd.join(format!("d{i:03}"));
        fs::create_dir_all(&dir).expect("subdir");
        let mut content = String::new();
        for j in 0..10 {
            content.push_str(&format!("match_line_{j}\n"));
        }
        fs::write(dir.join("file.txt"), &content).expect("file");
    }

    let config = large_output_config(cwd);
    let run_control = new_run_control(&config, Duration::from_secs(10));
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
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "match_line", "path": "." }),
    )
    .expect("grep execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1000);
    assert_eq!(payload["truncated"], true);
}

#[test]
fn grep_skips_ignored_directories() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let git_dir = cwd.join(".git");
    fs::create_dir_all(&git_dir).expect(".git dir");
    fs::write(git_dir.join("config"), "findme").expect("git file");

    let nm_dir = cwd.join("node_modules");
    fs::create_dir_all(&nm_dir).expect("node_modules dir");
    fs::write(nm_dir.join("lib.js"), "findme").expect("nm file");

    fs::write(cwd.join("src.txt"), "findme").expect("src file");

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
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "findme", "path": "." }),
    )
    .expect("grep execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], "src.txt");
}

#[test]
fn grep_truncation_includes_actionable_metadata() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    for i in 0..200 {
        let dir = cwd.join(format!("d{i:03}"));
        fs::create_dir_all(&dir).expect("subdir");
        let mut content = String::new();
        for j in 0..10 {
            content.push_str(&format!("match_line_{j}\n"));
        }
        fs::write(dir.join("file.txt"), &content).expect("file");
    }

    let config = large_output_config(cwd);
    let run_control = new_run_control(&config, Duration::from_secs(10));
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
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "match_line", "path": "." }),
    )
    .expect("grep execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["match_limit"], 1000);
    assert!(payload["files_scanned"].as_u64().unwrap() > 0);
    assert!(payload["last_file_scanned"].as_str().is_some());
    assert!(payload["note"].as_str().unwrap().contains("1000 matches"));
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
