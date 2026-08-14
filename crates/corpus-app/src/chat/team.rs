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

/// The bare corpus-admin tool names (the `corpus-mcp --admin` catalog).
pub const ALL_ADMIN_TOOLS: &[&str] = &[
    "project_list",
    "project_new",
    "project_clone",
    "project_delete",
    "project_rebind",
    "agent_list",
    "agent_get",
    "agent_save",
    "agent_clone",
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
                "agent_save",
                "agent_clone",
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
        TeamRole::Orchestrator => "co-ordinates the specialist team; holds no admin tools and delegates via summon",
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
}