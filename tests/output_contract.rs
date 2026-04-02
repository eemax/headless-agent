mod common;

use regex::Regex;

use common::TestWorkspace;

#[test]
fn session_new_prints_only_the_ulid_to_stdout() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    let output = workspace
        .command()
        .args(["session", "new"])
        .output()
        .expect("session new output");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(
        Regex::new("^[0-9a-z]{26}\n$")
            .expect("regex")
            .is_match(&stdout)
    );
    assert!(stderr.is_empty());
    assert!(workspace.sessions_dir.join(stdout.trim()).exists());
}

#[test]
fn agent_and_role_list_are_resolved_from_repo_root() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    workspace
        .command()
        .args(["agent", "list"])
        .assert()
        .success()
        .stdout("coder\n");

    workspace
        .command()
        .args(["role", "list"])
        .assert()
        .success()
        .stdout("auditor\n");
}
