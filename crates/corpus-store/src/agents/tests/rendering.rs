use super::*;

/// The render DERIVES the permission block from the role, so a stored
/// block that grants beyond the role cannot take effect — this is the
/// property the whole role system rests on.
#[test]
fn render_denies_corpus_tools_outside_the_role() {
    let store = tmp_store("role-deny");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    // The stored doc tries to grant EVERYTHING.
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
    // ...but the sidecar says researcher (the role it was created with).
    let text = fs::read_to_string(&store.render_project_agents("alpha").unwrap()[0]).unwrap();
    let perm = rendered_permission(&text);
    assert_eq!(perm["corpus_target_info"].as_str(), Some("allow"));
    assert_eq!(perm["corpus_technique_save"].as_str(), Some("allow"));
    for denied in [
        "corpus_sandbox_exec",
        "corpus_sandbox_write",
        "corpus_oracle_list",
        "corpus_oracle_run",
        "corpus_faucet",
        "corpus_wallet_fund",
        "corpus_attack_save",
        "corpus_finding_write",
    ] {
        assert_eq!(
            perm[denied].as_str(),
            Some("deny"),
            "a stored allow must not raise a researcher's ceiling: {denied}\n{text}"
        );
    }
}

/// Hand-tightening still works (deny-wins), and an entry authored with
/// NO permission block still gets a full explicit block — silence must
/// never mean allow in the rendered artifact.
#[test]
fn render_keeps_tightening_and_never_relies_on_omission() {
    let store = tmp_store("role-tighten");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "a",
            &doc(serde_json::json!({
                // No permission key at all.
                "a": { "mode": "primary", "prompt": "hi" },
            })),
        )
        .unwrap();
    let text = fs::read_to_string(&store.render_project_agents("alpha").unwrap()[0]).unwrap();
    let perm = rendered_permission(&text);
    for tool in CORPUS_TOOLS {
        assert!(
            perm[tool].as_str().is_some(),
            "{tool} must be written explicitly even with no stored block\n{text}"
        );
    }
}

/// A scalar `read`/`edit`/`write` used to skip red-line injection
/// entirely; it must be normalized to a map so the denies always land.
/// The agent tree itself is unwritable — it holds the role sidecars.
#[test]
fn red_lines_survive_scalar_permissions() {
    let store = tmp_store("role-scalar");
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
                    // Scalars, the shape that bypassed injection.
                    "permission": { "read": "allow", "write": "allow" },
                },
            })),
        )
        .unwrap();
    let text = fs::read_to_string(&store.render_project_agents("alpha").unwrap()[0]).unwrap();
    let perm = rendered_permission(&text);
    assert_eq!(
        perm["read"]["benchmarks/**"].as_str(),
        Some("deny"),
        "{text}"
    );
    assert_eq!(perm["read"]["plugins/**"].as_str(), Some("deny"), "{text}");
    assert_eq!(
        perm["write"]["store/projects/*/agents/**"].as_str(),
        Some("deny"),
        "no agent may rewrite the sidecars the role gate trusts\n{text}"
    );
}

/// The module doc has promised since the roles landed that a shell and a
/// restricted role cannot coexist. It was never enforced: `bash: deny`
/// was only a DEFAULT, so a stored allow rode straight through the
/// render — and a shell re-execs corpus-mcp with a forged identity.
#[test]
fn a_stored_bash_allow_cannot_survive_a_restricted_role() {
    let store = tmp_store("bash-hole");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    for (slug, role) in [
        ("res", AgentRole::Researcher),
        ("cur", AgentRole::Curator),
        ("sup", AgentRole::Super),
    ] {
        store.create_agent_with_role("alpha", slug, role).unwrap();
        store
            .save_agent(
                "alpha",
                slug,
                &doc(serde_json::json!({
                    slug: { "mode": "primary", "permission": { "bash": "allow" } },
                })),
            )
            .unwrap();
    }
    store.render_project_agents("alpha").unwrap();
    let read = |slug: &str| {
        fs::read_to_string(store.opencode_agent_dir("alpha").join(format!("{slug}.md"))).unwrap()
    };
    for slug in ["res", "cur", "sup"] {
        assert_eq!(
            rendered_permission(&read(slug))["bash"].as_str(),
            Some("deny"),
            "{slug}: a server-restricted role cannot hold a shell"
        );
    }
}

/// The management namespace is written for every role, denied wherever
/// it is not granted — the same discipline the corpus tools get, so the
/// artifact never depends on opencode's default for a tool the role has
/// an opinion about.
#[test]
fn render_derives_the_admin_namespace_for_curator_and_super() {
    let store = tmp_store("admin-ns");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "res", AgentRole::Researcher)
        .unwrap();
    store
        .create_agent_with_role("alpha", "cur", AgentRole::Curator)
        .unwrap();
    store
        .create_agent_with_role("alpha", "sup", AgentRole::Super)
        .unwrap();
    store.render_project_agents("alpha").unwrap();
    let read = |slug: &str| {
        rendered_permission(
            &fs::read_to_string(store.opencode_agent_dir("alpha").join(format!("{slug}.md")))
                .unwrap(),
        )
    };
    let (res, cur, sup) = (read("res"), read("cur"), read("sup"));
    for tool in PROJECT_MANAGEMENT_TOOLS {
        let key = format!("corpus_{tool}");
        assert_eq!(res[&key].as_str(), Some("deny"), "researcher: {key}");
        let expected = if CURATOR_TOOLS.contains(&tool) {
            "allow"
        } else {
            "deny"
        };
        assert_eq!(cur[&key].as_str(), Some(expected), "curator: {key}");
        let expected = if SUPER_ADMIN_TOOLS.contains(&tool) {
            "allow"
        } else {
            "deny"
        };
        assert_eq!(sup[&key].as_str(), Some(expected), "super: {key}");
    }
    assert_eq!(cur["corpus_mission_await"].as_str(), Some("deny"));
    assert_eq!(sup["corpus_mission_await"].as_str(), Some("deny"));
    // And the reverse: a curator holds no sandbox tools at all.
    for tool in CORPUS_TOOLS {
        assert_eq!(cur[tool].as_str(), Some("deny"), "curator: {tool}");
    }
    assert_eq!(cur["webfetch"].as_str(), Some("deny"));
    assert_eq!(cur["bash"].as_str(), Some("deny"));
}

/// Entry names are flat across a project's agents, so a collision must
/// be refused rather than silently resolved by slug order.
#[test]
fn render_refuses_colliding_entry_names() {
    let store = tmp_store("collide");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    for slug in ["one", "two"] {
        store
            .create_agent_with_role("alpha", slug, AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                slug,
                &doc(serde_json::json!({
                    slug: { "mode": "primary" },
                    "shared-scout": { "mode": "subagent", "description": "d" },
                })),
            )
            .unwrap();
    }
    let error = store
        .render_project_agents("alpha")
        .unwrap_err()
        .to_string();
    assert!(error.contains("shared-scout"), "{error}");
    assert!(error.contains("rename one"), "{error}");
}

/// The live leak, reduced: a primary that delegates to a subagent its
/// project never declares. opencode resolves such a name from whatever
/// config it discovers ABOVE the run dir — which is how project
/// `local-runner`'s discover reached `cloud-runner`'s scout. The
/// rendered set must be closed under delegation or the launch is
/// refused.
#[test]
fn render_refuses_dangling_delegation() {
    let store = tmp_store("dangling");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "discover", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "discover",
            &doc(serde_json::json!({
                "discover": {
                    "mode": "primary",
                    "permission": { "task": { "*": "deny", "discover-scout": "allow" } },
                },
            })),
        )
        .unwrap();
    let error = store
        .render_project_agents("alpha")
        .unwrap_err()
        .to_string();
    assert!(error.contains("discover-scout"), "{error}");
    assert!(error.contains("alpha"), "names the project: {error}");
    assert!(error.contains("closed"), "{error}");
    // The standalone checker agrees, so the app can flag it pre-launch.
    assert!(store.check_project_delegation("alpha").is_err());
}

/// Delegation ACROSS a project's agents is legal — the entry namespace
/// is flat, so `one` may call an entry `two` declares. This is why the
/// check cannot live in `validate_agent_doc`, which sees one document.
#[test]
fn delegation_across_agents_in_a_project_is_allowed() {
    let store = tmp_store("cross-delegate");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "one", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "one",
            &doc(serde_json::json!({
                "one": {
                    "mode": "primary",
                    "permission": { "task": { "*": "deny", "two-scout": "allow" } },
                },
            })),
        )
        .unwrap();
    store
        .create_agent_with_role("alpha", "two", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "two",
            &doc(serde_json::json!({
                "two": { "mode": "primary" },
                "two-scout": { "mode": "subagent", "description": "d" },
            })),
        )
        .unwrap();
    store.check_project_delegation("alpha").unwrap();
    let written = store.render_project_agents("alpha").unwrap();
    assert_eq!(written.len(), 3);
    let one = fs::read_to_string(store.opencode_agent_dir("alpha").join("one.md")).unwrap();
    assert_eq!(
        rendered_permission(&one)["task"]["two-scout"].as_str(),
        Some("allow")
    );
}

/// Glob matcher mirroring the enforcement layer's, for evaluating a
/// rendered rule map the way opencode would.
fn glob_matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.split_first(), t.split_first()) {
            (None, None) => true,
            (Some((b'*', rest)), _) => go(rest, t) || (!t.is_empty() && go(p, &t[1..])),
            (Some((b'?', rest)), Some((_, t_rest))) => go(rest, t_rest),
            (Some((pat, rest)), Some((txt, t_rest))) if pat == txt => go(rest, t_rest),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Last match wins, over the keys in the order the rendered file lists
/// them — which is lexicographic, because `canonical_json` sorts.
fn evaluate(rules: &crate::yaml::Value, path: &str) -> Option<String> {
    let map = rules.as_mapping()?;
    let mut action = None;
    for (k, v) in map {
        let (Some(pattern), Some(act)) = (k.as_str(), v.as_str()) else {
            continue;
        };
        if glob_matches(pattern, path) {
            action = Some(act.to_string());
        }
    }
    action
}

/// The relative patterns describe the run cwd. They say nothing about
/// an ABSOLUTE path, and after the store moved out of the repo an
/// absolute path is the one remaining way to name another project's
/// corpus — the run dir links only one project. Both spellings must
/// deny, and the agent's own corpus must still be reachable by both.
#[test]
fn rendered_permissions_deny_other_projects_absolutely() {
    let store = tmp_store("absolute");
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
                    "permission": { "read": { "*": "allow" }, "write": { "*": "deny" } },
                },
            })),
        )
        .unwrap();
    store.render_project_agents("alpha").unwrap();
    let text = fs::read_to_string(store.opencode_agent_dir("alpha").join("a.md")).unwrap();
    let read = &rendered_permission(&text)["read"];

    let root = store.root().display().to_string();
    let cases = [
        // (path, expected, why)
        (
            "store/projects/other/corpus/findings/x.md",
            "deny",
            "relative, another project",
        ),
        (
            "store/projects/alpha/corpus/findings/x.md",
            "allow",
            "relative, own corpus",
        ),
        (
            "store/projects/alpha/agents/a/agent.yaml",
            "deny",
            "own sidecars are not material",
        ),
        (
            &format!("{root}/projects/other/corpus/findings/x.md"),
            "deny",
            "absolute, another project",
        ),
        (
            &format!("{root}/projects/alpha/corpus/findings/x.md"),
            "allow",
            "absolute, own corpus",
        ),
    ];
    for (path, expected, why) in cases {
        assert_eq!(
            evaluate(read, path).as_deref(),
            Some(expected),
            "{why}: {path}\nrules: {read:?}"
        );
    }
}

/// An entry that says nothing about delegation must not inherit
/// opencode's default: `task` is always written, and an allow naming an
/// entry outside the project is force-denied in the ARTIFACT even if a
/// render path skipped the closure check.
#[test]
fn rendered_entries_deny_task_by_default() {
    let store = tmp_store("task-default");
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
    store.render_project_agents("alpha").unwrap();
    let text = fs::read_to_string(store.opencode_agent_dir("alpha").join("a.md")).unwrap();
    assert_eq!(
        rendered_permission(&text)["task"]["*"].as_str(),
        Some("deny")
    );

    // And the artifact-level force-deny, exercised directly: a stray
    // allow for an entry the project does not declare renders as deny.
    let known = BTreeSet::new();
    let ctx = RenderCtx {
        project: "alpha",
        role: AgentRole::Researcher,
        known_entries: &known,
        roots: DataRoots::default(),
    };
    let bound = bind_permission(
        &serde_json::json!({ "task": { "*": "deny", "ghost-scout": "allow" } }),
        &ctx,
    );
    assert_eq!(bound["task"]["ghost-scout"].as_str(), Some("deny"));
}

#[test]
fn render_binds_permission_and_scope_to_project() {
    let store = tmp_store("bind");
    store.create_project("alpha", "A", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("alpha", "a", AgentRole::Researcher)
        .unwrap();
    store
        .save_agent(
            "alpha",
            "a",
            &doc(serde_json::json!({
                "one": {
                    "mode": "primary",
                    "prompt": "hello",
                    "permission": {
                        "read": { "*": "allow", "benchmarks/**": "deny" },
                        "write": { "*": "deny", "store/projects/*/corpus/**": "allow" },
                        "bash": "deny"
                    }
                },
            })),
        )
        .unwrap();
    let written = store.render_project_agents("alpha").unwrap();
    let text = fs::read_to_string(&written[0]).unwrap();
    // Wildcard store paths bound to the concrete project.
    assert!(text.contains("store/projects/alpha/corpus/**"), "{text}");
    assert!(!text.contains("store/projects/*/corpus/**"), "{text}");
    // Read-allow gains the corpus boundary: other projects denied,
    // own project allowed (appended last so it wins evaluation).
    let fm = &text.split("---\n").nth(1).unwrap();
    let yaml: crate::yaml::Value = crate::yaml::from_str(&format!("{{{fm}}}").replace("---", ""))
        .unwrap_or_else(|_| crate::yaml::from_str(fm).unwrap());
    let read = &yaml["permission"]["read"];
    assert_eq!(read["store/projects/*"].as_str(), Some("deny"));
    // Narrowed to research material: the corpus and the mission
    // records, NOT the project's `agents/` sidecars (the role gate
    // trusts those) or its `var/`.
    assert_eq!(
        read["store/projects/alpha/corpus/**"].as_str(),
        Some("allow")
    );
    assert_eq!(
        read["store/projects/alpha/missions/**"].as_str(),
        Some("allow")
    );
    assert_eq!(read["store/projects/alpha/**"].as_str(), None);
    // Scalar permissions untouched.
    assert_eq!(yaml["permission"]["bash"].as_str(), Some("deny"));
    // The scope section names the project corpus.
    assert!(text.contains("## Corpus scope (bound at launch)"));
    assert!(text.contains("You are bound to project `alpha`"));
    // The pin section points at the TOOL and names no revision: the
    // render is a pure function of the project, so a second mission's
    // launch cannot rewrite the trees under a live one.
    assert!(text.contains("## Pinned sources"), "{text}");
    assert!(
        text.contains("Call `target_info` before you read any source"),
        "{text}"
    );
    assert!(
        !text.contains("sources/cdk/"),
        "a literal tree path is a per-RUN fact and must not reach a per-project file: {text}"
    );

    // A curator manages the project and reads no source, so it holds no
    // `target_info` and gets no pin section at all.
    store
        .create_agent_with_role("alpha", "keeper", AgentRole::Curator)
        .unwrap();
    store.render_project_agents("alpha").unwrap();
    let keeper = fs::read_to_string(store.opencode_agent_dir("alpha").join("keeper.md")).unwrap();
    assert!(!keeper.contains("Pinned sources"), "{keeper}");

    // THE property that lets two missions run at once: the render is a
    // pure function of the project, so a second mission's launch
    // rewrites the first's agent files with identical bytes. When the
    // pins lived in here, launching B changed the tree paths under a
    // live A — the same class of bug as telling an agent to read a
    // path that does not exist.
    let first: Vec<String> = store
        .render_project_agents("alpha")
        .unwrap()
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    let second: Vec<String> = store
        .render_project_agents("alpha")
        .unwrap()
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();
    assert_eq!(
        first, second,
        "a re-render must be byte-identical, or launching one mission \
         disturbs another that is already live"
    );
    let _ = fs::remove_dir_all(store.root());
}
