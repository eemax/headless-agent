mod common;

use std::{fs, time::Duration};

#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};

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
    assert_eq!(payload["end_line"], 2000);
    assert_eq!(payload["total_lines_lower_bound"], 2001);
    assert_eq!(payload["truncated"], true);
    assert!(
        payload["note"]
            .as_str()
            .unwrap()
            .contains("showing 2000 lines")
    );
    assert!(
        payload["note"]
            .as_str()
            .unwrap()
            .contains("start_line/end_line")
    );
}

#[test]
fn read_file_start_line_only_still_caps_to_default_window() {
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
        &json!({ "path": "big.txt", "start_line": 500 }),
    )
    .expect("read execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let text = payload["content"].as_str().expect("content");
    assert_eq!(payload["start_line"], 500);
    assert_eq!(payload["end_line"], 2499);
    assert_eq!(payload["truncated"], true);
    assert_eq!(payload["total_lines_lower_bound"], 2500);
    assert!(text.contains("line 500"));
    assert!(text.contains("line 2499"));
    assert!(!text.contains("line 2500"));
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
fn read_file_rejects_zero_start_line() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("small.txt"), "line 1\n").expect("small file");

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
    let error = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "small.txt", "start_line": 0 }),
    )
    .expect_err("zero start line should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
}

#[test]
fn read_file_rejects_oversized_lines() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("huge.txt"), vec![b'a'; 1024 * 1024 + 1]).expect("huge line");

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
    let error = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "huge.txt" }),
    )
    .expect_err("oversized line should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
    assert!(error.to_string().contains("line exceeds"));
}

#[test]
fn read_file_rejects_invalid_utf8() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("binary.txt"), vec![0xff, 0xfe, b'\n']).expect("binary file");

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
    let error = execute_tool(
        &context,
        &["read_file".to_string()],
        "read_file",
        &json!({ "path": "binary.txt" }),
    )
    .expect_err("invalid utf8 should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
    assert!(error.to_string().contains("not valid UTF-8"));
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

#[test]
fn edit_file_rejects_empty_old_text() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("target.txt"), "aaa\nbbb\n").expect("target file");

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
    let error = execute_tool(
        &context,
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "target.txt", "old_text": "", "new_text": "x" }),
    )
    .expect_err("empty old_text should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
}

#[test]
fn edit_file_replaces_single_match() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("target.txt"), "aaa\nbbb\nccc\n").expect("target file");

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
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "target.txt", "old_text": "bbb", "new_text": "zzz" }),
    )
    .expect("edit execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["replaced"], true);
    assert_eq!(
        fs::read_to_string(cwd.join("target.txt")).expect("read back"),
        "aaa\nzzz\nccc\n"
    );
}

#[test]
fn edit_file_rejects_overlapping_matches() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("target.txt"), "aaa\n").expect("target file");

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
    let error = execute_tool(
        &context,
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "target.txt", "old_text": "aa", "new_text": "z" }),
    )
    .expect_err("overlapping matches should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
}

#[test]
fn edit_file_rejects_zero_matches() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("target.txt"), "aaa\nbbb\n").expect("target file");

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
    let error = execute_tool(
        &context,
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "target.txt", "old_text": "missing", "new_text": "x" }),
    )
    .expect_err("should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
}

#[test]
fn edit_file_rejects_multiple_matches() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("target.txt"), "aaa\naaa\n").expect("target file");

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
    let error = execute_tool(
        &context,
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "target.txt", "old_text": "aaa", "new_text": "x" }),
    )
    .expect_err("should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
}

#[test]
#[cfg(unix)]
fn edit_file_preserves_existing_permissions() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    let path = cwd.join("script.sh");
    fs::write(&path, "echo before\n").expect("script");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod");

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
    execute_tool(
        &context,
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "script.sh", "old_text": "before", "new_text": "after" }),
    )
    .expect("edit execution");

    assert_eq!(
        fs::metadata(&path).expect("metadata").permissions().mode(),
        0o100755
    );
}

#[test]
#[cfg(unix)]
fn edit_file_through_symlink_updates_target() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    let target = cwd.join("target.txt");
    let link = cwd.join("link.txt");
    fs::write(&target, "before\n").expect("target file");
    symlink(&target, &link).expect("symlink");

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
    execute_tool(
        &context,
        &["edit_file".to_string()],
        "edit_file",
        &json!({ "path": "link.txt", "old_text": "before", "new_text": "after" }),
    )
    .expect("edit execution");

    assert!(
        fs::symlink_metadata(&link)
            .expect("metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).expect("target"), "after\n");
    assert_eq!(fs::read_to_string(&link).expect("link"), "after\n");
}

#[test]
fn write_file_creates_nested_path_with_create_parents() {
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
    let execution = execute_tool(
        &context,
        &["write_file".to_string()],
        "write_file",
        &json!({ "path": "a/b/c/deep.txt", "content": "nested", "create_parents": true }),
    )
    .expect("write execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    assert_eq!(payload["ok"], true);
    assert_eq!(
        fs::read_to_string(cwd.join("a/b/c/deep.txt")).expect("read back"),
        "nested"
    );
}

#[test]
#[cfg(unix)]
fn write_file_preserves_existing_permissions() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    let path = cwd.join("script.sh");
    fs::write(&path, "echo before\n").expect("script");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("chmod");

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
    execute_tool(
        &context,
        &["write_file".to_string()],
        "write_file",
        &json!({ "path": "script.sh", "content": "echo after\n" }),
    )
    .expect("write execution");

    assert_eq!(
        fs::metadata(&path).expect("metadata").permissions().mode(),
        0o100755
    );
}

#[test]
#[cfg(unix)]
fn write_file_new_file_matches_plain_fs_write_permissions() {
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
    execute_tool(
        &context,
        &["write_file".to_string()],
        "write_file",
        &json!({ "path": "tool.txt", "content": "tool\n" }),
    )
    .expect("write execution");
    fs::write(cwd.join("plain.txt"), "plain\n").expect("plain write");

    let tool_mode = fs::metadata(cwd.join("tool.txt"))
        .expect("tool metadata")
        .permissions()
        .mode()
        & 0o777;
    let plain_mode = fs::metadata(cwd.join("plain.txt"))
        .expect("plain metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(tool_mode, plain_mode);
}

#[test]
#[cfg(unix)]
fn write_file_through_symlink_updates_target() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    let target = cwd.join("target.txt");
    let link = cwd.join("link.txt");
    fs::write(&target, "before\n").expect("target file");
    symlink(&target, &link).expect("symlink");

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
    execute_tool(
        &context,
        &["write_file".to_string()],
        "write_file",
        &json!({ "path": "link.txt", "content": "after\n" }),
    )
    .expect("write execution");

    assert!(
        fs::symlink_metadata(&link)
            .expect("metadata")
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).expect("target"), "after\n");
    assert_eq!(fs::read_to_string(&link).expect("link"), "after\n");
}

#[test]
fn write_file_rejects_missing_parent_without_create_parents() {
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
    let error = execute_tool(
        &context,
        &["write_file".to_string()],
        "write_file",
        &json!({ "path": "nonexistent/dir/file.txt", "content": "hello" }),
    )
    .expect_err("should fail");
    assert!(matches!(error, headless::error::AppError::Tool(_)));
}

fn test_config(cwd: &std::path::Path) -> GlobalConfig {
    GlobalConfig {
        sessions_dir: cwd.join("sessions"),
        shell: "/bin/bash".to_string(),
        shell_args: vec!["-lc".to_string()],
        max_stdin_bytes: 1024,
        artifact_preview_bytes: 256,
        catastrophic_output_bytes: 4096,
        default_agent: None,
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
