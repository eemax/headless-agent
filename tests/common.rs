#![allow(dead_code)]

use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

pub struct TestWorkspace {
    _temp: TempDir,
    pub repo_root: PathBuf,
    pub home_root: PathBuf,
    pub sessions_dir: PathBuf,
    pub worktree: PathBuf,
    home_dir: PathBuf,
}

impl TestWorkspace {
    pub fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let repo_root = temp.path().join("repo-root");
        let home_root = temp.path().join("home-root");
        let sessions_dir = temp.path().join("sessions");
        let worktree = temp.path().join("worktree");
        let home_dir = temp.path().join("home");
        for path in [&repo_root, &home_root, &sessions_dir, &worktree, &home_dir] {
            fs::create_dir_all(path).expect("create test directory");
        }
        Self {
            _temp: temp,
            repo_root,
            home_root,
            sessions_dir,
            worktree,
            home_dir,
        }
    }

    pub fn write_repo_assets(&self, base_url: &str) {
        self.write_root_assets(&self.repo_root, base_url, "repo coder");
    }

    pub fn write_home_assets(&self, base_url: &str) {
        self.write_root_assets(&self.home_root, base_url, "home coder");
    }

    pub fn write_named_agent(&self, root: &Path, name: &str, base_url: &str, prompt_label: &str) {
        fs::create_dir_all(root.join("agents")).expect("agents dir");
        fs::create_dir_all(root.join("prompts")).expect("prompts dir");
        fs::write(
            root.join(format!("agents/{name}.toml")),
            format!(
                r#"name = "{name}"
description = "{prompt_label}"
base_url = "{base_url}"
api_key_env = "OPENROUTER_API_KEY"
default_model = "openai/gpt-4.1"
default_effort = "medium"
max_output_tokens = 12000
compaction_at_tokens = 180000
enabled_tools = ["read_file", "edit_file", "write_file", "glob", "grep", "apply_patch", "bash"]
system_prompt_file = "../prompts/{name}.md"
timeout = "2h"
"#
            ),
        )
        .expect("write agent");
        fs::write(root.join(format!("prompts/{name}.md")), prompt_label).expect("write prompt");
    }

    pub fn command(&self) -> Command {
        let mut command = Command::cargo_bin("headless").expect("binary");
        command.current_dir(&self.worktree);
        command.env("HEADLESS_REPO_ROOT", &self.repo_root);
        command.env("HEADLESS_HOME_ROOT", &self.home_root);
        command.env("HOME", &self.home_dir);
        command.env("OPENROUTER_API_KEY", "test-key");
        command
    }

    pub fn std_command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin("headless"));
        command.current_dir(&self.worktree);
        command.env("HEADLESS_REPO_ROOT", &self.repo_root);
        command.env("HEADLESS_HOME_ROOT", &self.home_root);
        command.env("HOME", &self.home_dir);
        command.env("OPENROUTER_API_KEY", "test-key");
        command
    }

    fn write_root_assets(&self, root: &Path, base_url: &str, prompt_label: &str) {
        fs::create_dir_all(root.join("agents")).expect("agents dir");
        fs::create_dir_all(root.join("roles")).expect("roles dir");
        fs::create_dir_all(root.join("prompts")).expect("prompts dir");
        fs::write(
            root.join("config.toml"),
            format!(
                r#"sessions_dir = "{}"
shell = "/bin/bash"
shell_args = ["-lc"]
max_stdin_bytes = 1048576
artifact_preview_bytes = 256
catastrophic_output_bytes = 65536
log_level = "error"
api_key_env = "OPENROUTER_API_KEY"
"#,
                self.sessions_dir.display()
            ),
        )
        .expect("write config");
        self.write_named_agent(root, "coder", base_url, prompt_label);
        fs::write(
            root.join("roles/auditor.toml"),
            r#"name = "auditor"
description = "auditor role"
system_prompt_file = "../prompts/auditor.md"
user_prefix_file = "../prompts/auditor-user.md"
"#,
        )
        .expect("write role");
        fs::write(root.join("prompts/auditor.md"), "auditor system").expect("write auditor prompt");
        fs::write(
            root.join("prompts/auditor-user.md"),
            "risk-focused user prefix",
        )
        .expect("write auditor user prompt");
    }
}

pub fn extract_created_session_id(stderr: &str) -> String {
    stderr
        .split_whitespace()
        .last()
        .expect("session id in stderr")
        .trim()
        .to_string()
}

#[derive(Debug, Clone)]
pub struct ResponseSpec {
    pub status: u16,
    pub body: Value,
    pub delay_ms: u64,
}

impl ResponseSpec {
    pub fn json(body: Value) -> Self {
        Self {
            status: 200,
            body,
            delay_ms: 0,
        }
    }

    pub fn delayed_json(body: Value, delay_ms: u64) -> Self {
        Self {
            status: 200,
            body,
            delay_ms,
        }
    }
}

pub struct FakeOpenRouter {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeOpenRouter {
    pub fn start(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let response_queue = Arc::new(Mutex::new(VecDeque::from(responses)));
        let stop = Arc::new(AtomicBool::new(false));

        let requests_clone = Arc::clone(&requests);
        let queue_clone = Arc::clone(&response_queue);
        let stop_clone = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            loop {
                if stop_clone.load(Ordering::SeqCst)
                    && queue_clone.lock().expect("queue").is_empty()
                {
                    break;
                }
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Some(spec) = queue_clone.lock().expect("queue").pop_front() else {
                            if stop_clone.load(Ordering::SeqCst) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(10));
                            continue;
                        };
                        handle_connection(stream, spec, &requests_clone);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            url,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn url(&self) -> String {
        self.url.clone()
    }

    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("requests").clone()
    }
}

impl Drop for FakeOpenRouter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(mut stream: TcpStream, spec: ResponseSpec, requests: &Arc<Mutex<Vec<Value>>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read header line") == 0 {
            break;
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().expect("content length");
        }
    }

    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).expect("read body");
    if !body.is_empty() {
        let parsed = serde_json::from_slice::<Value>(&body).expect("parse body");
        requests.lock().expect("requests").push(parsed);
    }

    if spec.delay_ms > 0 {
        thread::sleep(Duration::from_millis(spec.delay_ms));
    }
    let body = spec.body.to_string();
    let status_text = match spec.status {
        200 => "OK",
        401 => "Unauthorized",
        408 => "Request Timeout",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "Test",
    };
    let response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        spec.status,
        status_text,
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .expect("write response");
}
