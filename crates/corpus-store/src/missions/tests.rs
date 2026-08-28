use std::collections::BTreeMap;
use std::fs;

use super::*;
use crate::store::Store;

fn tmp_store(tag: &str) -> Store {
    let world = std::env::temp_dir().join(format!("corpus-missions-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&world);
    Store::new(world.join("store"))
}

#[test]
fn legacy_mission_without_workspace_identity_remains_compatible() {
    let store = tmp_store("legacy-workspace");
    store.create_project("p", "P", "fixture").unwrap();
    fs::create_dir_all(store.project_missions_dir("p")).unwrap();
    fs::write(
        store.project_missions_dir("p").join("legacy.md"),
        "---\nagent: runner\npins: {}\ncreated: 1\nopencode_session: ses_old\n---\n\nbrief\n",
    )
    .unwrap();

    let loaded = store.load_mission("p", "legacy").unwrap();
    assert_eq!(loaded.opencode_session.as_deref(), Some("ses_old"));
    assert_eq!(loaded.opencode_workspace, None);
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn an_historical_orphan_mission_can_be_updated_for_teardown_and_deleted() {
    let store = tmp_store("orphan-mission-delete");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "gone", crate::agents::AgentRole::Tester)
        .unwrap();
    let mut mission = Mission {
        agent: "gone".into(),
        pins: BTreeMap::new(),
        budget: None,
        created: 1,
        name: None,
        session: Some("corpus-old-run".into()),
        control: None,
        opencode_session: Some("ses_old".into()),
        opencode_workspace: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    store
        .write_mission("p", "orphan", &mission, "brief")
        .unwrap();

    // Reproduce the historical bug: the agent disappeared without its
    // mission. New delete_agent calls cannot create this state.
    fs::remove_dir_all(store.project_agent_dir("p", "gone")).unwrap();
    mission.session = None;
    store
        .update_mission("p", "orphan", &mission)
        .expect("teardown bookkeeping tolerates the old orphan");
    store
        .delete_mission("p", "orphan")
        .expect("the orphan can be removed");
    assert!(store.load_mission("p", "orphan").is_err());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn active_environment_identity_blocks_every_delete_cascade() {
    let store = tmp_store("active-environment-delete");
    store.create_project("p", "P", "fixture-regtest").unwrap();
    store
        .create_agent_with_role("p", "runner", crate::agents::AgentRole::Tester)
        .unwrap();
    let id = crate::EnvironmentSessionId {
        project: "p".into(),
        mission: "probe".into(),
        generation: 1,
    };
    let key = id.storage_key();
    store
        .save_environment_session(&crate::EnvironmentSessionRecord {
            id,
            plugin_id: "fixture-regtest".into(),
            plugin_version: "1.0.0".into(),
            plugin_digest: "sha256:fixture".into(),
            state: crate::EnvironmentSessionState::Ready,
            source_shas: BTreeMap::new(),
            environment_lock: None,
            image_digest: None,
            created: 1,
            updated: 1,
            error: None,
        })
        .unwrap();
    let mission = Mission {
        agent: "runner".into(),
        pins: BTreeMap::new(),
        budget: None,
        created: 1,
        name: None,
        session: None,
        control: None,
        opencode_session: None,
        opencode_workspace: None,
        environment_session: Some(key.clone()),
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    store
        .write_mission("p", "probe", &mission, "brief")
        .unwrap();

    for error in [
        store.delete_mission("p", "probe").unwrap_err(),
        store.delete_agent("p", "runner").unwrap_err(),
        store.delete_project("p").unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("lifecycle teardown first"),
            "{error}"
        );
    }
    assert!(store.load_mission("p", "probe").is_ok());
    assert!(store.project_agent_dir("p", "runner").is_dir());
    assert!(store.project_dir("p").is_dir());

    let mut environment = store
        .load_environment_session_key("fixture-regtest", &key)
        .unwrap();
    environment.state = crate::EnvironmentSessionState::Closed;
    store.save_environment_session(&environment).unwrap();
    store.delete_agent("p", "runner").unwrap();
    assert!(store.load_mission("p", "probe").is_err());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn legacy_launch_timestamp_deserializes_without_inventing_an_origin() {
    let legacy: Mission =
        crate::yaml::from_str("agent: keeper\nlaunch_requested: 1700000000\n").unwrap();
    assert_eq!(
        legacy.launch_requested,
        Some(MissionLaunchRequest {
            requested_at: 1_700_000_000,
            requested_by: None,
        })
    );
    assert_eq!(legacy.dispatch, None);

    let current = MissionLaunchRequest {
        requested_at: 1_700_000_001,
        requested_by: Some(MissionRunRef {
            project: "p".into(),
            mission: "curator-a".into(),
            run_id: "p1-p-m9-curator-a-g2".into(),
        }),
    };
    let yaml = crate::yaml::to_string(&current).unwrap();
    assert!(yaml.contains("requested_at: 1700000001"), "{yaml}");
    assert!(yaml.contains("mission: curator-a"), "{yaml}");
    assert_eq!(
        crate::yaml::from_str::<MissionLaunchRequest>(&yaml).unwrap(),
        current
    );
}
