use std::fs;
use std::path::PathBuf;

use crate::agents::AgentRole;
use crate::store::Store;

/// The env- and process-mutating launch tests are inherently global
/// (CORPUS_STORE/PATH, tmux sessions, stray processes), so they run
/// under one shared lock instead of racing the parallel test pool.
/// A store in its OWN world: the run dir is a sibling of the store
/// (`<store parent>/var/run/<project>`), so temp stores that shared a
/// parent — every one of them, when the parent was `/tmp` — collided
/// on the run dir of any project with the same slug.
pub(super) fn tmp_store(tag: &str) -> (Store, PathBuf) {
    let world = std::env::temp_dir().join(format!("corpus-launch-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    let dir = world.join("store");
    (Store::new(dir.clone()), dir)
}

/// A project with the two agents the launch tests exercise. Projects
/// no longer come with agents — they are created from a role.
pub(super) fn core_project(store: &Store) {
    store
        .create_project("default", "Default", "cdk-regtest")
        .unwrap();
    store
        .create_agent_with_role("default", "operator", AgentRole::Tester)
        .unwrap();
    store
        .create_agent_with_role("default", "researcher", AgentRole::Researcher)
        .unwrap();
}
