mod common;

use std::fs;

use tempfile::TempDir;

use headless::{config::GlobalConfig, session::SessionStore};

fn test_config(root: &std::path::Path) -> GlobalConfig {
    GlobalConfig {
        sessions_dir: root.join("sessions"),
        ..GlobalConfig::default()
    }
}

#[test]
fn session_list_ids_are_sorted_and_ignore_directories_without_meta() {
    let temp = TempDir::new().expect("tempdir");
    let config = test_config(temp.path());
    let store = SessionStore::new(&config);
    store.ensure_root().expect("ensure sessions");

    fs::create_dir_all(store.session_dir("b-session")).expect("dir");
    fs::write(
        store.session_dir("b-session").join("meta.json"),
        r#"{"session_id":"b-session","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","stopped_at":null,"revision":1,"char_count":0,"agent_name":null,"model":null,"role_name":null,"cwd":null,"effort":null}"#,
    )
    .expect("write meta");

    fs::create_dir_all(store.session_dir("a-session")).expect("dir");
    fs::write(
        store.session_dir("a-session").join("meta.json"),
        r#"{"session_id":"a-session","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:00Z","stopped_at":null,"revision":1,"char_count":0,"agent_name":null,"model":null,"role_name":null,"cwd":null,"effort":null}"#,
    )
    .expect("write meta");

    fs::create_dir_all(store.session_dir("missing-meta")).expect("dir");

    let ids = store.list_session_ids().expect("list ids");
    assert_eq!(ids, vec!["a-session".to_string(), "b-session".to_string()]);
}
