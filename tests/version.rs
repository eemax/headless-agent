mod common;

use common::TestWorkspace;

#[test]
fn version_prints_crate_version() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://127.0.0.1:9");

    workspace
        .command()
        .arg("version")
        .assert()
        .success()
        .stdout("0.1.0\n")
        .stderr("");
}
