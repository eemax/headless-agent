mod common;

use std::{
    fs,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tempfile::TempDir;

use common::{new_run_control, new_run_control_with_interrupt};
use headless::{
    config::GlobalConfig,
    error::AppError,
    tools::{ToolContext, execute_tool},
};

#[test]
fn bash_timeout_kills_the_process_group() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let config = GlobalConfig {
        artifact_preview_bytes: 4_096,
        ..test_config(cwd)
    };
    let run_control = new_run_control(&config, Duration::from_secs(1));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );
    let child_pid_path = cwd.join("child.pid");
    let command = format!(
        "sleep 5 & child=$!; echo $child > {}; wait $child",
        child_pid_path.display()
    );

    let started = Instant::now();
    let error = execute_tool(
        &context,
        &["bash".to_string()],
        "bash",
        &json!({ "command": command }),
    )
    .expect_err("bash timeout");
    assert!(matches!(error, AppError::Timeout(_)));
    assert!(started.elapsed() < Duration::from_secs(3));

    let child_pid = fs::read_to_string(&child_pid_path)
        .expect("child pid")
        .trim()
        .to_string();
    thread::sleep(Duration::from_millis(100));
    let status = Command::new("kill")
        .args(["-0", &child_pid])
        .stderr(Stdio::null())
        .status()
        .expect("kill -0");
    assert!(!status.success(), "timed out child process should be gone");
}

#[test]
fn bash_output_is_capped_without_changing_exit_status() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let config = GlobalConfig {
        catastrophic_output_bytes: 256,
        artifact_preview_bytes: 128,
        ..test_config(cwd)
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
        &["bash".to_string()],
        "bash",
        &json!({
            "command": "python3 -c \"import sys; sys.stdout.write('x' * 1000000); sys.stdout.flush()\""
        }),
    )
    .expect("bash execution");
    assert!(
        execution.artifact.is_some(),
        "large output should be artifact-backed"
    );
    let artifact_path = run_dir.join(&execution.artifact.as_ref().unwrap().path);
    let artifact_content = fs::read_to_string(&artifact_path).expect("read artifact");
    let payload: Value = serde_json::from_str(&artifact_content).expect("parse artifact json");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(payload["stdout_truncated"], true);
    assert_eq!(payload["stderr"], "");
    assert!(payload["note"].as_str().unwrap().contains("truncated"));
}

#[test]
fn bash_interrupt_kills_the_process_group() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let config = GlobalConfig {
        artifact_preview_bytes: 4_096,
        ..test_config(cwd)
    };
    let interrupted = Arc::new(AtomicBool::new(false));
    let run_control =
        new_run_control_with_interrupt(&config, Duration::from_secs(5), Arc::clone(&interrupted));
    let context = ToolContext::new(
        cwd,
        &run_dir,
        &config,
        false,
        &config.shell,
        &config.shell_args,
        &run_control,
    );
    let child_pid_path = cwd.join("child.pid");
    let command = format!(
        "sleep 5 & child=$!; echo $child > {}; wait $child",
        child_pid_path.display()
    );

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        interrupted.store(true, Ordering::SeqCst);
    });

    let started = Instant::now();
    let error = execute_tool(
        &context,
        &["bash".to_string()],
        "bash",
        &json!({ "command": command }),
    )
    .expect_err("bash interrupt");
    assert!(matches!(error, AppError::Runtime(_)));
    assert!(started.elapsed() < Duration::from_secs(3));

    let child_pid = fs::read_to_string(&child_pid_path)
        .expect("child pid")
        .trim()
        .to_string();
    thread::sleep(Duration::from_millis(100));
    let status = Command::new("kill")
        .args(["-0", &child_pid])
        .stderr(Stdio::null())
        .status()
        .expect("kill -0");
    assert!(
        !status.success(),
        "interrupted child process should be gone"
    );
}

#[test]
fn bash_artifact_backing_stores_large_output() {
    let temp = TempDir::new().expect("tempdir");
    let cwd = temp.path();
    let run_dir = cwd.join("run");
    fs::create_dir_all(&run_dir).expect("run dir");

    let config = GlobalConfig {
        artifact_preview_bytes: 32,
        ..test_config(cwd)
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
    let bash = execute_tool(
        &context,
        &["bash".to_string()],
        "bash",
        &json!({ "command": "printf 'abcdefghijklmnopqrstuvwxyz'" }),
    )
    .expect("bash execution");
    assert!(bash.artifact.is_some());
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
