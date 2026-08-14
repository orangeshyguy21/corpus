//! Step-3 assertion: round-trip through the project corpus (teamless).
//!
//! Create a project (seeded with the core agent pair), write a technique +
//! finding via the MCP tools into the project corpus, wipe the corpus
//! (generation bumps, agents survive), and verify the run_log gate still
//! holds within the project scope.

use std::path::PathBuf;

use corpus_core::{Scope, Store};
use corpus_mcp::tools::{self, Ctx};
use serde_json::json;

fn echo_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus-core/tests/echo-plugin")
}

struct TestRig {
    ctx: Ctx,
    store: Store,
    root: PathBuf,
}

fn seed_core(store: &Store) {
    let dir = store.seed_agents_dir();
    for slug in ["operator", "researcher"] {
        let d = dir.join(slug);
        let _ = std::fs::create_dir_all(&d);
        std::fs::write(
            d.join("opencode.json"),
            format!(
                "{{\"$schema\":\"https://opencode.ai/config.json\",\"agent\":{{\"{slug}\":{{\"description\":\"{slug}\",\"mode\":\"primary\",\"prompt\":\"You are {slug}.\\n\"}}}}}}"
            ),
        )
        .unwrap();
    }
}

fn rig(tag: &str) -> TestRig {
    let root = std::env::temp_dir().join(format!("corpus-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.clone());
    seed_core(&store);
    store
        .create_project("proj", "Proj", "echo-plugin")
        .expect("create project");
    let ctx = Ctx {
        plugin: corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store: store.clone(),
        scope: Scope::new("proj"),
        faucet_spent_sats: 0,
        faucet_budget_sats: 1_000_000,
        probe_ready: true,
        probe_notes: String::new(),
        last_probe: std::time::Instant::now(),
    };
    TestRig { ctx, store, root }
}

fn proj_corpus(store: &Store) -> PathBuf {
    store.project_corpus_dir("proj")
}

#[test]
fn roundtrip_project_scoped_writes_and_wipe() {
    let rig = rig("roundtrip");
    let TestRig { mut ctx, store, root } = rig;

    // A run transcript the technique card must cite (in the project corpus).
    let runs = proj_corpus(&store).join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();

    // technique_save into the project corpus (gated on run_log existence).
    let out = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "quote-front-run",
            "status": "fired",
            "body": "body",
            "run_log": "1700000000-op-run.log"
        }),
    )
    .expect("technique_save");
    assert!(out.contains("technique card saved"), "{out}");

    // finding_write (echo oracle violates -> verified) into the project corpus.
    let out = tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({
            "title": "quote front-run theft",
            "severity": "high",
            "detail": "PoC detail"
        }),
    )
    .expect("finding_write");
    assert!(out.contains("sensitivity: embargoed"));
    assert!(out.contains("finding recorded"), "{out}");

    let findings_dir = proj_corpus(&store).join("findings");
    let finding_files: Vec<PathBuf> = std::fs::read_dir(&findings_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert_eq!(finding_files.len(), 1);

    // run_log gate: a nonexistent run_log is refused.
    let err = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "missing-log-card",
            "status": "analyzed-only",
            "body": "x",
            "run_log": "nope.log"
        }),
    )
    .expect_err("missing run_log must be refused");
    assert!(err.to_string().contains("run_log must name an existing file"));

    // Wipe the project corpus: generation bumps, corpus gone, agents survive.
    let p = store.wipe_project_corpus("proj").expect("wipe");
    assert_eq!(p.corpus_generation, 1);
    assert!(!proj_corpus(&store).join("techniques/quote-front-run.md").exists());
    assert!(store.project_agent_dir("proj", "operator").join("opencode.json").is_file());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn attack_save_project_scope() {
    let rig = rig("attack");
    let TestRig { mut ctx, store, root } = rig;

    let out = tools::dispatch(
        &mut ctx,
        "attack_save",
        &json!({"name": "quote-id-front-run", "description": "d", "script": "#!/bin/sh\necho pwn\n"}),
    )
    .expect("attack_save");
    assert!(out.contains("attack saved"), "{out}");

    let dest = proj_corpus(&store).join("attacks").join("quote-id-front-run");
    assert!(dest.join("attack.md").is_file());
    assert!(dest.join("run.sh").is_file());
    let attack_text = std::fs::read_to_string(dest.join("attack.md")).unwrap();
    assert!(attack_text.contains("sensitivity: internal"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn corpus_promote_is_unknown_tool() {
    let rig = rig("no-promote");
    let TestRig { mut ctx, root, .. } = rig;
    let err = tools::dispatch(
        &mut ctx,
        "corpus_promote",
        &json!({"team": "red", "category": "techniques", "entry": "x.md"}),
    )
    .expect_err("corpus_promote is no longer a tool");
    assert!(err.to_string().contains("unknown tool"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}