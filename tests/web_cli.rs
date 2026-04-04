use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn base_command() -> (TempDir, Command) {
    let home = TempDir::new().expect("temp home");
    let mut command = Command::cargo_bin("headless").expect("cargo bin");
    command.env("HOME", home.path());
    command.env_remove("EXA_API_KEY");
    (home, command)
}

#[test]
fn websearch_requires_exa_api_key() {
    let (_home, mut command) = base_command();
    command.arg("websearch").arg("rust");
    command
        .assert()
        .failure()
        .code(8)
        .stderr(predicate::str::contains("missing EXA_API_KEY"));
}

#[test]
fn websearch_requires_a_query() {
    let (_home, mut command) = base_command();
    command
        .arg("websearch")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("missing search query"));
}

#[test]
fn websearch_rejects_invalid_type_before_runtime() {
    let (_home, mut command) = base_command();
    command
        .args(["websearch", "--type", "keyword", "rust"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid `--type` value"));
}

#[test]
fn websearch_rejects_out_of_range_num_results_before_runtime() {
    let (_home, mut command) = base_command();
    command
        .args(["websearch", "--num_results", "0", "rust"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("tool argument `num_results`"));
}
