use super::*;

/// Every corpus tool is classified by every role — a tool added to
/// corpus-mcp without a role decision must fail here, not silently
/// default to allowed somewhere.
#[test]
fn every_corpus_tool_is_classified_by_every_role() {
    for role in AgentRole::ALL {
        for tool in CORPUS_TOOLS {
            // `allows` accepts both the bare and the corpus_-prefixed
            // spelling; they must agree.
            let bare = tool.strip_prefix("corpus_").unwrap();
            assert_eq!(
                role.allows(tool),
                role.allows(bare),
                "{role:?} disagrees about {tool} vs {bare}"
            );
        }
        // Roles form a chain: researcher ⊆ tester ⊆ super.
        for tool in role.tools() {
            assert!(
                AgentRole::Super.allows(tool),
                "super must cover {tool} held by {role:?}"
            );
        }
    }
    assert!(AgentRole::Researcher.tools().len() < CORPUS_TOOLS.len());
    assert_eq!(AgentRole::Super.tools().len(), CORPUS_TOOLS.len());
    for role in AgentRole::ALL {
        assert_eq!(
            role.allows("corpus_probe_save"),
            role.allows("corpus_attack_save"),
            "legacy alias must not define separate authority for {role:?}"
        );
    }
    // Every role is current-project scoped; a host shell could forge a
    // different project even when the role already holds every local tool.
    for role in AgentRole::ALL {
        assert!(role.shell_would_defeat_gate(), "{role:?}");
    }
    // Round-trip every name.
    for role in AgentRole::ALL {
        assert_eq!(AgentRole::parse(role.as_str()), Some(role));
    }
    assert_eq!(AgentRole::parse("root"), None);
}

/// Inference reproduces the seeds' current behaviour, so migrating a
/// legacy agent changes nothing about what it can do.
#[test]
fn infer_role_matches_the_seed_permissions() {
    // Historical researcher seed: only target_info + technique_save allowed.
    // Inference remains backward-compatible even though new researchers also
    // receive the scoped persistence tools.
    let researcher = serde_json::json!({
        "permission": {
            "corpus_sandbox_exec": "deny", "corpus_faucet": "deny",
            "corpus_wallet_fund": "deny", "corpus_oracle_run": "deny",
            "corpus_oracle_list": "deny",
            "corpus_finding_write": "deny", "corpus_attack_save": "deny",
            "corpus_target_info": "allow", "corpus_technique_save": "allow",
            "webfetch": "allow", "websearch": "allow",
        }
    });
    assert_eq!(
        infer_role(researcher.as_object().unwrap()),
        AgentRole::Researcher
    );
    // The operator seed names no corpus_* key: silence means allow,
    // so it infers the full role rather than silently losing powers.
    let operator = serde_json::json!({ "permission": { "bash": "deny", "read": "deny" } });
    assert_eq!(infer_role(operator.as_object().unwrap()), AgentRole::Super);
    // No block at all: opencode allows everything.
    let bare = serde_json::json!({ "mode": "primary" });
    assert_eq!(infer_role(bare.as_object().unwrap()), AgentRole::Super);

    let mut conflicting_aliases = researcher.clone();
    conflicting_aliases["permission"]["corpus_probe_save"] = "allow".into();
    assert_eq!(
        infer_role(conflicting_aliases.as_object().unwrap()),
        AgentRole::Researcher,
        "the tighter alias decision must win during legacy inference"
    );
}

/// A subagent can be narrower than its parent but never wider — the
/// server cannot tell them apart at runtime, so the parent's ceiling
/// binds the whole session.
#[test]
fn subagent_role_is_capped_by_the_primary() {
    let mut meta = AgentSidecar {
        name: "discover".into(),
        created: 0,
        cloned_from: None,
        role: Some(AgentRole::Researcher),
        subagent_roles: Default::default(),
        modified: None,
        modified_by: None,
        delete_requested: None,
    };
    meta.subagent_roles.insert("scout".into(), AgentRole::Super);
    assert_eq!(
        entry_role(&meta, "scout", "discover"),
        AgentRole::Researcher,
        "a subagent must not exceed its parent"
    );
    meta.role = Some(AgentRole::Super);
    meta.subagent_roles
        .insert("scout".into(), AgentRole::Researcher);
    assert_eq!(
        entry_role(&meta, "scout", "discover"),
        AgentRole::Researcher,
        "a narrower subagent stays narrow"
    );
    assert_eq!(entry_role(&meta, "discover", "discover"), AgentRole::Super);
}

/// The subagent cap, as a table rather than as a consequence of
/// declaration order. It used to be `min` over a derived `Ord`, so the
/// answer for any pair was decided by where the variants happened to sit
/// in the enum body; this pins the relation the roles actually have, so
/// that adding a role that is NOT part of the chain shows up here as a
/// diff to read rather than as a number that silently changed.
#[test]
fn cap_under_reproduces_the_research_chain() {
    use AgentRole::{Researcher, Super, Tester};
    let table = [
        // (primary, subagent, capped)
        (Researcher, Researcher, Researcher),
        (Researcher, Tester, Researcher),
        (Researcher, Super, Researcher),
        (Tester, Researcher, Researcher),
        (Tester, Tester, Tester),
        (Tester, Super, Tester),
        (Super, Researcher, Researcher),
        (Super, Tester, Tester),
        (Super, Super, Super),
    ];
    for (primary, sub, want) in table {
        assert_eq!(
            sub.cap_under(primary),
            want,
            "{:?} under {:?}",
            sub.as_str(),
            primary.as_str()
        );
    }
    // The property the table encodes: a subagent never renders wider
    // than the session its parent's role bought.
    for primary in AgentRole::ALL {
        for sub in AgentRole::ALL {
            let capped = sub.cap_under(primary);
            for tool in capped.tools() {
                assert!(
                    primary.allows(tool),
                    "{} under {} kept {tool}, which the primary cannot call",
                    sub.as_str(),
                    primary.as_str()
                );
            }
        }
    }
}

/// Curator and Tester remain different domains, but Super contains both.
#[test]
fn super_contains_curator_while_other_research_roles_do_not() {
    use AgentRole::{Curator, Researcher, Super, Tester};
    assert_eq!(Curator.cap_under(Curator), Curator);
    // A curator primary curates its whole session.
    for sub in [Researcher, Tester, Super] {
        assert_eq!(
            sub.cap_under(Curator),
            Curator,
            "{} under a curator",
            sub.as_str()
        );
    }
    // Curator remains incompatible with the narrower research roles.
    for primary in [Researcher, Tester] {
        assert_eq!(
            Curator.cap_under(primary),
            Researcher,
            "a curator under {}",
            primary.as_str()
        );
    }
    assert_eq!(Curator.cap_under(Super), Curator);
    // The invariant the whole table exists to protect, restated over the
    // management namespace this time.
    for primary in AgentRole::ALL {
        for sub in AgentRole::ALL {
            for tool in sub.cap_under(primary).admin_tools() {
                assert!(
                    primary.admin_tools().contains(tool),
                    "{} under {} kept {tool}, which the primary cannot call",
                    sub.as_str(),
                    primary.as_str()
                );
            }
        }
    }
}

/// Picker order and legacy inference order are independent. Curator grants
/// no corpus tools and would match empty requirements if it moved earlier
/// in inference, while putting Super first would widen every migration.
#[test]
fn a_new_role_cannot_relabel_legacy_agents() {
    assert_eq!(
        AgentRole::ALL,
        [
            AgentRole::Super,
            AgentRole::Curator,
            AgentRole::Tester,
            AgentRole::Researcher
        ],
        "operator-facing authority/risk order"
    );
    assert_eq!(
        AgentRole::LEGACY_INFERENCE_ORDER[0],
        AgentRole::Researcher,
        "legacy inference stays safest-covering-first"
    );
    assert_eq!(
        AgentRole::LEGACY_INFERENCE_ORDER.last().copied(),
        Some(AgentRole::Curator),
        "a zero-corpus-tool role must infer last"
    );
    // An agent with no permission block at all, and one that grants
    // nothing: neither is a curator, whatever the tool arithmetic says.
    for doc in [
        serde_json::json!({}),
        serde_json::json!({ "permission": { "bash": "deny" } }),
    ] {
        let cfg = doc.as_object().unwrap().clone();
        assert_ne!(
            infer_role(&cfg),
            AgentRole::Curator,
            "inference must never invent a curator: {doc}"
        );
    }
}

/// Every role name survives a round trip. This is the ONLY thing that
/// catches a variant missing from `parse` — which is why `parse` is now
/// derived from `as_str` rather than written as a second match.
#[test]
fn every_role_name_round_trips() {
    for role in AgentRole::ALL {
        assert_eq!(AgentRole::parse(role.as_str()), Some(role));
        assert_eq!(AgentRole::parse(&role.as_str().to_uppercase()), Some(role));
        assert_eq!(
            AgentRole::parse(&format!("  {}  ", role.as_str())),
            Some(role)
        );
        assert!(
            AgentRole::names().contains(role.as_str()),
            "{} is missing from the usage line",
            role.as_str()
        );
    }
    assert_eq!(AgentRole::parse("root"), None);
    assert_eq!(AgentRole::parse(""), None);
}
