//! The curator route: project-management tools served to an in-project
//! agent, scoped to the project the server already proved.
//!
//! The property under test throughout is that the project is NOT the
//! caller's to choose. The `--admin` profile resolves it from a tool
//! argument at 17 separate sites; this route overwrites that argument from
//! `CORPUS_PROJECT` before any of them run, so naming another project is
//! not refused so much as impossible.

use std::path::PathBuf;

use corpus_core::{AgentRole, Project, Scope, Store};
use corpus_mcp::tools::{self, Ctx};
use serde_json::{json, Value};

fn echo_plugin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus-core/tests/echo-plugin")
}

struct Rig {
    ctx: Ctx,
    store: Store,
    root: PathBuf,
}

/// Two projects, so "it used the scope" and "it used the argument" give
/// different answers. `alpha` is the scope; `beta` is what a caller might
/// try to name.
fn rig(tag: &str, role: AgentRole) -> Rig {
    let world = std::env::temp_dir().join(format!("corpus-curator-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&world);
    // Mirrors what `Ctx::from_env` does from the run identity: everything
    // this process changes carries the name of the agent that changed it.
    let store = Store::new(world.join("store")).with_actor("curator:keeper");
    for slug in ["alpha", "beta"] {
        store.create_project(slug, slug, "echo-plugin").expect("project");
    }
    store
        .create_agent_with_role("alpha", "keeper", role)
        .expect("agent");
    store
        .create_agent_with_role("beta", "untouched", AgentRole::Researcher)
        .expect("agent");
    let ctx = Ctx::for_test(
        corpus_core::Plugin::spawn(&echo_plugin()).expect("spawn echo plugin"),
        store.clone(),
        Scope::new("alpha"),
        role,
    );
    Rig { ctx, store, root: world }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn names(catalog: &Value) -> Vec<String> {
    catalog
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t.get("name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The advertised catalog and the routing table are built from separate
/// lists; a tool in one and not the other is either unroutable or invisible.
#[test]
fn the_admin_catalog_matches_its_routing_table() {
    let mut advertised = names(&corpus_mcp::admin::catalog());
    advertised.sort();
    let mut declared: Vec<String> = corpus_mcp::admin::ADMIN_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect();
    declared.sort();
    assert_eq!(advertised, declared);
}

/// The catalog a curator advertises is exactly its grant set — no project
/// CRUD (a scoped server cannot honestly serve a tool whose subject is
/// another project), no whole-project wipe, and none of the sandbox tools.
#[test]
fn a_curator_advertises_exactly_its_grant_set() {
    let catalog = tools::catalog_for(&Ok(AgentRole::Curator));
    let mut got = names(&catalog);
    got.sort();
    let mut want: Vec<String> = corpus_core::CURATOR_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect();
    want.sort();
    assert_eq!(got, want);

    for forbidden in [
        "project_new",
        "project_delete",
        "project_clone",
        "project_rebind",
        "agent_copy",
        "corpus_wipe",
        "sandbox_exec",
        "oracle_run",
        "faucet",
        "finding_write",
        "target_info",
    ] {
        assert!(
            !got.contains(&forbidden.to_string()),
            "a curator must not advertise {forbidden}"
        );
    }
}

/// Narrow roles receive one domain; Super receives their current-project
/// union without acquiring operator-only cross-project tools.
#[test]
fn super_merges_the_two_scoped_catalogs() {
    for role in [AgentRole::Researcher, AgentRole::Tester] {
        let got = names(&tools::catalog_for(&Ok(role)));
        for admin in corpus_core::CURATOR_TOOLS {
            assert!(
                !got.contains(&admin.to_string()),
                "{} must not advertise {admin}",
                role.as_str()
            );
        }
        assert!(!got.is_empty(), "{} still has its own tools", role.as_str());
    }
    let curator = names(&tools::catalog_for(&Ok(AgentRole::Curator)));
    for sandbox in corpus_core::CORPUS_TOOLS {
        assert!(!curator.contains(&sandbox.trim_start_matches("corpus_").to_string()));
    }
    let super_tools = names(&tools::catalog_for(&Ok(AgentRole::Super)));
    for sandbox in corpus_core::CORPUS_TOOLS {
        assert!(super_tools.contains(&sandbox.trim_start_matches("corpus_").to_string()));
    }
    for admin in corpus_core::SUPER_ADMIN_TOOLS {
        assert!(super_tools.contains(&admin.to_string()), "super lacks {admin}");
    }
    for operator_only in [
        "project_new",
        "project_clone",
        "project_rebind",
        "project_delete",
        "agent_copy",
    ] {
        assert!(!super_tools.contains(&operator_only.to_string()), "{operator_only}");
    }
}

/// A scoped server must not ask for something it will overwrite. Leaving
/// `project` in the schema would invite a model to supply one and then have
/// it silently replaced — worse than never asking.
#[test]
fn scoped_management_schemas_never_advertise_a_project() {
    for role in [AgentRole::Curator, AgentRole::Super] {
        let catalog = tools::catalog_for(&Ok(role));
        for tool in catalog.as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            if !role.admin_tools().contains(&name) {
                continue;
            }
            let schema = &tool["inputSchema"];
            assert!(
                schema["properties"].get("project").is_none(),
                "{role:?}/{name} advertises a project property"
            );
            if let Some(required) = schema["required"].as_array() {
                assert!(
                    !required.iter().any(|k| k.as_str() == Some("project")),
                    "{role:?}/{name} requires a project"
                );
            }
        }
    }
}

#[test]
fn scoped_finding_list_uses_the_proven_project_and_is_read_only() {
    let mut rig = rig("finding-list-scope", AgentRole::Curator);
    std::fs::write(
        rig.store
            .project_corpus_dir("alpha")
            .join("findings/1787091200-alpha.md"),
        "---\ntitle: Alpha finding\nseverity: high\ntimestamp: 1787091200\n---\n",
    )
    .unwrap();
    std::fs::write(
        rig.store
            .project_corpus_dir("beta")
            .join("findings/1787091300-beta.md"),
        "---\ntitle: Beta secret\nseverity: critical\ntimestamp: 1787091300\n---\n",
    )
    .unwrap();

    let out = tools::dispatch(
        &mut rig.ctx,
        "finding_list",
        &json!({"project": "beta"}),
    )
    .expect("scoped finding list");
    let value: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(value["project"], "alpha");
    assert_eq!(value["count"], 1);
    assert_eq!(value["findings"][0]["title"], "Alpha finding");
    assert!(!out.contains("Beta secret"));

    let audit = corpus_core::audit::tail(&rig.store, "alpha", 20).unwrap();
    assert!(
        audit.iter().all(|record| record.op != "finding_list"),
        "read-only finding_list must not create curator audit acts"
    );
}

/// The heart of it: a curator scoped to `alpha` that explicitly names
/// `beta` gets alpha's answer, and beta is never touched.
/// Author-time pin validation is wired into `mission_new`, but it FAILS
/// OPEN when the source set can't be enumerated (the echo rig has no
/// `[sources]`): pinning a mission must still succeed, and the pin must
/// land on the record. This guards the wiring — a regression that made
/// `validate_pin` propagate the enumeration error would reject every
/// pinned mission here.
#[test]
fn a_pinned_mission_authors_cleanly_when_sources_are_unknown() {
    let mut rig = rig("pin", AgentRole::Curator);
    tools::dispatch(
        &mut rig.ctx,
        "mission_new",
        &json!({
            "slug": "m1", "agent": "keeper", "brief": "b",
            "pins": { "cdk": "8716e53de0472e5224d6a74866a680f7bc7b4513" },
        }),
    )
    .expect("a pinned mission authors cleanly (validation fails open)");
    let m = rig.store.load_mission("alpha", "m1").unwrap();
    assert_eq!(
        m.pins.get("cdk").map(String::as_str),
        Some("8716e53de0472e5224d6a74866a680f7bc7b4513"),
        "the pin is recorded verbatim"
    );
}

#[test]
fn a_curator_cannot_reach_another_project() {
    let mut rig = rig("cross", AgentRole::Curator);

    let listed = tools::dispatch(&mut rig.ctx, "agent_list", &json!({ "project": "beta" }))
        .expect("agent_list answers");
    assert!(
        listed.contains("keeper"),
        "the SCOPE decides, not the argument: {listed}"
    );
    assert!(
        !listed.contains("untouched"),
        "beta's agents must not be reachable: {listed}"
    );

    // And a mutation aimed at beta lands in alpha or not at all — never in
    // beta.
    let _ = tools::dispatch(
        &mut rig.ctx,
        "agent_new",
        &json!({
            "project": "beta",
            "agent": "planted",
            "description": "d",
            "prompt": "p",
        }),
    );
    assert!(
        !rig.store.project_agent_dir("beta", "planted").exists(),
        "nothing may be created in another project"
    );
    assert!(
        rig.store.project_agent_dir("alpha", "planted").exists(),
        "the write landed in the scoped project"
    );
    assert!(
        rig.store.load_agent("beta", "untouched").is_ok(),
        "beta's own agent survives"
    );
}

/// The curator writes corpus entries through `entry_write`, not raw file
/// tools: a relative path lands in the scoped project's corpus, and the
/// resolver's guards travel with it — an absolute path and a write into
/// `runs/` are both refused, so no spelling reaches outside the corpus.
#[test]
fn a_curator_writes_corpus_entries_by_relative_path() {
    let mut rig = rig("write", AgentRole::Curator);

    let out = tools::dispatch(
        &mut rig.ctx,
        "entry_write",
        &json!({
            "project": "beta",
            "path": "techniques/plan.md",
            "content": "# team plan\n\nrecon -> hunt -> validate\n",
        }),
    )
    .expect("entry_write lands");
    assert!(out.contains("techniques/plan.md"), "names the entry: {out}");

    // The SCOPE decides where it landed, never the argument: alpha, not the
    // beta the caller named.
    let written = rig
        .store
        .project_corpus_dir("alpha")
        .join("techniques/plan.md");
    assert!(written.exists(), "the write landed in the scoped corpus");
    assert!(
        !rig.store.project_corpus_dir("beta").join("techniques/plan.md").exists(),
        "nothing may be written into another project's corpus"
    );
    assert!(
        std::fs::read_to_string(&written).unwrap().contains("team plan"),
        "the content is the body we passed"
    );

    // Re-writing the same path replaces it in place — this is the "rewrite
    // an entry" case the curator does most.
    tools::dispatch(
        &mut rig.ctx,
        "entry_write",
        &json!({ "project": "alpha", "path": "techniques/plan.md", "content": "v2\n" }),
    )
    .expect("a rewrite lands");
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "v2\n");

    // An absolute path is not a corpus-relative one: refused, not resolved.
    assert!(
        tools::dispatch(
            &mut rig.ctx,
            "entry_write",
            &json!({ "project": "alpha", "path": "/etc/passwd", "content": "x" }),
        )
        .is_err(),
        "an absolute path is refused"
    );

    // runs/ holds transcripts the operator audits — never writable.
    assert!(
        tools::dispatch(
            &mut rig.ctx,
            "entry_write",
            &json!({ "project": "alpha", "path": "runs/forged.raw", "content": "x" }),
        )
        .is_err(),
        "runs/ is not writable"
    );
}

/// `mission_launch` flags the mission record for the app to spawn — the
/// MCP process cannot start a run itself. The flag lands on the scoped
/// project's mission, the brief is untouched, and a second call is a no-op
/// (the request is already pending).
#[test]
fn a_curator_requests_a_launch_by_flagging_the_record() {
    let mut rig = rig("launch", AgentRole::Curator);

    tools::dispatch(
        &mut rig.ctx,
        "mission_new",
        &json!({ "slug": "m1", "agent": "keeper", "brief": "probe the mint" }),
    )
    .expect("mission");

    // Before launch: no request pending.
    assert!(
        rig.store.load_mission("alpha", "m1").unwrap().launch_requested.is_none(),
        "a fresh mission carries no launch request"
    );

    let out = tools::dispatch(
        &mut rig.ctx,
        "mission_launch",
        &json!({ "project": "beta", "mission": "m1" }),
    )
    .expect("launch requested");
    assert!(out.contains("launch requested"), "{out}");

    // The flag landed on the SCOPE (alpha), not the beta the caller named,
    // and the brief survived the record rewrite.
    let m = rig.store.load_mission("alpha", "m1").unwrap();
    assert!(m.launch_requested.is_some(), "the launch is now requested");
    assert_eq!(
        rig.store.mission_brief("alpha", "m1").unwrap().trim(),
        "probe the mint",
        "the brief is preserved — it is the kickoff prompt"
    );

    // Idempotent: asking again does not stack or error.
    let first = m.launch_requested;
    tools::dispatch(&mut rig.ctx, "mission_launch", &json!({ "mission": "m1" }))
        .expect("a second request is fine");
    assert_eq!(
        rig.store.load_mission("alpha", "m1").unwrap().launch_requested,
        first,
        "a pending request is left as it is"
    );
}

/// `mission_status` reports the live run state. A mission with no session
/// reads as `idle`; the scope is injected (callable with only the mission
/// arg), and an all-missions poll lists each by slug.
#[test]
fn a_curator_polls_live_mission_status() {
    let mut rig = rig("status", AgentRole::Curator);
    tools::dispatch(
        &mut rig.ctx,
        "mission_new",
        &json!({ "slug": "m1", "agent": "keeper", "brief": "b" }),
    )
    .expect("mission");

    // Single mission, scope injected (no project key): a never-launched
    // mission has no session, so it is idle.
    let one = tools::dispatch(&mut rig.ctx, "mission_status", &json!({ "mission": "m1" }))
        .expect("status answers");
    assert!(one.contains("m1"), "names the mission: {one}");
    assert!(one.contains("idle"), "no session reads as idle: {one}");
    assert!(!one.contains("running"), "nothing is running: {one}");

    // All missions (no mission arg): still lists m1.
    let all = tools::dispatch(&mut rig.ctx, "mission_status", &json!({})).expect("status all");
    assert!(all.contains("m1"), "the all-poll lists the mission: {all}");
}

/// A research role reaching for a management tool is told it is a
/// permissions problem, not a typo — otherwise a model hunts for a spelling
/// that does not exist.
#[test]
fn narrow_research_roles_are_refused_management_tools_by_role() {
    for role in [AgentRole::Researcher, AgentRole::Tester] {
        let mut rig = rig(&format!("deny-{}", role.as_str()), role);
        let error = tools::dispatch(&mut rig.ctx, "agent_new", &json!({}))
            .expect_err("must refuse")
            .to_string();
        assert!(error.contains(role.as_str()), "{error}");
        assert!(
            !error.contains("unknown"),
            "a refusal must not read as a typo: {error}"
        );
        assert!(
            !error.contains("(allowed: )"),
            "the empty-grants message must be gone: {error}"
        );
    }
}

/// A curator reaching for a sandbox tool gets a message that names what it
/// DOES hold, rather than an empty parenthesis.
#[test]
fn a_curator_refused_a_sandbox_tool_is_told_what_it_holds() {
    let mut rig = rig("no-sandbox", AgentRole::Curator);
    let error = tools::dispatch(&mut rig.ctx, "sandbox_exec", &json!({ "command": "id" }))
        .expect_err("must refuse")
        .to_string();
    assert!(error.contains("curator"), "{error}");
    assert!(error.contains("project-management"), "{error}");
    assert!(!error.contains("(allowed: )"), "{error}");
}

/// A curator's work is store-side, so a red probe — a dead mint, an absent
/// plugin — must not stop it from repairing the project. The route runs
/// ahead of the probe gate for exactly this case.
#[test]
fn a_curator_works_while_the_probe_is_red() {
    let mut rig = rig("probe-red", AgentRole::Curator);
    rig.ctx.probe_ready = false;
    rig.ctx.probe_notes = "mints down".to_string();
    let listed = tools::dispatch(&mut rig.ctx, "agent_list", &json!({}))
        .expect("management tools survive an unhealthy arena");
    assert!(listed.contains("keeper"), "{listed}");
}

/// No proven scope, no management tools — the same fail-closed rule the
/// write tools follow.
#[test]
fn an_unresolved_scope_refuses_every_management_tool() {
    let mut rig = rig("no-scope", AgentRole::Curator);
    rig.ctx.scope = Err("CORPUS_PROJECT is unset — every launch path sets it".to_string());
    for tool in ["agent_list", "agent_new", "mission_list", "corpus_stats"] {
        let error = tools::dispatch(&mut rig.ctx, tool, &json!({}))
            .expect_err("must refuse")
            .to_string();
        assert!(error.contains("CORPUS_PROJECT"), "{tool}: {error}");
    }
}

/// The whole case for this role is that its acts are visible afterwards.
/// Every mutation leaves an intent line and an outcome line; reads leave
/// none, so the log stays a record of acts rather than of curiosity.
#[test]
fn every_mutation_is_recorded_and_reads_are_not() {
    let mut rig = rig("audit", AgentRole::Curator);

    for tool in ["agent_list", "mission_list", "corpus_stats"] {
        tools::dispatch(&mut rig.ctx, tool, &json!({})).expect("reads work");
    }
    assert!(
        corpus_core::audit::tail(&rig.store, "alpha", 100).unwrap().is_empty(),
        "looking is not an act"
    );

    tools::dispatch(
        &mut rig.ctx,
        "agent_new",
        &json!({ "agent": "built", "description": "d", "prompt": "p" }),
    )
    .expect("create");
    tools::dispatch(
        &mut rig.ctx,
        "agent_set_role",
        &json!({ "agent": "built", "role": "tester" }),
    )
    .expect("set role");

    let log = corpus_core::audit::tail(&rig.store, "alpha", 100).unwrap();
    assert_eq!(log.len(), 4, "an intent and an outcome each: {log:?}");
    for record in &log {
        assert!(
            record.actor.starts_with("curator:"),
            "every line names its actor: {record:?}"
        );
    }
    assert_eq!(log[0].op, "agent_new");
    assert_eq!(log[0].outcome, corpus_core::audit::Outcome::Intent);
    assert_eq!(log[0].target, "agents/built");
    assert_eq!(log[1].outcome, corpus_core::audit::Outcome::Ok);
    assert_eq!(log[2].op, "agent_set_role");
    assert!(log[2].detail.contains("tester"), "{:?}", log[2]);

    // A within-catalog policy refusal is recorded too — an attempt is an act.
    let _ = tools::dispatch(
        &mut rig.ctx,
        "agent_set_role",
        &json!({ "agent": "built", "role": "super" }),
    );
    let log = corpus_core::audit::tail(&rig.store, "alpha", 100).unwrap();
    assert_eq!(
        log.last().unwrap().outcome,
        corpus_core::audit::Outcome::Refused,
        "{:?}",
        log.last()
    );

    // And the agent it built carries the same provenance in its sidecar,
    // so an operator can see who made it without reading the log.
    let built = rig.store.load_agent("alpha", "built").unwrap();
    assert!(
        built.meta.modified_by.as_deref().unwrap_or("").starts_with("curator:"),
        "{:?}",
        built.meta
    );
}

fn confirm_token(dry_run: &str) -> String {
    dry_run
        .split("confirm_token: ")
        .nth(1)
        .expect("dry run contains a token")
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// Curators need scoped cleanup authority. Each delete stays inside the
/// injected project, produces an audited dry-run, and requires the server's
/// one-shot token before mutation.
#[test]
fn a_curator_can_confirm_scoped_deletions() {
    let mut rig = rig("curator-delete", AgentRole::Curator);
    rig.store
        .create_agent_with_role("alpha", "victim", AgentRole::Researcher)
        .expect("victim agent");
    tools::dispatch(
        &mut rig.ctx,
        "mission_new",
        &json!({ "slug": "m1", "agent": "victim", "brief": "b" }),
    )
    .expect("mission");
    let finding = rig.store.project_corpus_dir("alpha").join("findings/f1.md");
    std::fs::write(&finding, "evidence\n").unwrap();

    let dry = tools::dispatch(&mut rig.ctx, "mission_delete", &json!({ "mission": "m1" }))
        .expect("mission dry run");
    tools::dispatch(
        &mut rig.ctx,
        "mission_delete",
        &json!({ "mission": "m1", "confirm_token": confirm_token(&dry) }),
    )
    .expect("confirmed mission delete");

    let dry = tools::dispatch(&mut rig.ctx, "agent_delete", &json!({ "agent": "victim" }))
        .expect("agent dry run");
    tools::dispatch(
        &mut rig.ctx,
        "agent_delete",
        &json!({ "agent": "victim", "confirm_token": confirm_token(&dry) }),
    )
    .expect("confirmed agent delete");

    let dry = tools::dispatch(
        &mut rig.ctx,
        "entry_delete",
        &json!({ "path": "findings/f1.md" }),
    )
    .expect("entry dry run");
    tools::dispatch(
        &mut rig.ctx,
        "entry_delete",
        &json!({ "path": "findings/f1.md", "confirm_token": confirm_token(&dry) }),
    )
    .expect("confirmed entry delete");

    assert!(rig.store.load_mission("alpha", "m1").is_err());
    assert!(rig.store.load_agent("alpha", "victim").is_err());
    assert!(!finding.exists());
}

#[test]
fn super_can_manage_and_wipe_only_its_scoped_project() {
    let mut rig = rig("super-union", AgentRole::Super);
    tools::dispatch(
        &mut rig.ctx,
        "agent_new",
        &json!({
            "agent": "another-super",
            "description": "d",
            "prompt": "p",
            "role": "super",
        }),
    )
    .expect("Super may author any project role");
    assert_eq!(
        rig.store.load_agent("alpha", "another-super").unwrap().meta.role(),
        AgentRole::Super
    );

    let finding = rig.store.project_corpus_dir("alpha").join("findings/f1.md");
    std::fs::write(&finding, "evidence\n").unwrap();
    let before = Project::load(&rig.store, "alpha").unwrap().corpus_generation;
    let dry = tools::dispatch(&mut rig.ctx, "corpus_wipe", &json!({})).expect("wipe dry run");
    tools::dispatch(
        &mut rig.ctx,
        "corpus_wipe",
        &json!({ "confirm_token": confirm_token(&dry) }),
    )
    .expect("confirmed scoped wipe");
    assert!(!finding.exists());
    assert_eq!(
        Project::load(&rig.store, "alpha").unwrap().corpus_generation,
        before + 1
    );

    for (tool, args) in [
        ("project_delete", json!({ "slug": "beta" })),
        (
            "agent_copy",
            json!({
                "from_project": "alpha", "from": "keeper",
                "to_project": "beta", "to": "escaped"
            }),
        ),
    ] {
        let error = tools::dispatch(&mut rig.ctx, tool, &args)
            .expect_err("cross-project/lifecycle administration stays operator-only")
            .to_string();
        assert!(error.contains("does not"), "{tool}: {error}");
    }
    assert!(Project::load(&rig.store, "beta").is_ok());
    assert!(rig.store.load_agent("beta", "escaped").is_err());
}

#[test]
fn a_curator_cannot_grant_or_repurpose_super() {
    let mut rig = rig("super-ceiling", AgentRole::Curator);
    rig.store
        .create_agent_with_role("alpha", "root", AgentRole::Super)
        .expect("operator-created super fixture");

    let attempts = [
        (
            "agent_new",
            json!({
                "agent": "new-super", "description": "d", "prompt": "p", "role": "super"
            }),
        ),
        (
            "agent_new",
            json!({
                "agent": "implicit-super", "description": "d", "prompt": "p", "from": "root"
            }),
        ),
        ("agent_clone", json!({ "from": "root", "to": "root-copy" })),
        ("agent_set_role", json!({ "agent": "keeper", "role": "super" })),
        (
            "agent_subagent_add",
            json!({
                "agent": "keeper", "name": "wide", "description": "d", "prompt": "p", "role": "super"
            }),
        ),
        ("agent_set_role", json!({ "agent": "root", "role": "researcher" })),
    ];
    for (tool, args) in attempts {
        let error = tools::dispatch(&mut rig.ctx, tool, &args)
            .expect_err("super authority is operator-owned")
            .to_string();
        assert!(error.contains("operator-owned"), "{tool}: {error}");
    }

    // Copying configuration is allowed when the curator explicitly chooses a
    // narrow role; the inherited document cannot decide the capability ceiling.
    tools::dispatch(
        &mut rig.ctx,
        "agent_new",
        &json!({
            "agent": "narrow-copy",
            "description": "d",
            "prompt": "p",
            "from": "root",
            "role": "researcher",
        }),
    )
    .expect("explicitly narrowed copy");
    assert_eq!(
        rig.store.load_agent("alpha", "narrow-copy").unwrap().meta.role(),
        AgentRole::Researcher
    );
}

/// If an act cannot be recorded, it does not happen. An unwritable log
/// costs the role its powers, not its accountability.
#[test]
fn an_unrecordable_act_is_refused() {
    let mut rig = rig("audit-blocked", AgentRole::Curator);
    // Occupy the audit directory's path with a file, so the append cannot
    // create it.
    let audit_dir = rig.store.var_dir().join("audit");
    std::fs::create_dir_all(audit_dir.parent().unwrap()).unwrap();
    std::fs::write(&audit_dir, "not a directory\n").unwrap();

    let error = tools::dispatch(
        &mut rig.ctx,
        "agent_new",
        &json!({ "agent": "unrecorded", "description": "d", "prompt": "p" }),
    )
    .expect_err("must refuse")
    .to_string();
    assert!(error.contains("cannot be recorded"), "{error}");
    assert!(
        !rig.store.project_agent_dir("alpha", "unrecorded").exists(),
        "the act must not have happened"
    );

    // Reads still work — they were never going to be recorded.
    tools::dispatch(&mut rig.ctx, "agent_list", &json!({})).expect("reads are unaffected");
}

/// An edit that leaves the agent set open under delegation makes the NEXT
/// launch refuse to render the whole project. The curator learns about it
/// on the call that caused it, not a day later.
#[test]
fn a_dangling_delegation_is_reported_on_the_call_that_caused_it() {
    let mut rig = rig("delegation", AgentRole::Curator);
    // A primary that delegates to a scout, and the scout it names.
    let doc = json!({
        "$schema": "https://opencode.ai/config.json",
        "agent": {
            "keeper": {
                "mode": "primary",
                "permission": { "task": { "*": "deny", "keeper-scout": "allow" } },
            },
            "keeper-scout": { "mode": "subagent", "description": "d" },
        }
    });
    let saved = tools::dispatch(
        &mut rig.ctx,
        "agent_save",
        &json!({ "agent": "keeper", "document": doc }),
    )
    .expect("a closed set saves cleanly");
    assert!(!saved.contains("[warning]"), "{saved}");

    // Now drop the scout entry but keep the rule that names it.
    let broken = json!({
        "$schema": "https://opencode.ai/config.json",
        "agent": {
            "keeper": {
                "mode": "primary",
                "permission": { "task": { "*": "deny", "keeper-scout": "allow" } },
            },
        }
    });
    let out = tools::dispatch(
        &mut rig.ctx,
        "agent_save",
        &json!({ "agent": "keeper", "document": broken }),
    )
    .expect("the write still lands");
    assert!(out.contains("[warning]"), "{out}");
    assert!(out.contains("keeper-scout"), "{out}");
    assert!(out.contains("next launch"), "{out}");
}
