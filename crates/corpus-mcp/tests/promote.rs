//! Step-4 assertion: `corpus_promote` folds sensitivity into the gate.
//!
//! A technique (internal) promotes freely. A finding — which the write tools
//! default to `sensitivity: embargoed` — is refused without `confirm: true`
//! and succeeds with it, recording the source team scope in `promoted_from`.

use std::path::PathBuf;

use corpus_core::{core_agent_instances, Scope, Store};
use corpus_mcp::tools::{self, Ctx};
use serde_json::json;

fn echo_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus-core/tests/echo-plugin")
}

fn rig(tag: &str) -> (Ctx, Store, PathBuf) {
    let root = std::env::temp_dir().join(format!("corpus-promote-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.clone());
    store.create_project("proj", "Proj", "echo-plugin").unwrap();
    store
        .create_team("proj", "red", "Red team", core_agent_instances(), None)
        .unwrap();
    let ctx = Ctx {
        plugin: corpus_core::Plugin::spawn(&echo_plugin()).unwrap(),
        store: store.clone(),
        scope: Scope::new("proj", "red"),
        faucet_spent_sats: 0,
        faucet_budget_sats: 1_000_000,
        probe_ready: true,
        probe_notes: String::new(),
        last_probe: std::time::Instant::now(),
    };
    (ctx, store, root)
}

fn proj_corpus(store: &Store, category: &str, entry: &str) -> PathBuf {
    store.project_corpus_dir("proj").join(category).join(entry)
}

#[test]
fn promote_technique_succeeds_without_confirm() {
    let (mut ctx, store, root) = rig("technique");
    // Write a technique card into the team scope (internal by default).
    let runs = store.team_corpus_dir("proj", "red").join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();
    tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({"name": "quote-race", "status": "fired", "body": "the subtle mechanics body", "run_log": "1700000000-op-run.log"}),
    )
    .expect("write technique");

    let out = tools::dispatch(
        &mut ctx,
        "corpus_promote",
        &json!({"team": "red", "category": "techniques", "entry": "quote-race.md"}),
    )
    .expect("promote an internal technique needs no confirm");
    assert!(out.contains("sensitivity: internal"), "{out}");
    assert!(out.contains("from: proj/red@"), "{out} records team scope");

    let promoted = proj_corpus(&store, "techniques", "quote-race.md");
    let text = std::fs::read_to_string(&promoted).unwrap();
    assert!(text.contains("sensitivity: internal"));
    assert!(text.contains("promoted_from: proj/red@"));
    assert_eq!(text.matches("sensitivity:").count(), 1, "sensitivity not duplicated");
    assert!(text.contains("the subtle mechanics body"), "original body preserved");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn promote_finding_without_confirm_is_refused() {
    let (mut ctx, store, root) = rig("finding-refused");
    tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({"title": "quote front-run theft", "severity": "high", "detail": "PoC"}),
    )
    .expect("write finding (embargoed by default)");

    let finding = std::fs::read_dir(store.team_corpus_dir("proj", "red").join("findings"))
        .unwrap()
        .filter_map(|e| e.ok())
        .next()
        .unwrap()
        .path();
    let entry = finding.file_name().unwrap().to_string_lossy().to_string();

    let err = tools::dispatch(
        &mut ctx,
        "corpus_promote",
        &json!({"team": "red", "category": "findings", "entry": entry}),
    )
    .expect_err("embargoed finding must be refused without confirm");
    assert!(
        err.to_string().contains("refusing to promote embargoed"),
        "{err}"
    );
    assert!(
        !proj_corpus(&store, "findings", &entry).exists(),
        "nothing promoted"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn promote_finding_with_confirm_succeeds_and_records_provenance() {
    let (mut ctx, store, root) = rig("finding-confirmed");
    tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({"title": "quote front-run theft", "severity": "high", "detail": "PoC"}),
    )
    .expect("write finding");

    let finding = std::fs::read_dir(store.team_corpus_dir("proj", "red").join("findings"))
        .unwrap()
        .filter_map(|e| e.ok())
        .next()
        .unwrap()
        .path();
    let entry = finding.file_name().unwrap().to_string_lossy().to_string();

    let out = tools::dispatch(
        &mut ctx,
        "corpus_promote",
        &json!({"team": "red", "category": "findings", "entry": entry, "confirm": true}),
    )
    .expect("embargoed finding promotes with confirm");
    assert!(out.contains("sensitivity: embargoed"), "{out}");

    let promoted = proj_corpus(&store, "findings", &entry);
    let text = std::fs::read_to_string(&promoted).unwrap();
    assert!(text.contains("sensitivity: embargoed"), "class preserved");
    assert!(text.contains("promoted_from: proj/red@"), "provenance recorded");

    // The provenance matches the source team spec's `project/team@hash/generation`.
    let spec = corpus_core::TeamSpec::load(&store, "proj", "red").unwrap();
    let expected_prov = spec.provenance("proj", "red");
    assert!(
        text.contains(&format!("promoted_from: {expected_prov}")),
        "provenance is the team spec hash: {text}"
    );
    assert!(
        store.team_corpus_dir("proj", "red").join("findings").join(&entry).exists(),
        "source entry left in the team corpus"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn promote_attack_directory() {
    let (mut ctx, store, root) = rig("attack");
    tools::dispatch(
        &mut ctx,
        "attack_save",
        &json!({"name": "quote-id-front-run", "description": "d", "script": "#!/bin/sh\necho pwn\n"}),
    )
    .expect("attack_save (internal)");

    let out = tools::dispatch(
        &mut ctx,
        "corpus_promote",
        &json!({"team": "red", "category": "attacks", "entry": "quote-id-front-run"}),
    )
    .expect("promote an internal attack needs no confirm");
    assert!(out.contains("sensitivity: internal"), "{out}");

    let dest = proj_corpus(&store, "attacks", "quote-id-front-run");
    assert!(dest.join("attack.md").is_file());
    assert!(dest.join("run.sh").is_file());
    let attack_text = std::fs::read_to_string(dest.join("attack.md")).unwrap();
    assert!(attack_text.contains("promoted_from: proj/red@"));
    let _ = std::fs::remove_dir_all(&root);
}