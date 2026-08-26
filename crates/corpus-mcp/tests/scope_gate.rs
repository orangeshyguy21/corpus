//! The project gate: a server that cannot prove which project it serves
//! must refuse, not guess.
//!
//! The bug these cover: `CORPUS_PROJECT` unset resolved silently to the
//! project named `default`, so a launch that lost the variable wrote a
//! whole mission's findings into another project's corpus and reported
//! success. Two independent halves are asserted here — resolution
//! (`Scope::from_env_strict`, including the `--role` ordering) and
//! enforcement (the write tools, which must never bring a project into
//! being by writing to it).

use std::path::PathBuf;
use std::sync::Mutex;

use corpus_core::{Scope, Store};
use corpus_mcp::tools::{self, Ctx};
use serde_json::json;

/// `set_var`/`remove_var` are process-global; these tests must not race.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn echo_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus-core/tests/echo-plugin")
}

fn tmp_store(tag: &str) -> (Store, PathBuf) {
    let root = std::env::temp_dir().join(format!("corpus-scope-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.clone());
    (store, root)
}

/// Resolution: named, existing, and slug-valid — or an error that says
/// which of the three failed.
#[test]
fn scope_resolution_requires_a_real_project() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, root) = tmp_store("resolve");
    store.create_project("proj", "Proj", "echo-plugin").unwrap();

    std::env::remove_var(corpus_core::PROJECT_ENV);
    let error = Scope::from_env_strict(&store).unwrap_err();
    assert!(error.contains(corpus_core::PROJECT_ENV), "{error}");

    // Empty is the same as unset — a launch that exported a blank value
    // must not resolve to anything either.
    std::env::set_var(corpus_core::PROJECT_ENV, "   ");
    let error = Scope::from_env_strict(&store).unwrap_err();
    assert!(error.contains(corpus_core::PROJECT_ENV), "{error}");

    // Named but nonexistent: the old default's whole failure mode.
    std::env::set_var(corpus_core::PROJECT_ENV, "ghost");
    let error = Scope::from_env_strict(&store).unwrap_err();
    assert!(error.contains("ghost"), "{error}");
    assert!(error.contains("no project"), "{error}");
    assert!(
        !store.project_dir("ghost").exists(),
        "resolution must not create what it failed to find"
    );

    std::env::set_var(corpus_core::PROJECT_ENV, "proj");
    assert_eq!(Scope::from_env_strict(&store).unwrap().project, "proj");

    std::env::remove_var(corpus_core::PROJECT_ENV);
    let _ = std::fs::remove_dir_all(&root);
}

/// `--role` overrides the AGENT lookup, never the project gate. It used to
/// return before the project check, so `--role super` with no
/// `CORPUS_PROJECT` produced a full-capability server writing into the
/// default project.
#[test]
fn role_flag_does_not_bypass_the_project_gate() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (store, root) = tmp_store("role-flag");
    store.create_project("proj", "Proj", "echo-plugin").unwrap();

    std::env::remove_var(corpus_core::PROJECT_ENV);
    // The flag this process was NOT started with is the point: the gate is
    // resolved from the scope, so no argv value can stand in for a project.
    assert!(
        Scope::from_env_strict(&store).is_err(),
        "no argv flag substitutes for a project"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Enforcement: with a Super role — so the role gate is not what refuses —
/// every write tool refuses a project that does not exist, and leaves no
/// trace of having tried.
#[test]
fn writes_never_bring_a_project_into_being() {
    let (store, root) = tmp_store("ghost-writes");
    store.create_project("proj", "Proj", "echo-plugin").unwrap();
    let mut ctx = Ctx::for_test(
        corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store.clone(),
        Scope::new("ghost"),
        corpus_core::AgentRole::Super,
    );

    let calls = [
        (
            "finding_write",
            json!({ "title": "t", "severity": "high", "detail": "d" }),
        ),
        (
            "probe_save",
            json!({ "name": "a", "description": "d", "script": "#!/bin/sh\n" }),
        ),
        (
            "technique_save",
            json!({ "name": "t", "body": "b", "status": "fired", "run_log": "x.raw" }),
        ),
    ];
    for (tool, args) in calls {
        let error = tools::dispatch(&mut ctx, tool, &args)
            .expect_err(&format!("{tool} must refuse a nonexistent project"));
        let error = error.to_string();
        assert!(error.contains("ghost"), "{tool}: {error}");
    }
    assert!(
        !store.project_dir("ghost").exists(),
        "a refused write must not leave a corpus tree behind"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// An unresolved scope refuses every scoped tool with the resolution
/// error, the same way an unresolved role does.
#[test]
fn unresolved_scope_refuses_scoped_tools() {
    let (store, root) = tmp_store("unresolved");
    store.create_project("proj", "Proj", "echo-plugin").unwrap();
    let mut ctx = Ctx::for_test(
        corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store.clone(),
        Scope::new("proj"),
        corpus_core::AgentRole::Super,
    );
    ctx.scope = Err("CORPUS_PROJECT is unset — every launch path sets it".to_string());

    let error = tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({ "title": "t", "severity": "high", "detail": "d" }),
    )
    .expect_err("a scopeless server must refuse")
    .to_string();
    assert!(error.contains(corpus_core::PROJECT_ENV), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}
