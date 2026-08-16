//! The management-chat TEAM shape: per-specialist scoped tool sets
//! (dev/decisions.md chunk 2). This module is **pure data** — it holds NO
//! goose/GDK type, so it lives outside the goose quarantine. The backend
//! (`embedded.rs`, the one goose-aware file) maps each role onto an
//! `ExtensionConfig::Stdio` whose `available_tools` is exactly
//! [`TeamRole::admin_tools`]. Goose's own gate (`ExtensionConfig::is_tool_available`)
//! then refuses any tool NOT in that list — so a scope reaching out of its
//! domain fails BY CONSTRUCTION, not by the model deciding to decline.
//!
//! Semantics that drive the design (from the goose source):
//! - `available_tools` non-empty → ONLY those bare tool names are exposed.
//! - `available_tools` EMPTY → ALL tools available (goose default). So "no
//!   tools" is NOT an empty list: the Orchestrator registers **no** admin
//!   extension at all.
//! - The destructive set is excluded from EVERY specialist scope: destruction
//!   (`corpus_wipe` / `project_delete` / `agent_delete` / `mission_delete`) is
//!   operator-only, and even there it is gated by the inline approval
//!   (decision 5) + the corpus-mcp server-side confirm-token backstop.

use std::fmt;

/// The blast radius of every specialist: prohibited by construction.
pub const DESTRUCTIVE_TOOLS: &[&str] =
    &["corpus_wipe", "project_delete", "agent_delete", "mission_delete"];

/// Read-only admin tools: NO operator approval — a smart, effective agent
/// reads freely (the blanket Approve mode buried every `agent_list` under an
/// approval card).
pub const READ_ONLY_TOOLS: &[&str] = &[
    "project_list",
    "agent_list",
    "agent_get",
    "mission_list",
    "mission_get",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "model_list",
];

/// Mutating-but-not-destructive admin tools: approval-gated FOR NOW (the
/// operator watches mutations while the harness earns trust). Flip
/// `CORPUS_CHAT_APPROVE_WRITES=0` to release the gate — destructive tools
/// are NOT covered by that switch; they always gate.
pub const WRITE_TOOLS: &[&str] = &[
    "project_new",
    "project_clone",
    "project_rebind",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_copy",
    // The granular editors: one field per call instead of resending the
    // whole document. `agent_set_role` moves a SERVER-ENFORCED capability
    // ceiling, so it gates like any other write.
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "mission_new",
    "mission_set_budget",
    "mission_set_pins",
];

/// The bare tool name from a goose-prefixed call (`corpus-admin__agent_list`
/// → `agent_list`); un-prefixed names pass through.
pub fn bare_tool_name(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// Which store area a SUCCESSFUL call to this tool mutates ("projects" /
/// "agents" / "missions" / "corpus"), if any. Drives the app's nav refresh:
/// the sidebar re-reads the store when the chat changes it.
pub fn mutated_area(tool: &str) -> Option<&'static str> {
    match bare_tool_name(tool) {
        "project_new" | "project_clone" | "project_rebind" | "project_delete" => Some("projects"),
        "agent_new" | "agent_save" | "agent_clone" | "agent_delete" | "agent_copy"
        | "agent_set" | "agent_set_role" | "agent_set_permission" | "agent_subagent_add"
        | "agent_subagent_remove" => Some("agents"),
        "mission_new" | "mission_delete" | "mission_set_budget" | "mission_set_pins" => {
            Some("missions")
        }
        "corpus_wipe" => Some("corpus"),
        _ => None,
    }
}

/// Whether a tool call needs the operator's inline Approve before dispatch.
/// Policy (dev/chat-harness-plan.md): reads never gate; writes gate while
/// `CORPUS_CHAT_APPROVE_WRITES` is unset/non-"0"; the destructive set ALWAYS
/// gates — the kill-switch never covers it. Whitelists are the pure-data
/// tables above: adding/removing a tool is a one-line edit (the
/// classification test keeps them disjoint and complete).
pub fn needs_approval(tool: &str) -> bool {
    let bare = bare_tool_name(tool);
    if DESTRUCTIVE_TOOLS.contains(&bare) {
        return true;
    }
    if READ_ONLY_TOOLS.contains(&bare) {
        return false;
    }
    if WRITE_TOOLS.contains(&bare) {
        return std::env::var("CORPUS_CHAT_APPROVE_WRITES")
            .map(|v| v != "0")
            .unwrap_or(true);
    }
    // Unknown tool: gate it (fail closed).
    true
}

/// The bare corpus-admin tool names (the `corpus-mcp --admin` catalog).
pub const ALL_ADMIN_TOOLS: &[&str] = &[
    "project_list",
    "project_new",
    "project_clone",
    "project_delete",
    "project_rebind",
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_copy",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "agent_delete",
    "mission_list",
    "mission_get",
    "mission_new",
    "mission_delete",
    "mission_set_budget",
    "mission_set_pins",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "corpus_wipe",
    "model_list",
];

/// A role in the management-chat team.
///
/// `Operator` is the unfiltered (all-tools, still approval-gated) session the
/// app's chat defaults to; `Orchestrator` has NO admin tools (it co-ordinates
/// specialists); the four specialists each own a strict domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamRole {
    Operator,
    Orchestrator,
    AgentBuilder,
    ProjectManager,
    MissionManager,
    CorpusInspector,
}

impl TeamRole {
    /// The bare corpus-admin tool names this role may call. Empty for
    /// `Operator` (meaning "all tools" under goose's semantics) and for
    /// `Orchestrator`.
    pub fn admin_tools(self) -> Vec<String> {
        let names: &[&str] = match self {
            TeamRole::Operator | TeamRole::Orchestrator => &[],
            TeamRole::AgentBuilder => &[
                "agent_list",
                "agent_get",
                "agent_new",
                "agent_save",
                "agent_clone",
                // Cross-project copy + the granular editors: this
                // specialist's whole job is editing agents, and doing it a
                // field at a time is what keeps a local model from having
                // to re-emit an entire nested document.
                "agent_copy",
                "agent_set",
                "agent_set_role",
                "agent_set_permission",
                "agent_subagent_add",
                "agent_subagent_remove",
                // Model discovery: an edited config's model ids must resolve
                // against the real opencode catalog, never be guessed.
                "model_list",
            ],
            TeamRole::ProjectManager => &[
                "project_list",
                "project_new",
                "project_clone",
                "project_rebind",
            ],
            TeamRole::MissionManager => &[
                "mission_list",
                "mission_get",
                "mission_new",
                "mission_set_budget",
                "mission_set_pins",
            ],
            TeamRole::CorpusInspector => &[
                "project_list",
                "agent_list",
                "agent_get",
                "mission_list",
                "mission_get",
                "corpus_stats",
                "corpus_list",
                "corpus_read",
            ],
        };
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Whether this role has any admin capability at all (false for
    /// `Operator`/`Orchestrator` under goose's empty-means-all semantics).
    pub fn has_scoped_admin(self) -> bool {
        !matches!(self, TeamRole::Operator | TeamRole::Orchestrator)
    }

    /// A human label (used for logs / selection).
    pub fn label(self) -> &'static str {
        match self {
            TeamRole::Operator => "operator",
            TeamRole::Orchestrator => "orchestrator",
            TeamRole::AgentBuilder => "agent-builder",
            TeamRole::ProjectManager => "project-manager",
            TeamRole::MissionManager => "mission-manager",
            TeamRole::CorpusInspector => "corpus-inspector",
        }
    }
}

/// A short description of a role, for the summon discovery files and pickers.
pub fn role_description(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Operator => "unfiltered operator with the full corpus-admin catalog (destructive ops gated by Approve/Reject)",
        TeamRole::Orchestrator => "co-ordinates the specialist team; holds no admin tools and delegates via the delegate tool",
        TeamRole::AgentBuilder => "agent-builder: creates and edits agent configs in the project scope",
        TeamRole::ProjectManager => "project-manager: creates and rebinds project scopes / plugin bindings",
        TeamRole::MissionManager => "mission-manager: creates and sizes missions and their budgets/pins",
        TeamRole::CorpusInspector => "corpus-inspector: read-only inspection of projects, agents, missions and the corpus store",
    }
}

impl fmt::Display for TeamRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Parse a role from its label (`"corpus-inspector"`, `"orchestrator"`, …).
pub fn role_from_label(label: &str) -> Option<TeamRole> {
    ALL_ROLES
        .iter()
        .copied()
        .find(|r| r.label() == label)
}

/// Every specialist + orchestrator role (NOT `Operator`, which is the
/// unfiltered default and not a team member).
pub const SPECIALIST_ROLES: &[TeamRole] = &[
    TeamRole::Orchestrator,
    TeamRole::AgentBuilder,
    TeamRole::ProjectManager,
    TeamRole::MissionManager,
    TeamRole::CorpusInspector,
];

/// The roles the orchestrator may DELEGATE to via the `delegate` frontend
/// tool: the four specialists (never Operator, never itself).
pub const DELEGATABLE_ROLES: &[TeamRole] = &[
    TeamRole::AgentBuilder,
    TeamRole::ProjectManager,
    TeamRole::MissionManager,
    TeamRole::CorpusInspector,
];

/// Every role the panel can launch (Operator + Orchestrator + specialists).
pub const ALL_ROLES: &[TeamRole] = &[
    TeamRole::Operator,
    TeamRole::Orchestrator,
    TeamRole::AgentBuilder,
    TeamRole::ProjectManager,
    TeamRole::MissionManager,
    TeamRole::CorpusInspector,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// INJECTION PROBE — the deciding evidence for the team shape. Every
    /// specialist scope must be incapable of a destructive call BY
    /// CONSTRUCTION: `project_delete` and the rest of `DESTRUCTIVE_TOOLS` are
    /// never granted to any scope regardless of what the model is told.
    #[test]
    fn injection_probe_no_specialist_can_reach_corpus_wipe_or_deletes() {
        for role in SPECIALIST_ROLES {
            if *role == TeamRole::Orchestrator {
                continue; // no admin materialises an extension at all (see below)
            }
            let tools = role.admin_tools();
            for destructive in DESTRUCTIVE_TOOLS {
                assert!(
                    !tools.iter().any(|t| t == destructive),
                    "scope {role} must NOT grant {destructive} by construction (injection probe)"
                );
            }
        }
    }

    /// The orchestrator's capability surface is empty: no admin tool exists to
    /// call. Because goose treats an empty `available_tools` as "all tools",
    /// the orchestrator must register NO admin extension — assert the empty
    /// manifest and that the backend relies on absence, not an empty filter
    /// (see embedded.rs wiring test for the extension absence).
    #[test]
    fn orchestrator_has_no_admin_tools() {
        assert!(TeamRole::Orchestrator.admin_tools().is_empty());
        assert!(!TeamRole::Orchestrator.has_scoped_admin());
    }

    /// Cross-domain: corpus-inspector must not reach ANY mutating/cross-domain
    /// tool (no new/save/clone/rebind/budget/pins), not just the destructive
    /// four.
    #[test]
    fn corpus_inspector_is_read_only() {
        let tools = TeamRole::CorpusInspector.admin_tools();
        for forbidden in [
            "project_new",
            "project_clone",
            "project_rebind",
            "agent_save",
            "agent_clone",
            "mission_new",
            "mission_set_budget",
            "mission_set_pins",
            "corpus_wipe",
            "project_delete",
            "agent_delete",
            "mission_delete",
        ] {
            assert!(
                !tools.iter().any(|t| t == forbidden),
                "corpus-inspector must not reach {forbidden} by construction"
            );
        }
        // And it does hold its read-only capabilities.
        for required in ["project_list", "corpus_stats", "corpus_read", "agent_get"] {
            assert!(
                tools.iter().any(|t| t == required),
                "corpus-inspector must retain {required}"
            );
        }
    }

    /// `ALL_ADMIN_TOOLS` must be EXACTLY the server's `--admin` catalog.
    /// Nothing else can catch this: the two crates are independent, so a
    /// tool added to corpus-mcp and never classified here is invisible to
    /// every specialist and to the approval policy's read/write partition —
    /// a silent capability loss, in the direction that looks like nothing
    /// is wrong.
    #[test]
    fn admin_tool_table_matches_the_server_catalog() {
        let mut server: Vec<String> = corpus_mcp::admin::catalog()
            .as_array()
            .expect("catalog is an array")
            .iter()
            .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .collect();
        server.sort();
        let mut ours: Vec<String> = ALL_ADMIN_TOOLS.iter().map(|s| s.to_string()).collect();
        ours.sort();
        let missing: Vec<_> = server.iter().filter(|t| !ours.contains(t)).collect();
        let extra: Vec<_> = ours.iter().filter(|t| !server.contains(t)).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "team.rs drifted from the corpus-mcp --admin catalog.\n\
             in the server but unclassified here: {missing:?}\n\
             listed here but not in the server: {extra:?}"
        );
    }

    /// Every specialist domain is non-empty and every manifest entry is a real
    /// corpus-admin tool (no hallucinated/dangling names — a scope pointing at
    /// a nonexistent tool is a silent capability loss).
    #[test]
    fn every_specialist_domain_is_valid() {
        for role in SPECIALIST_ROLES {
            if *role == TeamRole::Orchestrator {
                continue;
            }
            let tools = role.admin_tools();
            assert!(!tools.is_empty(), "scope {role} must name its domain");
            for t in &tools {
                assert!(
                    ALL_ADMIN_TOOLS.contains(&t.as_str()),
                    "scope {role} names {t} which is not in the corpus-admin catalog"
                );
            }
        }
    }

    #[test]
    fn role_labels_round_trip() {
        for role in ALL_ROLES {
            assert_eq!(role_from_label(role.label()), Some(*role));
        }
        assert_eq!(role_from_label("bogus"), None);
    }

    /// The approval whitelists must partition the catalog EXACTLY: every
    /// admin tool is in precisely one of read-only / write / destructive.
    /// This is what makes "add/remove from the whitelist" a safe one-line
    /// edit — an unclassified tool fails here, not silently in production
    /// (where `needs_approval` fails closed).
    #[test]
    fn approval_classification_partitions_the_catalog() {
        for tool in ALL_ADMIN_TOOLS {
            let n = [
                READ_ONLY_TOOLS.contains(tool),
                WRITE_TOOLS.contains(tool),
                DESTRUCTIVE_TOOLS.contains(tool),
            ]
            .iter()
            .filter(|b| **b)
            .count();
            assert_eq!(n, 1, "{tool} must be in exactly one approval class");
        }
        // And no whitelist entry dangles outside the catalog.
        for tool in READ_ONLY_TOOLS
            .iter()
            .chain(WRITE_TOOLS)
            .chain(DESTRUCTIVE_TOOLS)
        {
            assert!(
                ALL_ADMIN_TOOLS.contains(tool),
                "{tool} is whitelisted but not in the corpus-admin catalog"
            );
        }
    }

    /// The policy itself: reads never gate, writes gate (unless the
    /// kill-switch is off), destruction ALWAYS gates, unknown gates.
    #[test]
    fn needs_approval_policy() {
        // Env hygiene: the kill-switch is unset in the test process.
        std::env::remove_var("CORPUS_CHAT_APPROVE_WRITES");
        assert!(!needs_approval("agent_list"));
        assert!(!needs_approval("corpus-admin__agent_list")); // prefixed form
        assert!(!needs_approval("corpus_read"));
        assert!(needs_approval("agent_save"));
        assert!(needs_approval("project_new"));
        for t in DESTRUCTIVE_TOOLS {
            assert!(needs_approval(t), "{t} must always gate");
        }
        assert!(needs_approval("some_unknown_tool"), "unknown fails closed");
        // The kill-switch releases WRITES only.
        std::env::set_var("CORPUS_CHAT_APPROVE_WRITES", "0");
        assert!(!needs_approval("agent_save"));
        assert!(needs_approval("corpus_wipe"), "destructive gates even with the kill-switch off");
        std::env::remove_var("CORPUS_CHAT_APPROVE_WRITES");
    }
}