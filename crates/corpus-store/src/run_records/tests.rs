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
