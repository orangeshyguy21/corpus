//! Step-3 assertion: round-trip through scoped corpora.
//!
//! Create a project, create a team from the core templates, write a
//! technique + finding via the MCP tools into the team scope, clone the
//! team, wipe the original's corpus — the generation counter increments and
//! the clone is untouched. Gated-write rules (technique run_log must cite an
//! existing runs/ file) still hold, now within the team scope.

use std::path::PathBuf;

use corpus_core::{core_agent_instances, Scope, Store};
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

fn rig(tag: &str) -> TestRig {
    let root = std::env::temp_dir().join(format!("corpus-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.clone());
    store
        .create_project("proj", "Proj", "echo-plugin")
        .expect("create project");
    store
        .create_team(
            "proj",
            "red",
            "Red team",
            core_agent_instances(),
            None,
            None,
        )
        .expect("create team from core templates");
    let ctx = Ctx {
        plugin: corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store: store.clone(),
        scope: Scope::new("proj", "red"),
        faucet_spent_sats: 0,
        faucet_budget_sats: 1_000_000,
        probe_ready: true,
        probe_notes: String::new(),
        last_probe: std::time::Instant::now(),
    };
    TestRig { ctx, store, root }
}

/// An EMPTY store (no projects at all) with an arbitrary default scope —
/// for probing the write_scope gate against teams that do not exist.
fn bare_rig(tag: &str, scope: Scope) -> (Ctx, Store, PathBuf) {
    let root = std::env::temp_dir().join(format!("corpus-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.clone());
    let ctx = Ctx {
        plugin: corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store: store.clone(),
        scope,
        faucet_spent_sats: 0,
        faucet_budget_sats: 1_000_000,
        probe_ready: true,
        probe_notes: String::new(),
        last_probe: std::time::Instant::now(),
    };
    (ctx, store, root)
}

fn team_corpus(store: &Store, team: &str) -> PathBuf {
    store.team_corpus_dir("proj", team)
}

#[test]
fn roundtrip_scoped_writes_clone_and_wipe() {
    let rig = rig("roundtrip");
    let TestRig { mut ctx, store, root } = rig;

    // A run transcript the technique card must cite (in the team scope).
    let runs = team_corpus(&store, "red").join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();

    // technique_save into the team scope (gated on run_log existence).
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
    assert!(out.contains("teams/red/corpus/techniques/quote-front-run.md"), "wrote into team scope: {out}");

    // finding_write (echo oracle violates -> verified) into the team scope.
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
    assert!(out.contains("teams/red/corpus/findings/"), "finding in team scope: {out}");
    assert!(out.contains("sensitivity: embargoed"));

    let findings_dir = team_corpus(&store, "red").join("findings");
    let finding_files: Vec<PathBuf> = std::fs::read_dir(&findings_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    assert_eq!(finding_files.len(), 1);

    // run_log gate: a nonexistent run_log is refused, within the team scope.
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

    // run_log fallback: a migrated log living in the PROJECT corpus stays
    // resolvable (flat-store migration backward compat).
    let proj_runs = store.project_corpus_dir("proj").join("runs");
    std::fs::create_dir_all(&proj_runs).unwrap();
    std::fs::write(proj_runs.join("1600000000-old.log"), "# old\n").unwrap();
    let out = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "old-log-card",
            "status": "analyzed-only",
            "body": "x",
            "run_log": "1600000000-old.log"
        }),
    )
    .expect("run_log fallback resolves migrated logs");
    assert!(out.contains("teams/red/corpus/techniques/old-log-card.md"));

    // Clone the team (deep copy incl corpus).
    store.clone_team("proj", "red", "blue").expect("clone team");
    assert!(team_corpus(&store, "blue").join("findings").join(
        finding_files[0].file_name().unwrap()
    ).is_file(), "clone has a copy of the finding");

    // Wipe the original: generation bumps, corpus gone, clone untouched.
    let red_after = store.wipe_team_corpus("proj", "red").expect("wipe");
    assert_eq!(red_after.corpus_generation, 1, "wipe increments generation");
    assert!(!team_corpus(&store, "red").join("techniques/quote-front-run.md").exists());
    assert!(team_corpus(&store, "blue").join("findings").join(
        finding_files[0].file_name().unwrap()
    ).is_file(), "clone corpus untouched by the wipe");
    let blue = corpus_core::TeamSpec::load(&store, "proj", "blue").expect("load blue");
    assert_eq!(blue.corpus_generation, 0, "clone generation is a snapshot");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn write_to_unknown_team_fails_loud() {
    let rig = rig("writescope-ghost");
    let TestRig { mut ctx, store: _, root } = rig;
    // A nonexistent team must be refused, not silently auto-created.
    let err = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "ghost-card",
            "status": "fired",
            "body": "x",
            "run_log": "nope.log",
            "team": "ghost"
        }),
    )
    .expect_err("unknown team must be refused");
    assert!(err.to_string().contains("team not found: proj/ghost"), "{err}");
    assert!(err.to_string().contains("corpus team new"), "{err} points at the fix");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn backward_compat_default_scope_works_without_team_spec() {
    // A server configured with the default default/default scope (no migrate
    // run yet, i.e. no team.yaml) must still accept unscoped writes — that
    // scope IS the backward-compat target the migration creates.
    let (mut ctx, store, root) = bare_rig("writescope-default", Scope::new("default", "default"));
    let out = tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({"title": "scoped by default", "severity": "low", "detail": "d"}),
    )
    .expect("default/default unscoped write is allowed");
    assert!(out.contains("teams/default/corpus/findings/"), "{out}");
    assert!(
        store
            .team_dir("default", "default")
            .join("corpus/findings")
            .is_dir(),
        "default scope gets its corpus dir"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn unscoped_write_uses_scope_without_team_argument() {
    let rig = rig("unscoped");
    let TestRig { mut ctx, store, root } = rig;
    let runs = team_corpus(&store, "red").join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();
    let out = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "no-team-card",
            "status": "fired",
            "body": "x",
            "run_log": "1700000000-op-run.log"
        }),
    )
    .expect("unscoped technique_save");
    // No `team` arg: lands in the configured scope (proj/red).
    assert!(out.contains("teams/red/corpus/techniques/no-team-card.md"));
    // An explicit `team` arg overrides the scope on the same server.
    let out = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "scoped-card",
            "status": "fired",
            "body": "x",
            "run_log": "1700000000-op-run.log",
            "team": "red"
        }),
    )
    .expect("explicit team technique_save");
    assert!(out.contains("teams/red/corpus/techniques/scoped-card.md"));
    let _ = std::fs::remove_dir_all(&root);
}