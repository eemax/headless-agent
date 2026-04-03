mod common;

use common::TestWorkspace;

#[test]
fn webfetch_prints_structured_output_and_exits_zero_for_blocked_hosts() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    let output = workspace
        .command()
        .args(["webfetch", "http://localhost/private"])
        .output()
        .expect("webfetch output");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout");
    assert!(stdout.contains("URL:             http://localhost/private"));
    assert!(stdout.contains("Extraction:      Error"));
    assert!(stdout.contains("Error:           blocked_address"));
    assert!(stdout.contains("(no content)"));
}

#[test]
fn webfetch_without_a_url_is_a_usage_error() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    let output = workspace
        .command()
        .args(["webfetch"])
        .output()
        .expect("webfetch usage output");

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .expect("stderr")
            .contains("invalid `webfetch` command")
    );
}
