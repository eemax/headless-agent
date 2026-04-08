mod common;

use common::TestWorkspace;
use headless::{
    agent_def::LoadedAgent, config::HeadlessRoots, prompt_def::LoadedPrompt, role_def::LoadedRole,
};

#[test]
fn repo_root_agent_definition_takes_precedence_over_home_root() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://repo.example");
    workspace.write_home_assets("http://home.example");

    let roots = HeadlessRoots {
        repo_root: Some(workspace.repo_root.clone()),
        home_root: Some(workspace.home_root.clone()),
    };
    let agent = LoadedAgent::load(&roots, "coder").expect("load agent");

    assert_eq!(agent.def.base_url.as_deref(), Some("http://repo.example"));
    assert!(agent.system_prompt.contains("repo coder"));
}

#[test]
fn role_prompt_paths_resolve_relative_to_the_role_file() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://repo.example");

    let roots = HeadlessRoots {
        repo_root: Some(workspace.repo_root.clone()),
        home_root: Some(workspace.home_root.clone()),
    };
    let role = LoadedRole::load(&roots, "auditor").expect("load role");

    assert_eq!(role.system_prompt.as_deref(), Some("auditor system"));
}

#[test]
fn named_prompt_paths_resolve_relative_to_the_prompt_file() {
    let workspace = TestWorkspace::new();
    workspace.write_repo_assets("http://repo.example");

    let roots = HeadlessRoots {
        repo_root: Some(workspace.repo_root.clone()),
        home_root: Some(workspace.home_root.clone()),
    };
    let prompt = LoadedPrompt::load(&roots, "auditor").expect("load prompt");

    assert_eq!(prompt.prompt, "risk-focused user prefix");
}
