mod common;

use std::{fs, time::Duration};

use serde_json::json;
use tempfile::TempDir;

use common::new_run_control;
use headless::{
    config::GlobalConfig,
    error::AppError,
    tools::{ToolContext, execute_tool},
};

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
        "new file\n"
    );
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
