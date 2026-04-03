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
fn web_fetch_returns_structured_failures_instead_of_tool_errors() {
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
        api_key: None,
        api_key_env: None,
        source_path: None,
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

    let execution = execute_tool(
        &context,
        &["web_fetch".to_string()],
        "web_fetch",
        &json!({ "url": "notaurl" }),
    )
    .expect("structured tool response");
    let payload: Value = serde_json::from_str(&execution.content).expect("json payload");

    assert_eq!(payload["ok"], false);
    assert_eq!(payload["error"], "invalid_url");
    assert_eq!(payload["extraction_kind"], "Error");
    assert_eq!(payload["status"], Value::Null);
}
