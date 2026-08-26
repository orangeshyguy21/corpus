use super::*;

/// One role is enforced per session, taken from the primary — so a
/// curator and a research subagent cannot share one. Refused at set
/// time, where the operator can see it, rather than silently collapsed
/// at render time.
#[test]
fn a_curator_cannot_hold_a_research_subagent() {
    let store = tmp_store("mixed-subs");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "cur", AgentRole::Curator)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "cur",
            &doc(serde_json::json!({
                "cur": { "mode": "primary" },
                "cur-helper": { "mode": "subagent", "description": "d" },
            })),
        )
        .unwrap();
    let error = store
        .set_subagent_role("alpha", "cur", "cur-helper", AgentRole::Tester)
        .unwrap_err()
        .to_string();
    assert!(error.contains("curator"), "{error}");
    assert!(error.contains("tester"), "{error}");
    let error = store
        .add_subagent(&AddSubagentRequest {
            project: "alpha".into(),
            agent: "cur".into(),
            name: "bad-helper".into(),
            description: "incompatible".into(),
            prompt: String::new(),
            model: None,
            role: Some(AgentRole::Tester),
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("cannot hold a tester"), "{error}");
    assert!(
        store.load_agent("alpha", "cur").unwrap().doc["agent"]
            .get("bad-helper")
            .is_none(),
        "role compatibility must be checked before the document is saved"
    );
    // A curator subagent under a curator is fine.
    store
        .set_subagent_role("alpha", "cur", "cur-helper", AgentRole::Curator)
        .unwrap();

    // And the mirror: a research primary refuses a curator subagent.
    store
        .create_agent_with_role("alpha", "res", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "res",
            &doc(serde_json::json!({
                "res": { "mode": "primary" },
                "res-scout": { "mode": "subagent", "description": "d" },
            })),
        )
        .unwrap();
    assert!(store
        .set_subagent_role("alpha", "res", "res-scout", AgentRole::Curator)
        .is_err());

    // Super is the cross-domain union and may host any project role.
    store
        .create_agent_with_role("alpha", "sup", AgentRole::Super)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "sup",
            &doc(serde_json::json!({
                "sup": { "mode": "primary" },
                "sup-curator": { "mode": "subagent", "description": "d" },
            })),
        )
        .unwrap();
    store
        .set_subagent_role("alpha", "sup", "sup-curator", AgentRole::Curator)
        .unwrap();
}

/// A field edit touches ONE key and leaves the rest of the document
/// byte-identical — the whole point of the granular tools.
#[test]
fn set_agent_field_is_surgical() {
    let store = tmp_store("field");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "a",
            &doc(serde_json::json!({
                "a": {
                    "mode": "primary",
                    "description": "before",
                    "prompt": "keep me",
                    "permission": { "bash": "deny" },
                },
                "a-scout": { "mode": "subagent", "description": "scout" },
            })),
        )
        .unwrap();
    store
        .set_agent_field(
            "alpha",
            "a",
            None,
            "model",
            "maple/deepseek-v4-flash".into(),
        )
        .unwrap();
    store
        .set_agent_field("alpha", "a", None, "description", "after".into())
        .unwrap();
    let after = store.load_agent("alpha", "a").unwrap();
    let primary = &after.doc["agent"]["a"];
    assert_eq!(primary["model"].as_str(), Some("maple/deepseek-v4-flash"));
    assert_eq!(primary["description"].as_str(), Some("after"));
    // Untouched neighbours survive verbatim.
    assert_eq!(primary["prompt"].as_str(), Some("keep me"));
    assert_eq!(primary["permission"]["bash"].as_str(), Some("deny"));
    assert!(
        after.doc["agent"]["a-scout"].is_object(),
        "subagent untouched"
    );

    // A subagent can be targeted by name.
    store
        .set_agent_field("alpha", "a", Some("a-scout"), "model", "ollama/x".into())
        .unwrap();
    let after = store.load_agent("alpha", "a").unwrap();
    assert_eq!(
        after.doc["agent"]["a-scout"]["model"].as_str(),
        Some("ollama/x")
    );

    // Null removes; structural keys are refused.
    store
        .set_agent_field("alpha", "a", None, "model", serde_json::Value::Null)
        .unwrap();
    assert!(store.load_agent("alpha", "a").unwrap().doc["agent"]["a"]
        .get("model")
        .is_none());
    let err = store
        .set_agent_field("alpha", "a", None, "mode", "subagent".into())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not settable"), "{err}");
    // An unknown entry is refused rather than silently created.
    let err = store
        .set_agent_field("alpha", "a", Some("ghost"), "model", "x".into())
        .unwrap_err()
        .to_string();
    assert!(err.contains("no entry named"), "{err}");
}

/// A permission patch merges — a caller changing one rule must not
/// drop the others by omission.
#[test]
fn permission_patch_merges_and_removes() {
    let store = tmp_store("perm-patch");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "a",
            &doc(serde_json::json!({
                "a": {
                    "mode": "primary",
                    "permission": { "bash": "deny", "webfetch": "allow" },
                },
            })),
        )
        .unwrap();
    store
        .patch_agent_permission(
            "alpha",
            "a",
            None,
            &serde_json::json!({ "websearch": "allow", "webfetch": null }),
        )
        .unwrap();
    let perm = &store.load_agent("alpha", "a").unwrap().doc["agent"]["a"]["permission"];
    assert_eq!(
        perm["bash"].as_str(),
        Some("deny"),
        "untouched key survives"
    );
    assert_eq!(perm["websearch"].as_str(), Some("allow"), "added");
    assert!(perm.get("webfetch").is_none(), "null removes");
}

/// Adding a subagent also wires the primary's `task:` allow; removing
/// it takes the rule and the sidecar role back out.
#[test]
fn subagent_add_and_remove_keep_delegation_consistent() {
    let store = tmp_store("subagent");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "a",
            &doc(serde_json::json!({ "a": { "mode": "primary" } })),
        )
        .unwrap();
    store
        .add_subagent(&AddSubagentRequest {
            project: "alpha".into(),
            agent: "a".into(),
            name: "a-scout".into(),
            description: "scouts ahead".into(),
            prompt: "You scout.".into(),
            model: Some("ollama/x".into()),
            role: Some(AgentRole::Researcher),
        })
        .unwrap();
    let after = store.load_agent("alpha", "a").unwrap();
    assert_eq!(
        after.doc["agent"]["a-scout"]["mode"].as_str(),
        Some("subagent")
    );
    let task = &after.doc["agent"]["a"]["permission"]["task"];
    assert_eq!(
        task["a-scout"].as_str(),
        Some("allow"),
        "delegation wired: {task}"
    );
    assert_eq!(
        task["*"].as_str(),
        Some("deny"),
        "others still denied: {task}"
    );
    assert_eq!(
        after.meta.subagent_roles.get("a-scout"),
        Some(&AgentRole::Researcher)
    );
    // Duplicates and self-collisions refused.
    assert!(store
        .add_subagent(&AddSubagentRequest {
            project: "alpha".into(),
            agent: "a".into(),
            name: "a-scout".into(),
            description: "d".into(),
            prompt: "p".into(),
            model: None,
            role: None,
        })
        .is_err());
    assert!(store
        .add_subagent(&AddSubagentRequest {
            project: "alpha".into(),
            agent: "a".into(),
            name: "a".into(),
            description: "d".into(),
            prompt: "p".into(),
            model: None,
            role: None,
        })
        .is_err());

    store.remove_subagent("alpha", "a", "a-scout").unwrap();
    let after = store.load_agent("alpha", "a").unwrap();
    assert!(after.doc["agent"].get("a-scout").is_none(), "entry gone");
    assert!(
        after.doc["agent"]["a"]["permission"]["task"]
            .get("a-scout")
            .is_none(),
        "no dangling delegation rule"
    );
    assert!(
        after.meta.subagent_roles.is_empty(),
        "sidecar role cleaned up"
    );
}

/// Migration assigns a role only to agents that never had one, and is
/// safe to re-run: an agent the operator tightened by hand must never
/// be silently re-widened by a second pass.
#[test]
fn migrate_roles_is_idempotent_and_never_rewidens() {
    let store = tmp_store("migrate");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    // An agent whose permissions grant everything, with NO role yet:
    // strip the sidecar's role to simulate a pre-roles install.
    let mut perm = serde_json::Map::new();
    for tool in CORPUS_TOOLS {
        perm.insert(tool.to_string(), "allow".into());
    }
    store
        .save_agent(
            "alpha",
            "a",
            &doc(serde_json::json!({
                "a": { "mode": "primary", "permission": perm },
            })),
        )
        .unwrap();
    let dir = store.project_agent_dir("alpha", "a");
    fs::write(
        dir.join("agent.yaml"),
        "name: a\ncreated: 0\n", // legacy sidecar: no role key
    )
    .unwrap();
    assert!(
        !read_sidecar(&dir, "a").has_role(),
        "precondition: unassigned"
    );

    // Dry run reports without writing.
    let preview = store.migrate_agent_roles("alpha", false).unwrap();
    let row = preview.iter().find(|r| r.agent == "a").unwrap();
    assert_eq!(row.current, None);
    assert_eq!(
        row.inferred,
        AgentRole::Super,
        "permissions grant everything"
    );
    assert!(!row.applied);
    assert!(
        !read_sidecar(&dir, "a").has_role(),
        "a dry run writes nothing"
    );

    // Apply.
    let applied = store.migrate_agent_roles("alpha", true).unwrap();
    assert!(applied.iter().find(|r| r.agent == "a").unwrap().applied);
    assert_eq!(read_sidecar(&dir, "a").role(), AgentRole::Super);

    // The operator now tightens it by hand...
    store
        .set_agent_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    // ...and a second migration must LEAVE IT ALONE, even though the
    // permission block still says "allow everything".
    let again = store.migrate_agent_roles("alpha", true).unwrap();
    let row = again.iter().find(|r| r.agent == "a").unwrap();
    assert!(!row.applied, "an assigned role is never re-inferred");
    assert_eq!(
        read_sidecar(&dir, "a").role(),
        AgentRole::Researcher,
        "re-running migration must not undo a hand tightening"
    );
}
