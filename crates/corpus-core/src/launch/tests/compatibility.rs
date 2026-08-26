use std::fs;

use super::support::tmp_store;
use crate::agents::AgentRole;
use crate::launch::session_raw_log;

/// The session name round-trips to the raw capture `start_tui`
/// writes — the app finds the log of a run it never owned.
#[test]
fn session_raw_log_pairs_with_the_launch_stamp() {
    let (store, dir) = tmp_store("raw-pair");
    let runs = store.project_corpus_dir("p").join("runs");
    assert_eq!(
        session_raw_log(&store, "p", "corpus-discover-1786911614"),
        Some(runs.join("1786911614-discover.raw"))
    );
    // An agent stem with its own dashes keeps them: only the trailing
    // stamp is split off.
    assert_eq!(
        session_raw_log(&store, "p", "corpus-web-scanner-1786911614"),
        Some(runs.join("1786911614-web-scanner.raw"))
    );
    // Not ours / no stamp / no agent: no guessing.
    assert_eq!(session_raw_log(&store, "p", "my-editor"), None);
    assert_eq!(session_raw_log(&store, "p", "corpus-discover-later"), None);
    assert_eq!(session_raw_log(&store, "p", "corpus-1786911614"), None);
    let _ = fs::remove_dir_all(&dir);
}

/// materialize_agent renders the launched agent's files into
/// `.opencode/agent/` with bare names.
#[test]
fn materialize_agent_renders_agent_files() {
    let (store, dir) = tmp_store("mat-v2");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "operator", AgentRole::Tester)
        .unwrap();
    let written = store.render_agent("p", "operator").unwrap();
    assert!(!written.is_empty());
    let dest = &written[0];
    assert!(dest.ends_with("operator.md"), "{dest:?}");
    let text = fs::read_to_string(dest).unwrap();
    assert!(text.contains("mode: primary"), "{text}");
    assert!(text.contains("corpus TESTER"), "the role's prompt: {text}");
    let _ = fs::remove_dir_all(&dir);
}
