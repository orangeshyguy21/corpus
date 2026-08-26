use super::*;

/// Cloning preserves subagents (it used to drop them) and carries the
/// role across.
#[test]
fn clone_preserves_subagents_and_role() {
    let store = tmp_store("clone-subs");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "src", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "src",
            &doc(serde_json::json!({
                "src": {
                    "mode": "primary",
                    "permission": { "task": { "*": "deny", "src-scout": "allow" } },
                },
                "src-scout": { "mode": "subagent", "description": "scout" },
            })),
        )
        .unwrap();
    store.clone_agent("alpha", "src", "dst").unwrap();
    let cloned = store.load_agent("alpha", "dst").unwrap();
    let map = cloned.doc["agent"].as_object().unwrap();
    assert!(map.contains_key("dst"), "primary renamed: {map:?}");
    assert!(!map.contains_key("src"), "old primary removed: {map:?}");
    assert!(
        map.contains_key("src-scout"),
        "subagents must survive a clone: {map:?}"
    );
}

#[cfg(unix)]
#[test]
fn clone_refuses_symlinks_without_publishing_a_partial_agent() {
    let store = tmp_store("clone-symlink");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "src", AgentRole::Researcher)
        .unwrap();
    let outside = store.root().join("outside-prompt.md");
    fs::write(&outside, "not part of the agent tree").unwrap();
    std::os::unix::fs::symlink(
        &outside,
        store
            .project_agent_dir("alpha", "src")
            .join("linked-prompt.md"),
    )
    .unwrap();

    let err = store.clone_agent("alpha", "src", "dst").unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
    assert!(
        !store.project_agent_dir("alpha", "dst").exists(),
        "preflight refusal must not publish even a partial destination"
    );

    fs::remove_file(
        store
            .project_agent_dir("alpha", "src")
            .join("linked-prompt.md"),
    )
    .unwrap();
    let outside_dir = store.root().join("outside-destination");
    fs::create_dir(&outside_dir).unwrap();
    fs::write(outside_dir.join("sentinel"), "unchanged").unwrap();
    std::os::unix::fs::symlink(&outside_dir, store.project_agent_dir("alpha", "dst")).unwrap();
    let err = store.clone_agent("alpha", "src", "dst").unwrap_err();
    assert!(err.to_string().contains("copy destination"), "{err}");
    assert_eq!(
        fs::read_to_string(outside_dir.join("sentinel")).unwrap(),
        "unchanged",
        "a planted destination symlink must never be followed"
    );
}

#[test]
fn create_and_clone_failures_remove_unpublished_destinations() {
    let store = tmp_store("agent-publication");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "src", AgentRole::Researcher)
        .unwrap();

    let create_error = store
        .create_agent(&CreateAgentRequest {
            project: "alpha".into(),
            slug: "created".into(),
            description: "derived".into(),
            prompt: "{file:missing.md}".into(),
            model: None,
            from: Some("src".into()),
            role: None,
        })
        .unwrap_err();
    assert!(
        create_error.to_string().contains("does not resolve"),
        "{create_error}"
    );
    assert!(
        !store.project_agent_dir("alpha", "created").exists(),
        "validation after copying must clean the unpublished directory"
    );

    fs::write(
        store
            .project_agent_dir("alpha", "src")
            .join("opencode.json"),
        serde_json::to_string_pretty(&doc(serde_json::json!({
            "helper": { "mode": "subagent" }
        })))
        .unwrap(),
    )
    .unwrap();
    let clone_error = store.clone_agent("alpha", "src", "cloned").unwrap_err();
    assert!(clone_error.to_string().contains("exactly one primary"));
    assert!(
        !store.project_agent_dir("alpha", "cloned").exists(),
        "an invalid source must be refused before publication"
    );
}

/// Cross-project copy — the operation that did not exist, and whose
/// absence cost a whole management-chat session.
#[test]
fn copy_agent_across_projects_carries_tree_and_role() {
    let store = tmp_store("copy");
    store.create_project("src", "S", "cdk-regtest").unwrap();
    store.create_project("dst", "D", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("src", "hunter", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "src",
            "hunter",
            &doc(serde_json::json!({
                "hunter": { "mode": "primary", "prompt": "hunt" },
                "hunter-scout": { "mode": "subagent", "description": "s" },
            })),
        )
        .unwrap();
    store
        .set_agent_role("src", "hunter", AgentRole::Tester)
        .unwrap();

    store.copy_agent("src", "hunter", "dst", "hunter").unwrap();
    let copied = store.load_agent("dst", "hunter").unwrap();
    assert_eq!(
        copied.doc["agent"]["hunter"]["prompt"].as_str(),
        Some("hunt")
    );
    assert!(
        copied.doc["agent"]["hunter-scout"].is_object(),
        "subagents cross with it"
    );
    assert_eq!(copied.meta.role(), AgentRole::Tester, "role carries over");
    assert_eq!(copied.meta.cloned_from.as_deref(), Some("src/hunter"));
    // The source is untouched, and a second copy is refused.
    assert!(store.load_agent("src", "hunter").is_ok());
    assert!(store.copy_agent("src", "hunter", "dst", "hunter").is_err());
    let err = store
        .copy_agent("src", "hunter", "ghost", "hunter")
        .unwrap_err()
        .to_string();
    assert!(err.contains("project not found"), "{err}");

    store.create_project("blocked", "B", "cdk-regtest").unwrap();
    store.request_project_delete("blocked").unwrap();
    let err = store
        .copy_agent("src", "hunter", "blocked", "copy")
        .unwrap_err()
        .to_string();
    assert!(err.contains("project blocked is pending deletion"), "{err}");
    assert!(!store.project_agent_dir("blocked", "copy").exists());

    store.request_agent_delete("src", "hunter").unwrap();
    let err = store
        .copy_agent("src", "hunter", "dst", "pending-copy")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("agent src/hunter is pending deletion"),
        "{err}"
    );
    assert!(!store.project_agent_dir("dst", "pending-copy").exists());
}

#[test]
fn save_agent_refuses_invalid_and_persists_valid() {
    let store = tmp_store("save");
    // Saving edits an existing agent, so create one explicitly first.
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "a", AgentRole::Researcher)
        .unwrap();

    // No agent map.
    assert!(store
        .save_agent("p", "a", &serde_json::json!({ "$schema": "x" }))
        .is_err());
    // Empty agent map.
    assert!(store
        .save_agent("p", "a", &doc(serde_json::json!({})))
        .is_err());
    // Exactly one primary.
    assert!(store
        .save_agent(
            "p",
            "a",
            &doc(serde_json::json!({
                "one": {"mode": "primary", "prompt": "x"},
                "two": {"mode": "primary", "prompt": "y"},
            })),
        )
        .is_err());
    // Bad permission action.
    assert!(store
        .save_agent(
            "p",
            "a",
            &doc(serde_json::json!({
                "one": {"mode": "primary", "prompt": "x", "permission": "hax"},
            })),
        )
        .is_err());
    // Unresolved {file:} prompt ref.
    assert!(store
        .save_agent(
            "p",
            "a",
            &doc(serde_json::json!({
                "one": {"mode": "primary", "prompt": "see {file:nope.md}"},
            })),
        )
        .is_err());
    // Existing files outside the agent dir cannot be reached with `..`.
    let agent_dir = store.project_agent_dir("p", "a");
    fs::write(agent_dir.parent().unwrap().join("secret.md"), "secret").unwrap();
    assert!(store
        .save_agent(
            "p",
            "a",
            &doc(serde_json::json!({
                "one": {"mode": "primary", "prompt": "see {file:../secret.md}"},
            })),
        )
        .is_err());
    // A symlink planted inside the agent dir cannot inline an outside file.
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(
            agent_dir.parent().unwrap().join("secret.md"),
            agent_dir.join("escape.md"),
        )
        .unwrap();
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "see {file:escape.md}"},
                })),
            )
            .is_err());
    }
    // A valid document persists and reloads.
    store
        .save_agent(
            "p",
            "a",
            &doc(serde_json::json!({
                "one": {"mode": "primary", "prompt": "hello"},
                "two": {"mode": "subagent", "prompt": "hi", "permission": {"task": "allow"}},
            })),
        )
        .unwrap();
    let agent = store.load_agent("p", "a").unwrap();
    let map = agent.doc.get("agent").unwrap().as_object().unwrap();
    assert!(map.contains_key("one") && map.contains_key("two"));
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn create_agent_builds_a_valid_doc_from_structured_fields() {
    let store = tmp_store("create");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    // Blank path: minimal doc, validator passes, key == slug.
    store
        .create_agent(&CreateAgentRequest {
            project: "p".into(),
            slug: "depbot".into(),
            description: "scans deps".into(),
            prompt: "you scan deps".into(),
            model: None,
            from: None,
            role: None,
        })
        .unwrap();
    let agent = store.load_agent("p", "depbot").unwrap();
    let map = agent.doc.get("agent").unwrap().as_object().unwrap();
    let cfg = map.get("depbot").expect("primary key is the slug");
    assert_eq!(cfg.get("description").unwrap(), "scans deps");
    assert_eq!(cfg.get("prompt").unwrap(), "you scan deps");
    assert_eq!(cfg.get("mode").unwrap(), "primary");
    assert!(
        cfg.get("permission").is_none(),
        "blank path has no permission block"
    );
    // Duplicate refused.
    assert!(store
        .create_agent(&CreateAgentRequest {
            project: "p".into(),
            slug: "depbot".into(),
            description: "x".into(),
            prompt: "y".into(),
            model: None,
            from: None,
            role: None,
        })
        .is_err());
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn create_agent_from_inherits_and_overlays() {
    let store = tmp_store("createfrom");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent(&CreateAgentRequest {
            project: "p".into(),
            slug: "base".into(),
            description: "base agent".into(),
            prompt: "base prompt".into(),
            model: None,
            from: None,
            role: None,
        })
        .unwrap();
    // Give the base a permission block to inherit.
    let mut doc = store.load_agent("p", "base").unwrap().doc;
    doc["agent"]["base"]["permission"] = serde_json::json!({"bash": "deny"});
    store.save_agent("p", "base", &doc).unwrap();
    store
        .create_agent(&CreateAgentRequest {
            project: "p".into(),
            slug: "child".into(),
            description: "child desc".into(),
            prompt: "child prompt".into(),
            model: Some("ollama/x".into()),
            from: Some("base".into()),
            role: None,
        })
        .unwrap();
    let agent = store.load_agent("p", "child").unwrap();
    let map = agent.doc.get("agent").unwrap().as_object().unwrap();
    // The key is RENAMED to the new slug (the depbot-session lie:
    // agent_get depbot answered with a "researcher" doc).
    assert!(
        !map.contains_key("base"),
        "the inherited key must be renamed"
    );
    let cfg = map.get("child").expect("primary key is the new slug");
    assert_eq!(cfg.get("description").unwrap(), "child desc");
    assert_eq!(cfg.get("prompt").unwrap(), "child prompt");
    assert_eq!(cfg.get("model").unwrap(), "ollama/x");
    assert_eq!(
        cfg["permission"]["bash"],
        serde_json::json!("deny"),
        "permissions inherited"
    );
    // Missing 'from' names the rule.
    let err = store
        .create_agent(&CreateAgentRequest {
            project: "p".into(),
            slug: "orphan".into(),
            description: "d".into(),
            prompt: "p".into(),
            model: None,
            from: Some("ghost".into()),
            role: None,
        })
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("'from' must name an existing agent"),
        "{err}"
    );
    let _ = fs::remove_dir_all(store.root());
}

#[test]
fn clone_agent_renames_the_primary_key() {
    let store = tmp_store("clonerename");
    store.create_project("p", "P", "cdk-regtest").unwrap();
    // Create a researcher with a real document, then verify the clone
    // renames its primary key.
    store
        .create_agent_with_role("p", "researcher", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "p",
            "researcher",
            &doc(serde_json::json!({
                "researcher": {"mode": "primary", "description": "r", "prompt": "x"},
            })),
        )
        .unwrap();
    store.clone_agent("p", "researcher", "depbot").unwrap();
    let agent = store.load_agent("p", "depbot").unwrap();
    let map = agent.doc.get("agent").unwrap().as_object().unwrap();
    assert!(
        map.contains_key("depbot"),
        "clone must rename the primary key"
    );
    assert!(
        !map.contains_key("researcher"),
        "clone must not keep the old key"
    );
    // The not-found error teaches the create path.
    let err = store.clone_agent("p", "ghost", "x").unwrap_err();
    assert!(err.to_string().contains("agent_list"), "{err}");
    let err = store
        .save_agent(
            "p",
            "ghost",
            &doc(serde_json::json!({"a": {"prompt": "x"}})),
        )
        .unwrap_err();
    assert!(err.to_string().contains("agent_new"), "{err}");
    let _ = fs::remove_dir_all(store.root());
}
