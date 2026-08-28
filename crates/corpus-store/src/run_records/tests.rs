use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::*;
use crate::missions::Mission;
use crate::store::Store;

fn tmp_store(tag: &str) -> Store {
    let world =
        std::env::temp_dir().join(format!("corpus-run_records-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    Store::new(world.join("store"))
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

#[test]
fn session_keyed_logs_resolve_through_their_mission() {
    let store = tmp_store("session-log-agent");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "runner", crate::agents::AgentRole::Tester)
        .unwrap();
    let mission = Mission {
        agent: "runner".into(),
        pins: BTreeMap::new(),
        budget: None,
        created: 1,
        name: None,
        session: None,
        control: None,
        opencode_session: Some("ses_abc".into()),
        opencode_workspace: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    store
        .write_mission("p", "probe", &mission, "brief")
        .unwrap();
    write(
        &store.project_corpus_dir("p").join("runs/ses_abc.json"),
        "{}",
    );
    write(
        &store.project_corpus_dir("p").join("runs/legacy.json"),
        "{}",
    );

    let logs = mission_logs(&store, "p").unwrap();
    let linked = logs.iter().find(|log| log.name == "ses_abc.json").unwrap();
    assert_eq!(linked.agent.as_deref(), Some("runner"));
    let legacy = logs.iter().find(|log| log.name == "legacy.json").unwrap();
    assert_eq!(legacy.agent, None);
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn compound_run_names_resolve_only_the_agent_prefix() {
    let store = tmp_store("compound-log-agent");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "cdk-recon", crate::agents::AgentRole::Tester)
        .unwrap();
    let uuid = "af2cf850-a33f-46a7-a9ec-0346292a11bb";
    store
        .create_agent_with_role("p", uuid, crate::agents::AgentRole::Tester)
        .unwrap();
    let runs = store.project_corpus_dir("p").join("runs");
    write(
        &runs.join("1786891368-cdk-recon-m01-hypothesis-scan-g1.raw"),
        "capture",
    );
    write(
        &runs.join(format!("1786891369-{uuid}-m02-bea-g1.raw")),
        "capture",
    );
    write(
        &runs.join("1786891370-removed-agent-m03-probe-g2.raw"),
        "capture",
    );

    let logs = mission_logs(&store, "p").unwrap();
    assert_eq!(logs[2].agent.as_deref(), Some("cdk-recon"));
    assert_eq!(logs[1].agent.as_deref(), Some(uuid));
    assert_eq!(logs[0].agent, None);
    let _ = fs::remove_dir_all(store.root());
}
