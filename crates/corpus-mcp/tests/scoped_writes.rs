//! Step-3 assertion: round-trip through the project corpus (teamless).
//!
//! Create a project, add its agents explicitly, write a technique +
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

fn rig(tag: &str) -> TestRig {
    let root = std::env::temp_dir().join(format!("corpus-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.clone());
    store
        .create_project("proj", "Proj", "echo-plugin")
        .expect("create project");
    // Projects no longer arrive with agents — they are created from a role.
    for (slug, role) in [
        ("operator", corpus_core::AgentRole::Tester),
        ("researcher", corpus_core::AgentRole::Researcher),
    ] {
        store
            .create_agent_with_role("proj", slug, role)
            .expect("create agent");
    }
    // Super: these tests exercise the WRITE tools, so the role gate must
    // not be what refuses them. Role-gating itself is tested separately.
    let ctx = Ctx::for_test(
        corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store.clone(),
        Scope::new("proj"),
        corpus_core::AgentRole::Super,
    );
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
    let finding_raw = std::fs::read_to_string(&finding_files[0]).unwrap();
    assert!(
        finding_raw.contains("oracle_verified: true"),
        "{finding_raw}"
    );
    assert!(finding_raw.contains("echo oracle log"), "{finding_raw}");

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

    // run_log defaults to CORPUS_RUN_LOG (ctx.run_log) when omitted.
    ctx.run_log = Some("1700000000-op-run.log".to_string());
    let out = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "default-log-card",
            "status": "analyzed-only",
            "body": "omitted run_log -> defaults to ctx.run_log"
        }),
    )
    .expect("technique_save with default run_log");
    assert!(out.contains("technique card saved"), "{out}");
    let card = std::fs::read_to_string(
        proj_corpus(&store).join("techniques/default-log-card.md"),
    )
    .unwrap();
    assert!(card.contains("run_log: 1700000000-op-run.log"), "{card}");

    // run_log omitted AND no ctx.run_log -> helpful error.
    ctx.run_log = None;
    let err = tools::dispatch(
        &mut ctx,
        "technique_save",
        &json!({
            "name": "no-log-card",
            "status": "analyzed-only",
            "body": "x"
        }),
    )
    .expect_err("no run_log and no CORPUS_RUN_LOG must be refused");
    assert!(err.to_string().contains("run_log not provided"));

    // Wipe the project corpus: generation bumps, corpus gone, agents survive.
    let p = store.wipe_project_corpus("proj").expect("wipe");
    assert_eq!(p.corpus_generation, 1);
    assert!(!proj_corpus(&store).join("techniques/quote-front-run.md").exists());
    assert!(store.project_agent_dir("proj", "operator").join("opencode.json").is_file());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn finding_write_validates_before_persistence_and_preserves_project_agency() {
    let rig = rig("finding-writer");
    let TestRig { mut ctx, store, root } = rig;
    ctx.run_log = Some("1787091000-operator.raw".to_string());
    ctx.source_pins = Some(serde_json::Map::from_iter([(
        "cdk".to_string(),
        json!("0123456789abcdef"),
    )]));

    let error = tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({"title": "bad", "severity": "urgent", "detail": "x"}),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("invalid finding severity"), "{error}");
    assert_eq!(
        std::fs::read_dir(proj_corpus(&store).join("findings"))
            .unwrap()
            .count(),
        0
    );

    let out = tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({
            "title": "Header\nseverity: low",
            "severity": "critical",
            "detail": "demonstrated PoC",
            "path": "campaigns/august/header-injection.md",
            "metadata": {
                "id": "CDK-REG-900",
                "component": "mint: api",
                "cwes": ["CWE-20", "CWE-284"]
            }
        }),
    )
    .expect("finding_write nested path");
    assert!(
        out.contains("findings/campaigns/august/header-injection.md"),
        "{out}"
    );
    assert!(out.contains("reference: CDK-REG-900"), "{out}");

    let path = proj_corpus(&store).join("findings/campaigns/august/header-injection.md");
    let raw = std::fs::read_to_string(&path).unwrap();
    let (frontmatter, _) = corpus_core::frontmatter::split(&raw).unwrap();
    let frontmatter = frontmatter.unwrap();
    assert_eq!(
        corpus_core::frontmatter::get_str(&frontmatter, "title").as_deref(),
        Some("Header\nseverity: low")
    );
    assert_eq!(
        corpus_core::frontmatter::get_str(&frontmatter, "severity").as_deref(),
        Some("critical")
    );
    assert_eq!(
        corpus_core::frontmatter::get_str(&frontmatter, "run_log").as_deref(),
        Some("1787091000-operator.raw")
    );
    assert_eq!(
        corpus_core::frontmatter::get_str(&frontmatter, "actor").as_deref(),
        Some("operator")
    );

    for args in [
        json!({
            "title": "duplicate",
            "severity": "high",
            "detail": "x",
            "path": "campaigns/august/header-injection.md"
        }),
        json!({
            "title": "traversal",
            "severity": "high",
            "detail": "x",
            "path": "../runs/stolen.md"
        }),
        json!({
            "title": "reserved",
            "severity": "high",
            "detail": "x",
            "metadata": {"severity": "low"}
        }),
    ] {
        assert!(tools::dispatch(&mut ctx, "finding_write", &args).is_err());
    }
    assert_eq!(
        corpus_core::frontmatter::get_str(
            &corpus_core::frontmatter::split(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .0
                .unwrap(),
            "severity"
        )
        .as_deref(),
        Some("critical"),
        "a refused write must not replace the existing finding"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn attack_save_project_scope() {
    let rig = rig("attack");
    let TestRig {
        mut ctx,
        store,
        root,
    } = rig;

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

/// target_info must forward the mission's resolved source pins to the
/// plugin: a mission pinned off the plugin's default rev must see the
/// mounts the sandbox actually gets (the launch pins), not the plugin's
/// config defaults. Regression: target_info dropped the pins, so a
/// pinned mission was told it read code it wasn't reading.
#[test]
fn target_info_reports_mission_pins_not_defaults() {
    let rig = rig("pins");
    let TestRig { mut ctx, root, .. } = rig;
    let mut pins = serde_json::Map::new();
    pins.insert(
        "cdk".to_string(),
        serde_json::Value::String("cccccccccccccccccccccccccccccccccccccccc".to_string()),
    );
    ctx.source_pins = Some(pins);

    let out = tools::dispatch(&mut ctx, "target_info", &json!({})).expect("target_info");
    assert!(
        out.contains("cccccccccccccccccccccccccccccccccccccccc"),
        "mission pin must appear in the reported sources: {out}"
    );
    assert!(
        !out.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "the plugin default pin must NOT be reported for a pinned mission: {out}"
    );

    // No pins -> the plugin's defaults, unchanged.
    ctx.source_pins = None;
    let out = tools::dispatch(&mut ctx, "target_info", &json!({})).expect("target_info no pins");
    assert!(
        out.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "unpinned run reports the plugin default: {out}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The namespace split that cost a run. The pinned trees exist in two
/// places — `sources/<name>/<sha>` in the run cwd, and `/opt/src/<name>`
/// inside the container — and each is reachable only by its own tool.
/// `target_info` used to report the SANDBOX path to every role and assert
/// "you have no host filesystem", which is false for all of them and
/// useless to a researcher, whose two tools do not include `sandbox_exec`.
/// One agent believed it, spent a run being denied by opencode for reading
/// a path absent from the machine, and concluded the harness was lying to
/// it rather than that it held the wrong path. It was right.
#[test]
fn source_paths_are_reported_per_role_not_per_sandbox() {
    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let TestRig { mut ctx, root, .. } = rig("source-paths");

    // A researcher cannot enter the sandbox, so the mount is not a path it
    // can act on and naming it is the whole bug.
    ctx.role = Ok(corpus_core::AgentRole::Researcher);
    let out = tools::dispatch(&mut ctx, "target_info", &json!({})).expect("target_info");
    assert!(
        out.contains(&format!("sources/cdk/{SHA}")),
        "a researcher must be told the path it can actually read: {out}"
    );
    assert!(
        !out.contains("/opt/src"),
        "the sandbox mount is unreachable for this role and must not be named: {out}"
    );
    assert!(
        !out.contains("no host filesystem"),
        "its working directory IS a host directory: {out}"
    );

    // A tester or super holds `sandbox_exec`, so BOTH are true — and each
    // is labelled with the tool it belongs to rather than left to be
    // guessed at.
    for role in [corpus_core::AgentRole::Tester, corpus_core::AgentRole::Super] {
        ctx.role = Ok(role);
        let out = tools::dispatch(&mut ctx, "target_info", &json!({})).expect("target_info");
        assert!(
            out.contains(&format!("sources/cdk/{SHA}")),
            "{role:?} reads on the host too: {out}"
        );
        assert!(
            out.contains("path_inside_sandbox_exec") && out.contains("/opt/src/cdk"),
            "{role:?} can enter the sandbox, where the mount is the right path: {out}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// THE role gate: a researcher is refused the execution and publication
/// tools by the SERVER, whatever any permission block says — the server
/// never reads that block. This is the property the whole role system
/// exists to provide.
#[test]
fn researcher_role_is_refused_execution_and_publication_tools() {
    let TestRig { mut ctx, root, .. } = rig("role-researcher");
    ctx.role = Ok(corpus_core::AgentRole::Researcher);

    for (tool, args) in [
        ("sandbox_exec", json!({"command": "echo hi"})),
        ("oracle_list", json!({})),
        ("oracle_run", json!({"name": "double-spend"})),
        ("faucet", json!({"op": "balance"})),
        (
            "wallet_fund",
            json!({"work_dir": "/tmp/w", "amount_sat": 10, "idempotency_key": "fund-1"}),
        ),
        ("attack_save", json!({"name": "a", "description": "d", "script": "s"})),
        (
            "finding_write",
            json!({"title": "t", "severity": "high", "detail": "d"}),
        ),
    ] {
        let err = tools::dispatch(&mut ctx, tool, &args)
            .expect_err("a researcher must be refused {tool}");
        let msg = err.to_string();
        assert!(msg.contains("researcher"), "{tool}: {msg}");
        assert!(msg.contains(tool), "{tool}: {msg}");
    }

    // ...and still gets the two tools its role does grant.
    tools::dispatch(&mut ctx, "target_info", &json!({}))
        .expect("a researcher reads its target");
    let _ = std::fs::remove_dir_all(&root);
}

/// An unresolved identity denies EVERYTHING (fail closed): a gate that is
/// bypassed by unsetting an environment variable is not a gate.
#[test]
fn unresolved_role_denies_every_tool() {
    let TestRig { mut ctx, root, .. } = rig("role-unresolved");
    ctx.role = Err("CORPUS_OPENCODE_AGENT is unset".to_string());
    for tool in ["target_info", "technique_save", "sandbox_exec", "finding_write"] {
        let err = tools::dispatch(&mut ctx, tool, &json!({}))
            .expect_err("an unresolved role denies everything");
        assert!(err.to_string().contains("no resolved agent role"), "{tool}: {err}");
    }
    // And it advertises nothing, so the agent isn't invited to try.
    let catalog = tools::catalog_for(&ctx.role);
    assert_eq!(catalog.as_array().map(|a| a.len()), Some(0), "{catalog}");
    let _ = std::fs::remove_dir_all(&root);
}

/// The advertised catalog matches what the role can actually call, so a
/// low-trust agent never sees attack-relevant tool descriptions.
#[test]
fn advertised_catalog_matches_the_role() {
    let names = |role| -> Vec<String> {
        tools::catalog_for(&Ok(role))
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect()
    };
    let researcher = names(corpus_core::AgentRole::Researcher);
    assert!(researcher.contains(&"target_info".to_string()), "{researcher:?}");
    assert!(researcher.contains(&"technique_save".to_string()), "{researcher:?}");
    for hidden in [
        "sandbox_exec",
        "oracle_list",
        "oracle_run",
        "faucet",
        "finding_write",
        "attack_save",
    ] {
        assert!(
            !researcher.contains(&hidden.to_string()),
            "a researcher must not be shown {hidden}: {researcher:?}"
        );
    }
    let sup = names(corpus_core::AgentRole::Super);
    assert_eq!(
        sup.len(),
        corpus_core::CORPUS_TOOLS.len() + corpus_core::SUPER_ADMIN_TOOLS.len(),
        "{sup:?}"
    );
    for tool in corpus_core::SUPER_ADMIN_TOOLS {
        assert!(sup.contains(&tool.to_string()), "super lacks {tool}: {sup:?}");
    }
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
