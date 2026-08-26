use std::fs;
use std::path::Path;

use super::*;
use crate::store::Store;

fn tmp_store(tag: &str) -> Store {
    let world = std::env::temp_dir().join(format!("corpus-projects-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    Store::new(world.join("store"))
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn validate_slug_rejects_bad_names() {
    for bad in [
        "",
        "..",
        "a/b",
        "Upper",
        "under_score",
        "-lead",
        "trail-",
        "a b",
    ] {
        assert!(validate_slug(bad).is_err(), "should reject {bad:?}");
    }
    for good in ["default", "cdk-regtest", "red-alpha-2"] {
        assert!(validate_slug(good).is_ok(), "should accept {good:?}");
    }
}

#[test]
fn projects_start_empty_and_clones_mirror_only_declared_agents() {
    let store = tmp_store("project-agents");
    store
        .create_project("source", "Source", "cdk-regtest")
        .unwrap();
    assert!(store.list_agents("source").unwrap().is_empty());

    store
        .create_agent_with_role("source", "analyst", crate::agents::AgentRole::Researcher)
        .unwrap();
    store.clone_project("source", "clone", None, false).unwrap();

    let agents: Vec<String> = store
        .list_agents("clone")
        .unwrap()
        .into_iter()
        .map(|(slug, _)| slug)
        .collect();
    assert_eq!(agents, ["analyst"]);
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn wipe_project_corpus_bumps_generation() {
    let store = tmp_store("wipe");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "operator", crate::agents::AgentRole::Tester)
        .unwrap();
    write(&store.project_corpus_dir("p").join("findings/1.md"), "x\n");
    let p = store.wipe_project_corpus("p").unwrap();
    assert_eq!(p.corpus_generation, 1);
    assert!(!store.project_corpus_dir("p").join("findings/1.md").exists());
    assert!(store.project_corpus_dir("p").join("findings").is_dir());
    // agents survive a wipe
    assert!(store
        .project_agent_dir("p", "operator")
        .join("opencode.json")
        .is_file());
    let _ = fs::remove_dir_all(store.root());
}
