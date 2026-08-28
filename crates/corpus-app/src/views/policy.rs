//! Pure presentation model for an agent role's effective capability groups.
//!
//! The role picker and policy preview must never hand-author authority claims:
//! this module derives every group from `corpus_core::AgentRole`, the same
//! source of truth the renderer and corpus-mcp gate consume.

use corpus_core::AgentRole;

/// Human-scale capability groups used by the role comparison UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    OpenInternet,
    TargetResearch,
    CorpusPersistence,
    SandboxExecution,
    OracleExecution,
    ProjectAdministration,
}

impl Capability {
    pub const ALL: [Self; 6] = [
        Self::OpenInternet,
        Self::TargetResearch,
        Self::CorpusPersistence,
        Self::SandboxExecution,
        Self::OracleExecution,
        Self::ProjectAdministration,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OpenInternet => "Open internet",
            Self::TargetResearch => "Target research",
            Self::CorpusPersistence => "Corpus persistence",
            Self::SandboxExecution => "Sandbox execution",
            Self::OracleExecution => "Oracle execution",
            Self::ProjectAdministration => "Project administration",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::OpenInternet => "External web research",
            Self::TargetResearch => "Read target context and save techniques",
            Self::CorpusPersistence => "Write audited data anywhere in this project's corpus",
            Self::SandboxExecution => "Execute inside the isolated target",
            Self::OracleExecution => "Test target invariants through the environment",
            Self::ProjectAdministration => "Manage this project's team, missions and corpus",
        }
    }
}

/// One role's display policy, mechanically derived from core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RolePolicy {
    role: AgentRole,
}

impl RolePolicy {
    pub fn new(role: AgentRole) -> Self {
        Self { role }
    }

    #[cfg(test)]
    pub fn role(self) -> AgentRole {
        self.role
    }

    pub fn allows(self, capability: Capability) -> bool {
        match capability {
            Capability::OpenInternet => self.role.grants_web(),
            Capability::TargetResearch => {
                self.role.allows("target_info") || self.role.allows("technique_save")
            }
            Capability::CorpusPersistence => self.role.admin_tools().contains(&"entry_write"),
            Capability::SandboxExecution => self.role.allows("sandbox_exec"),
            Capability::OracleExecution => self.role.allows("oracle_run"),
            Capability::ProjectAdministration => self
                .role
                .admin_tools()
                .iter()
                .any(|tool| *tool != "entry_write"),
        }
    }
}

/// The role the preview must describe. A primary owns its requested ceiling;
/// a subagent is rendered under the primary session's core-defined cap.
pub fn effective_role(requested: AgentRole, primary: AgentRole, is_primary: bool) -> AgentRole {
    if is_primary {
        requested
    } else {
        requested.cap_under(primary)
    }
}

/// Compact picker copy. These stay intentionally subordinate to core's full
/// `hint()` text and are capped below 50 characters so all four roles fit in
/// one selector without turning it into a policy report.
pub fn short_description(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Researcher => "Web and source research; no execution",
        AgentRole::Tester => "Sandbox testing and findings; no web",
        AgentRole::Super => "All current-project permissions",
        AgentRole::Curator => "Manages agents, missions, and corpus",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_matrix_is_derived_from_role_authority() {
        use AgentRole::{Curator, Researcher, Super, Tester};
        use Capability::{
            CorpusPersistence, OpenInternet, OracleExecution, ProjectAdministration,
            SandboxExecution, TargetResearch,
        };

        let expected = [
            (Researcher, [true, true, true, false, false, false]),
            (Tester, [false, true, true, true, true, false]),
            (Super, [true, true, true, true, true, true]),
            (Curator, [false, false, true, false, false, true]),
        ];
        let capabilities = [
            OpenInternet,
            TargetResearch,
            CorpusPersistence,
            SandboxExecution,
            OracleExecution,
            ProjectAdministration,
        ];

        for (role, allowed) in expected {
            let policy = RolePolicy::new(role);
            assert_eq!(policy.role(), role);
            for (capability, expected) in capabilities.into_iter().zip(allowed) {
                assert_eq!(
                    policy.allows(capability),
                    expected,
                    "{role:?} / {capability:?}"
                );
            }
        }
    }

    #[test]
    fn capability_catalog_is_total_and_stable() {
        assert_eq!(Capability::ALL.len(), 6);
        for role in AgentRole::ALL {
            let policy = RolePolicy::new(role);
            assert_eq!(
                Capability::ALL
                    .into_iter()
                    .filter(|capability| policy.allows(*capability))
                    .count(),
                match role {
                    AgentRole::Researcher => 3,
                    AgentRole::Tester => 4,
                    AgentRole::Super => 6,
                    AgentRole::Curator => 2,
                }
            );
        }
    }

    #[test]
    fn every_subagent_role_is_previewed_under_the_primary_ceiling() {
        use AgentRole::{Curator, Researcher, Super, Tester};

        let expected = [
            (Researcher, [Researcher, Researcher, Researcher, Researcher]),
            (Tester, [Tester, Researcher, Tester, Researcher]),
            (Super, [Super, Curator, Tester, Researcher]),
            (Curator, [Curator, Curator, Curator, Curator]),
        ];
        for (primary, effective) in expected {
            for (requested, expected) in AgentRole::ALL.into_iter().zip(effective) {
                assert_eq!(
                    effective_role(requested, primary, false),
                    expected,
                    "primary={primary:?}, requested={requested:?}"
                );
                assert_eq!(effective_role(requested, primary, true), requested);
            }
        }
    }

    #[test]
    fn picker_descriptions_stay_below_fifty_characters() {
        for role in AgentRole::ALL {
            let description = short_description(role);
            assert!(description.chars().count() < 50, "{role:?}: {description}");
        }
    }
}
