//! Chunk-1 admin profile: the corpus-admin MCP tool group, thin over
//! corpus-core. Covers the confirm-token gate on every destructive op
//! (dry-run without token, one-shot token completes), the agent validator
//! round-trip, and rebind plugin validation against the registry.

use std::path::PathBuf;

use corpus_core::{Project, Scope, Store};
use corpus_mcp::admin;
use corpus_mcp::tools::Ctx;
use serde_json::json;

fn echo_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus-core/tests/echo-plugin")
}

/// Admin rig: project "proj" + one ops mission + an echo-plugin binding in
/// the registry (CORPUS_PLUGINS_DIR points at the echo plugin) so rebind
/// validation sees a real plugin name.
fn rig(tag: &str) -> (Ctx, Store, PathBuf, String) {
    let root = std::env::temp_dir().join(format!("corpus-admin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    // Point plugin discovery at the echo plugin so project_rebind validates.
    std::env::set_var("CORPUS_PLUGINS_DIR", echo_plugin().parent().unwrap());
    let store = Store::new(root.clone());
    store.create_project("proj", "Proj", "echo-plugin").expect("create project");
    store
        .create_agent_with_role("proj", "appsec", corpus_core::AgentRole::Researcher)
        .expect("agent");
    // The admin profile has no agent identity — the role is irrelevant to
    // it by design (see the orthogonality test below).
    let ctx = Ctx::for_test(
        corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store.clone(),
        Scope::new("proj"),
        corpus_core::AgentRole::Super,
    );
    (ctx, store, root, "proj".to_string())
}

fn proj_corpus(store: &Store) -> PathBuf {
    store.project_corpus_dir("proj")
}

#[test]
fn finding_list_is_structured_recursive_and_keeps_unrated_entries_visible() {
    let (mut ctx, store, root, project) = rig("finding-list");
    let findings = proj_corpus(&store).join("findings");
    std::fs::create_dir_all(findings.join("campaigns/august")).unwrap();
    std::fs::write(
        findings.join("1787091200-high.md"),
        "---\ntitle: High finding\nseverity: high\ntimestamp: 1787091200\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        findings.join("campaigns/august/plain-note.md"),
        "# Unrated nested lead\n",
    )
    .unwrap();

    let out = admin::dispatch(
        &mut ctx,
        "finding_list",
        &json!({"project": project, "sort": "severity"}),
    )
    .expect("finding list");
    let value: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["project"], "proj");
    assert_eq!(value["count"], 2);
    assert_eq!(value["findings"][0]["severity"], "high");
    assert_eq!(value["findings"][1]["unrated"], true);
    assert_eq!(
        value["findings"][1]["path"],
        "findings/campaigns/august/plain-note.md"
    );
    assert!(value["findings"][1]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning == "missing-severity"));

    let filtered = admin::dispatch(
        &mut ctx,
        "finding_list",
        &json!({
            "project": "proj",
            "severity": ["high"],
            "include_unrated": false,
            "text": "high",
            "limit": 1
        }),
    )
    .unwrap();
    let filtered: serde_json::Value = serde_json::from_str(&filtered).unwrap();
    assert_eq!(filtered["count"], 1);
    assert_eq!(filtered["findings"][0]["title"], "High finding");

    let error = admin::dispatch(
        &mut ctx,
        "finding_list",
        &json!({"project": "proj", "severity": "urgent"}),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("invalid finding severity"), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}

// --- project rebind validates against discovery ---

#[test]
fn rebind_rejects_unknown_plugin() {
    let (mut ctx, _store, root, project) = rig("rebind-unknown");
    // A hallucinated plugin (chunk-0 finding) is refused — no dangling binding.
    let err = admin::dispatch(&mut ctx, "project_rebind", &json!({"slug": project, "plugin": "gdk-regtest"}))
        .expect_err("gdk-regtest not in registry");
    assert!(err.to_string().contains("unknown plugin"), "{err}");

    // The real plugin name validates and lands.
    let out = admin::dispatch(&mut ctx, "project_rebind", &json!({"slug": project, "plugin": "echo-plugin"}))
        .expect("rebind echo-plugin");
    assert!(out.contains("rebound project"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

// --- agent_save runs the core validator ---

#[test]
fn agent_save_refuses_invalid_document() {
    let (mut ctx, _store, root, project) = rig("agent-save");
    // One primary but an invalid permission action -> the core validator
    // rejects it; nothing is written.
    let bad = json!({
        "$schema": "https://opencode.ai/config.json",
        "agent": {
            "appsec": {
                "description": "d",
                "mode": "primary",
                "prompt": "hunt\n",
                "permission": {"bash": "always"}
            }
        }
    });
    let err = admin::dispatch(
        &mut ctx,
        "agent_save",
        &json!({"project": project, "agent": "appsec", "document": bad}),
    )
    .expect_err("agent with only a primary-labeled agent map but no prompt must be invalid");
    assert!(err.to_string().contains("error"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn agent_save_valid_roundtrip() {
    let (mut ctx, _store, root, project) = rig("agent-save-ok");
    let doc = json!({
        "$schema": "https://opencode.ai/config.json",
        "agent": {
            "appsec": {
                "description": "payment replay hunter",
                "mode": "primary",
                "prompt": "You hunt payment replay bugs.\n"
            }
        }
    });
    let out = admin::dispatch(
        &mut ctx,
        "agent_save",
        &json!({"project": project, "agent": "appsec", "document": doc}),
    )
    .expect("valid agent saves");
    assert!(out.contains("validator passed"), "{out}");
    let _ = std::fs::remove_dir_all(&root);
}

// --- the confirm-token gate ---

#[test]
fn corpus_wipe_without_token_is_dry_run_and_requires_confirmation() {
    let (mut ctx, store, root, project) = rig("wipe-gate");
    // Seed a finding so the dry-run summary reports real content.
    let runs = proj_corpus(&store).join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();

    // Without a token: dry-run, no mutation (generation stays 0), token minted.
    let out = admin::dispatch(&mut ctx, "corpus_wipe", &json!({"project": project}))
        .expect("dry-run wipe");
    assert!(out.contains("DRY RUN"), "{out}");
    assert!(out.contains("confirm_token:"), "{out}");

    // Generation must still be 0 — no mutation happened.
    let p = Project::load(&store, &project).expect("project");
    assert_eq!(p.corpus_generation, 0);
    assert!(proj_corpus(&store).join("runs/1700000000-op-run.log").is_file());

    // Re-call with the token commits the wipe.
    let token = out.split("confirm_token: ").nth(1).expect("token").split_whitespace().next().unwrap().to_string();
    let out = admin::dispatch(
        &mut ctx,
        "corpus_wipe",
        &json!({"project": project, "confirm_token": token}),
    )
    .expect("confirmed wipe");
    assert!(out.contains("wiped project corpus"), "{out}");
    let p = Project::load(&store, &project).expect("project");
    assert_eq!(p.corpus_generation, 1);
    assert!(!proj_corpus(&store).join("runs/1700000000-op-run.log").exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn entry_delete_is_dry_run_first_and_bound_to_current_state() {
    let (mut ctx, store, root, project) = rig("entry-delete-gate");
    let finding = proj_corpus(&store).join("findings/f1.md");
    std::fs::write(&finding, "first\n").unwrap();

    let dry = admin::dispatch(
        &mut ctx,
        "entry_delete",
        &json!({"project": project, "path": "findings/f1.md"}),
    )
    .expect("dry run");
    assert!(dry.contains("DRY RUN"), "{dry}");
    assert!(dry.contains("confirm_token:"), "{dry}");
    assert!(finding.is_file(), "dry run must not delete");

    let token = dry
        .split("confirm_token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    // Change the target after approval. The old token must no longer match.
    std::fs::write(&finding, "changed after preview\n").unwrap();
    let error = admin::dispatch(
        &mut ctx,
        "entry_delete",
        &json!({
            "project": project,
            "path": "findings/f1.md",
            "confirm_token": token,
        }),
    )
    .expect_err("stale preview token must be refused")
    .to_string();
    assert!(error.contains("does not match"), "{error}");
    assert!(finding.is_file());

    let dry = admin::dispatch(
        &mut ctx,
        "entry_delete",
        &json!({"project": project, "path": "findings/f1.md"}),
    )
    .expect("fresh dry run");
    let token = dry
        .split("confirm_token: ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let out = admin::dispatch(
        &mut ctx,
        "entry_delete",
        &json!({
            "project": project,
            "path": "findings/f1.md",
            "confirm_token": token,
        }),
    )
    .expect("confirmed deletion");
    assert!(out.contains("[confirmed with one-shot token]"), "{out}");
    assert!(!finding.exists());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn entry_delete_directory_requires_recursive_before_minting() {
    let (mut ctx, store, root, project) = rig("entry-delete-recursive");
    let attack = proj_corpus(&store).join("attacks/replay");
    std::fs::create_dir_all(&attack).unwrap();
    std::fs::write(attack.join("attack.md"), "attack\n").unwrap();
    let error = admin::dispatch(
        &mut ctx,
        "entry_delete",
        &json!({"project": project, "path": "attacks/replay"}),
    )
    .expect_err("directory needs recursive")
    .to_string();
    assert!(error.contains("pass recursive"), "{error}");
    assert!(!error.contains("confirm_token"), "must not mint unusable token: {error}");
    assert!(attack.is_dir());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn wrong_token_is_refused() {
    let (mut ctx, store, root, project) = rig("wipe-wrong-token");
    let runs = proj_corpus(&store).join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();

    // Mint a valid token from the dry-run...
    let out = admin::dispatch(&mut ctx, "corpus_wipe", &json!({"project": project})).expect("dry-run");
    // ...but call the mutation with a DIFFERENT, never-minted token.
    let _minted = out;
    let err = admin::dispatch(
        &mut ctx,
        "corpus_wipe",
        &json!({"project": project, "confirm_token": "deadbeef"}),
    )
    .expect_err("a token that was never minted must be refused");
    assert!(err.to_string().contains("invalid or expired"), "{err}");
    assert_ne!(
        Project::load(&store, &project).expect("project").corpus_generation,
        1,
        "no mutation on a wrong token"
    );
    assert!(proj_corpus(&store).join("runs/1700000000-op-run.log").is_file());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn host_admin_tools_are_absent_from_the_research_catalog() {
    // The research artifact advertises no host-global admin tools.
    let names: Vec<String> = corpus_mcp::tools::catalog()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    for admin_tool in [
        "project_list", "project_delete", "project_rebind",
        "agent_save", "mission_new", "mission_set_budget",
        "corpus_wipe", "corpus_stats", "finding_list",
    ] {
        assert!(!names.contains(&admin_tool.to_string()), "sandbox profile must not carry {admin_tool}");
    }
}

#[test]
fn confirm_token_is_single_use_and_op_scoped() {
    let (mut ctx, store, root, project) = rig("wipe-once");
    let runs = proj_corpus(&store).join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-op-run.log"), "# run\n").unwrap();

    let out = admin::dispatch(&mut ctx, "corpus_wipe", &json!({"project": project})).expect("dry-run");
    let token = out.split("confirm_token: ").nth(1).unwrap().split_whitespace().next().unwrap().to_string();
    admin::dispatch(&mut ctx, "corpus_wipe", &json!({"project": project, "confirm_token": token})).expect("wipe");

    // Replay of the SAME token must fail (single-use).
    let err = admin::dispatch(
        &mut ctx,
        "corpus_wipe",
        &json!({"project": project, "confirm_token": token}),
    )
    .expect_err("replay must be refused");
    assert!(err.to_string().contains("invalid or expired"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn admin_catalog_carries_no_sandbox_tools() {
    let cat = admin::catalog();
    let names: Vec<String> = cat
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
        .collect();
    for bad in ["sandbox_exec", "oracle_list", "oracle_run", "faucet", "finding_write", "agent_save_of_missions", "target_info"] {
        assert!(!names.contains(&bad.to_string()), "admin catalog must not carry {bad}");
    }
    for op in [
        "project_delete",
        "agent_delete",
        "mission_delete",
        "corpus_wipe",
        "entry_delete",
    ] {
        assert!(names.contains(&op.to_string()), "must carry destructive op {op}");
    }
    // The model discovery tool (the chat agent resolves exact model ids
    // through this instead of guessing).
    assert!(names.contains(&"model_list".to_string()), "must carry model_list");
}

#[test]
fn every_destructive_tool_advertises_confirmation() {
    let catalog = admin::catalog();
    for op in admin::DESTRUCTIVE_OPS {
        let tool = catalog
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == op)
            .unwrap_or_else(|| panic!("destructive op {op} missing from catalog"));
        assert!(
            tool["inputSchema"]["properties"]["confirm_token"].is_object(),
            "{op} is destructive but advertises no confirm_token"
        );
        assert!(
            tool["description"]
                .as_str()
                .is_some_and(|description| description.contains("CONFIRM-GATED")),
            "{op} does not tell the caller it is confirm-gated"
        );
    }
}
