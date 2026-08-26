use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use corpus_core::{
    AgentRole, MissionCompletion, MissionControl, MissionDispatchIdentity, MissionRunRef, Plugin,
    Scope, Store,
};
use corpus_integration::assertions::assert_exact_parent;
use corpus_integration::TestHarness;
use corpus_mcp::tools::{self, Ctx};
use serde_json::{json, Value};

fn echo_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus-core/tests/echo-plugin")
}

fn admin_call(
    store: &Store,
    confirms: &mut HashMap<String, corpus_admin::PendingConfirm>,
    name: &str,
    args: Value,
) -> String {
    corpus_admin::dispatch(
        &mut corpus_admin::Ctx {
            store,
            pending_confirms: confirms,
        },
        name,
        &args,
    )
    .unwrap_or_else(|error| panic!("admin {name} failed: {error}"))
}

fn create_agent(
    store: &Store,
    confirms: &mut HashMap<String, corpus_admin::PendingConfirm>,
    project: &str,
    slug: &str,
    role: AgentRole,
    model: &str,
) {
    admin_call(
        store,
        confirms,
        "agent_new",
        json!({
            "project": project,
            "agent": slug,
            "description": format!("integration {slug}"),
            "prompt": format!("Act as the {slug} in the integration campaign."),
            "role": role.as_str(),
            "model": model
        }),
    );
}

fn child_identity(store: &Store, project: &str, slug: &str) -> MissionDispatchIdentity {
    let mission = store.load_mission(project, slug).unwrap();
    let dispatch = mission.dispatch.unwrap();
    MissionDispatchIdentity {
        parent: dispatch.parent,
        child_run_id: dispatch.child_run_id,
        completion: dispatch.completion.unwrap(),
    }
}

/// Tier A assembly of the durable curator campaign. The local-model version
/// drives the same transitions through Qwen3.8; this test keeps routing,
/// persistence, retry, restart, and duplicate suppression deterministic on
/// every pull request.
#[test]
fn full_curator_campaign_preserves_origin_completion_and_retry_state() {
    let harness = TestHarness::new("curator-orchestration-hermetic");
    let store = harness.store();
    let project = "campaign";
    let model = "ollama/qwen3.8:27b-mlx";
    let mut confirms = HashMap::new();
    let mut events = Vec::new();

    admin_call(
        store,
        &mut confirms,
        "project_new",
        json!({"slug": project, "name": "Curator integration", "plugin": "echo-plugin"}),
    );
    for (slug, role) in [
        ("curator", AgentRole::Curator),
        ("artifact-tester", AgentRole::Tester),
        ("quiet-researcher", AgentRole::Researcher),
    ] {
        create_agent(store, &mut confirms, project, slug, role, model);
    }
    store
        .set_project_pins(
            project,
            BTreeMap::from([("target".to_string(), "fixture-rev-1".to_string())]),
        )
        .unwrap();

    admin_call(
        store,
        &mut confirms,
        "mission_new",
        json!({
            "project": project,
            "slug": "curator-campaign",
            "agent": "curator",
            "name": "Curator campaign",
            "brief": "Dispatch research and synthesize exact completion notifications.",
            "pins": {"target": "fixture-rev-1"}
        }),
    );
    let parent = MissionRunRef {
        project: project.into(),
        mission: "curator-campaign".into(),
        run_id: "corpus-campaign-curator-g1".into(),
    };
    let mut parent_record = store.load_mission(project, &parent.mission).unwrap();
    parent_record.session = Some(parent.run_id.clone());
    parent_record.control = Some(MissionControl {
        run_id: parent.run_id.clone(),
        port: 41_001,
    });
    parent_record.opencode_session = Some("ses_curator_campaign".into());
    store
        .update_mission(project, &parent.mission, &parent_record)
        .unwrap();

    let mut curator = Ctx::for_test(
        Plugin::spawn(&echo_plugin()).unwrap(),
        store.clone(),
        Scope::new(project),
        AgentRole::Curator,
    );
    curator.run_origin = Ok(Some(parent.clone()));
    for (slug, agent) in [
        ("artifact-child", "artifact-tester"),
        ("quiet-child", "quiet-researcher"),
    ] {
        tools::dispatch(
            &mut curator,
            "mission_new",
            &json!({"slug": slug, "agent": agent, "brief": format!("execute {slug}")}),
        )
        .unwrap();
        tools::dispatch(&mut curator, "mission_launch", &json!({"mission": slug})).unwrap();
        let requested = store.load_mission(project, slug).unwrap();
        assert_exact_parent(&requested, &parent);
        assert_eq!(requested.pins["target"], "fixture-rev-1");

        store
            .consume_mission_launch_request(project, slug, true)
            .unwrap();
        let mut launched = store.load_mission(project, slug).unwrap();
        let run_id = format!("corpus-{slug}-g1");
        launched.session = Some(run_id.clone());
        let dispatch = launched.dispatch.as_mut().unwrap();
        dispatch.child_run_id = Some(run_id);
        dispatch.live_seen = true;
        dispatch.running_seen = true;
        store.update_mission(project, slug, &launched).unwrap();
        assert_exact_parent(&launched, &parent);
        events.push(json!({"event": "child_live", "mission": slug}));
    }

    // The tester writes through its real scoped MCP surface. Its exact
    // artifact path is carried by terminal evidence, never inferred from model
    // prose or from another concurrent child's view of the project.
    let mut tester = Ctx::for_test(
        Plugin::spawn(&echo_plugin()).unwrap(),
        store.clone(),
        Scope::new(project),
        AgentRole::Tester,
    );
    tester.run_log = Some("artifact-child.raw".into());
    let finding = tools::dispatch(
        &mut tester,
        "finding_write",
        &json!({
            "title": "assembled curator finding",
            "severity": "high",
            "detail": "hermetic integration evidence",
            "path": "campaign/assembled.md"
        }),
    )
    .unwrap();
    assert!(finding.contains("findings/campaign/assembled.md"));

    assert!(store
        .record_mission_dispatch_completion(
            project,
            "artifact-child",
            MissionCompletion::Completed {
                at: 100,
                artifacts: vec!["findings/campaign/assembled.md".into()],
            },
        )
        .unwrap());
    assert!(store
        .record_mission_dispatch_completion(
            project,
            "quiet-child",
            MissionCompletion::Completed {
                at: 101,
                artifacts: Vec::new(),
            },
        )
        .unwrap());
    assert!(!store
        .record_mission_dispatch_completion(
            project,
            "quiet-child",
            MissionCompletion::UnexpectedExit { at: 102 },
        )
        .unwrap());

    // Both children are admitted as one parent-scoped continuation. Exact
    // identity proofs reject cross-parent and stale-message acknowledgements.
    let artifact_identity = child_identity(store, project, "artifact-child");
    let quiet_identity = child_identity(store, project, "quiet-child");
    let grouped_message = "msg_curator_group_1";
    for (slug, identity) in [
        ("artifact-child", &artifact_identity),
        ("quiet-child", &quiet_identity),
    ] {
        assert!(store
            .admit_mission_dispatch_delivery(project, slug, identity, 1, grouped_message)
            .unwrap());
    }
    let wrong_parent = MissionDispatchIdentity {
        parent: MissionRunRef {
            project: "other-project".into(),
            mission: "other-curator".into(),
            run_id: "other-run".into(),
        },
        ..artifact_identity.clone()
    };
    assert!(!store
        .acknowledge_mission_dispatch_delivery(
            project,
            "artifact-child",
            &wrong_parent,
            grouped_message,
        )
        .unwrap());

    // A failed curator turn remains durable and retryable with a new attempt.
    assert!(store
        .retry_mission_dispatch_delivery(
            project,
            "artifact-child",
            &artifact_identity,
            grouped_message,
        )
        .unwrap());
    assert!(store
        .admit_mission_dispatch_delivery(
            project,
            "artifact-child",
            &artifact_identity,
            2,
            "msg_curator_retry_2",
        )
        .unwrap());

    // Simulate restart after durable completion but before acknowledgement.
    let restarted = Store::new(store.root().to_path_buf()).with_actor("integration:restart");
    assert!(restarted
        .acknowledge_mission_dispatch_delivery(
            project,
            "artifact-child",
            &artifact_identity,
            "msg_curator_retry_2",
        )
        .unwrap());
    assert!(restarted
        .acknowledge_mission_dispatch_delivery(
            project,
            "quiet-child",
            &quiet_identity,
            grouped_message,
        )
        .unwrap());
    assert!(!restarted
        .acknowledge_mission_dispatch_delivery(
            project,
            "quiet-child",
            &quiet_identity,
            grouped_message,
        )
        .unwrap());

    // Failure is a terminal notification too, and the curator may use the
    // received result to create a fresh exact-origin follow-up.
    tools::dispatch(
        &mut curator,
        "mission_new",
        &json!({"slug": "failed-child", "agent": "quiet-researcher", "brief": "will fail"}),
    )
    .unwrap();
    tools::dispatch(
        &mut curator,
        "mission_launch",
        &json!({"mission": "failed-child"}),
    )
    .unwrap();
    store
        .consume_mission_launch_request(project, "failed-child", true)
        .unwrap();
    assert!(store
        .record_mission_dispatch_completion(
            project,
            "failed-child",
            MissionCompletion::LaunchFailed {
                at: 103,
                error: "fixture launch refusal".into(),
            },
        )
        .unwrap());

    tools::dispatch(
        &mut curator,
        "mission_new",
        &json!({
            "slug": "follow-up",
            "agent": "quiet-researcher",
            "brief": "follow up the assembled finding"
        }),
    )
    .unwrap();
    tools::dispatch(
        &mut curator,
        "mission_launch",
        &json!({"mission": "follow-up"}),
    )
    .unwrap();
    assert_exact_parent(&store.load_mission(project, "follow-up").unwrap(), &parent);

    let artifact_completion = store
        .load_mission(project, "artifact-child")
        .unwrap()
        .dispatch
        .unwrap()
        .completion
        .unwrap();
    assert!(matches!(
        artifact_completion,
        MissionCompletion::Completed { ref artifacts, .. }
            if artifacts == &["findings/campaign/assembled.md"]
    ));
    events.push(json!({
        "event": "campaign_verified",
        "parent": parent,
        "grouped_message": grouped_message
    }));
    harness.record_json("events.json", &events);
    harness.record_text(
        "scenario.yaml",
        include_str!("../scenarios/curator-orchestration.yaml"),
    );
}
