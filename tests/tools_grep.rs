mod common;

use std::{
    collections::BTreeSet,
    fs,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use tempfile::TempDir;

use common::{new_run_control, new_run_control_with_interrupt};
use headless::{
    config::GlobalConfig,
    error::AppError,
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
fn grep_excludes_git_but_searches_other_hidden_files() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let git_dir = cwd.join(".git");
    fs::create_dir_all(&git_dir).expect(".git dir");
    fs::write(git_dir.join("config"), "findme").expect("git file");

    let hidden_dir = cwd.join(".config");
    fs::create_dir_all(&hidden_dir).expect(".config dir");
    fs::write(hidden_dir.join("settings"), "findme").expect("hidden file");

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
    assert_eq!(matches.len(), 2);
    let paths = matches
        .iter()
        .map(|entry| entry["path"].as_str().expect("path"))
        .collect::<BTreeSet<_>>();
    assert_eq!(paths, BTreeSet::from([".config/settings", "src.txt"]));
}

#[test]
fn grep_respects_repo_ignore_rules() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    fs::create_dir_all(cwd.join(".git")).expect(".git dir");
    fs::write(cwd.join(".gitignore"), "node_modules/\n").expect(".gitignore");
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
fn grep_explicit_ignored_root_returns_no_matches() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    fs::write(cwd.join(".ignore"), "generated/\n").expect(".ignore");
    fs::create_dir_all(cwd.join("generated")).expect("generated dir");
    fs::write(cwd.join("generated/file.txt"), "findme").expect("generated file");

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
        &json!({ "pattern": "findme", "path": "generated" }),
    )
    .expect("grep execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert!(matches.is_empty());
    assert_eq!(payload["files_scanned"], 0);
}

#[test]
fn grep_explicit_git_path_returns_no_matches() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    fs::create_dir_all(cwd.join(".git")).expect(".git dir");
    fs::write(cwd.join(".git/config"), "findme").expect("git file");

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
        &json!({ "pattern": "findme", "path": ".git" }),
    )
    .expect("grep execution");
    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert!(matches.is_empty());
    assert_eq!(payload["files_scanned"], 0);
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

#[test]
fn grep_early_termination_does_not_scan_all_files() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    // 2000 dirs x 10 matches = 20,000 potential matches.
    // With early termination rg should be killed after ~1000,
    // well before scanning all directories.
    for i in 0..2000 {
        let dir = cwd.join(format!("d{i:04}"));
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

    let start = std::time::Instant::now();
    let execution = execute_tool(
        &context,
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "match_line", "path": "." }),
    )
    .expect("grep execution");
    let elapsed = start.elapsed();

    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1000);
    assert_eq!(payload["truncated"], true);
    assert!(
        elapsed < Duration::from_secs(5),
        "grep took {elapsed:?}, expected early termination to complete quickly"
    );
}

#[test]
fn grep_interrupt_returns_runtime_error_promptly() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    for i in 0..5000 {
        let dir = cwd.join(format!("d{i:04}"));
        fs::create_dir_all(&dir).expect("subdir");
        fs::write(dir.join("file.txt"), "no match here\n".repeat(100)).expect("file");
    }

    let config = large_output_config(cwd);
    let interrupted = Arc::new(AtomicBool::new(false));
    let run_control =
        new_run_control_with_interrupt(&config, Duration::from_secs(10), Arc::clone(&interrupted));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        interrupted.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let error = execute_tool(
        &context,
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "definitely-not-present", "path": "." }),
    )
    .expect_err("grep interrupt");
    assert!(matches!(error, AppError::Runtime(_)));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
#[cfg(unix)]
fn grep_returns_partial_results_when_descendants_are_unreadable() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    fs::write(cwd.join("src.txt"), "findme\n").expect("src file");
    fs::write(cwd.join("secret.txt"), "findme\n").expect("secret file");
    let mut perms = fs::metadata(cwd.join("secret.txt"))
        .expect("secret metadata")
        .permissions();
    perms.set_mode(0o000);
    fs::set_permissions(cwd.join("secret.txt"), perms).expect("chmod 000");

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

    let mut restore = fs::metadata(cwd.join("secret.txt"))
        .expect("secret metadata")
        .permissions();
    restore.set_mode(0o600);
    fs::set_permissions(cwd.join("secret.txt"), restore).expect("chmod 600");

    let payload: Value = serde_json::from_str(&execution.content).expect("json");
    let matches = payload["matches"].as_array().expect("matches array");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["partial"], true);
    assert_eq!(
        payload["warning"],
        "Some files could not be searched; results may be incomplete."
    );
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["path"], "src.txt");
}

#[test]
fn grep_invalid_regex_returns_tool_error() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");
    fs::write(cwd.join("src.txt"), "findme\n").expect("src file");

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
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "(", "path": "." }),
    )
    .expect_err("invalid regex should fail");
    assert!(matches!(error, AppError::Tool(_)));
    assert!(error.to_string().contains("regex parse error"));
}

#[test]
fn grep_missing_path_returns_tool_error() {
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
        &["grep".to_string()],
        "grep",
        &json!({ "pattern": "findme", "path": "missing" }),
    )
    .expect_err("missing path should fail");
    assert!(matches!(error, AppError::Tool(_)));
    assert!(error.to_string().contains("search path does not exist"));
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
