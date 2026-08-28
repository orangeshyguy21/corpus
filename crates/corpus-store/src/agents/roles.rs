//! Agent role authority and tool-catalog policy.

use serde::{Deserialize, Serialize};

/// A first-class capability ceiling enforced by both rendering and corpus-mcp.
///
/// Deliberately not `Ord`: Curator and Tester occupy different risk domains,
/// so authority relationships must be expressed by [`AgentRole::cap_under`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    /// Reads and curates but never executes.
    #[default]
    Researcher,
    /// Executes against the regtest arena without open-internet access.
    Tester,
    /// Holds every capability inside one project.
    Super,
    /// Manages agents, missions, and corpus content without target execution.
    Curator,
}

/// Sandbox tool keys governed by every role decision.
pub const CORPUS_TOOLS: [&str; 10] = [
    "corpus_target_info",
    "corpus_technique_save",
    "corpus_sandbox_exec",
    "corpus_sandbox_write",
    "corpus_oracle_list",
    "corpus_oracle_run",
    "corpus_faucet",
    "corpus_wallet_fund",
    "corpus_probe_save",
    "corpus_finding_write",
];

/// Deprecated tool spellings rendered and accepted during the probe
/// namespace compatibility window. They never define separate authority.
pub const LEGACY_CORPUS_TOOLS: [&str; 1] = ["corpus_attack_save"];

const RESEARCHER_TOOLS: [&str; 3] = [
    "corpus_target_info",
    "corpus_technique_save",
    "corpus_finding_write",
];

/// Non-destructive, project-confined corpus tools available to every role.
const CONTRIBUTOR_TOOLS: [&str; 1] = ["entry_write"];

/// Curator's project-scoped management catalog.
pub const CURATOR_TOOLS: [&str; 27] = [
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_delete",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_new",
    "mission_launch",
    "mission_delete",
    "mission_set_budget",
    "mission_set_pins",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "finding_list",
    "entry_delete",
    "entry_move",
    "entry_write",
    "model_list",
];

/// Super's management catalog: Curator plus project-local corpus wipe.
pub const SUPER_ADMIN_TOOLS: [&str; 28] = [
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_delete",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_new",
    "mission_launch",
    "mission_delete",
    "mission_set_budget",
    "mission_set_pins",
    "corpus_wipe",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "finding_list",
    "entry_delete",
    "entry_move",
    "entry_write",
    "model_list",
];

/// Every management permission the renderer must classify, including retired
/// or app-only tools whose stored grants must not survive by omission.
pub(super) const PROJECT_MANAGEMENT_TOOLS: [&str; 29] = [
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_delete",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_await",
    "mission_new",
    "mission_launch",
    "mission_delete",
    "mission_set_budget",
    "mission_set_pins",
    "corpus_wipe",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "finding_list",
    "entry_delete",
    "entry_move",
    "entry_write",
    "model_list",
];

impl AgentRole {
    pub const ALL: [Self; 4] = [Self::Super, Self::Curator, Self::Tester, Self::Researcher];

    /// Kept separate from UI order so picker changes cannot widen migrations.
    pub(super) const LEGACY_INFERENCE_ORDER: [Self; 4] =
        [Self::Researcher, Self::Tester, Self::Super, Self::Curator];

    pub fn parse(raw: &str) -> Option<Self> {
        let key = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|role| role.as_str() == key)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Researcher => "researcher",
            Self::Tester => "tester",
            Self::Super => "super",
            Self::Curator => "curator",
        }
    }

    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>()
            .join("|")
    }

    pub fn default_prompt(self) -> &'static str {
        match self {
            Self::Researcher => include_str!("../prompts/researcher.md"),
            Self::Tester => include_str!("../prompts/tester.md"),
            Self::Super => include_str!("../prompts/super.md"),
            Self::Curator => include_str!("../prompts/curator.md"),
        }
    }

    pub fn default_description(self) -> &'static str {
        match self {
            Self::Researcher => {
                "Reads the corpus, the pinned source and the open internet; never executes. \
                 Persists cited project knowledge wherever it fits best."
            }
            Self::Tester => {
                "Runs adversarial missions against sandboxed targets through the corpus tools \
                 (sandbox, oracles, faucet, gated findings). No open internet."
            }
            Self::Super => {
                "Full authority inside this project: research, sandbox execution, corpus work, \
                 team and mission management, and confirmation-gated destructive maintenance."
            }
            Self::Curator => {
                "Manages this project: its agents, their roles, its missions and its corpus. \
                 Runs no missions itself."
            }
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::Researcher => {
                "reads and records project knowledge: target_info, entry_write, finding_write, \
                 technique_save, and the open internet. No execution."
            }
            Self::Tester => {
                "acts in the regtest arena: sandbox, oracles, faucet, findings, probes. \
                 No open internet, so an execution turn cannot pull in untrusted text."
            }
            Self::Super => {
                "all current-project capabilities: web research, sandbox execution, findings, \
                 project management, and confirmation-gated destructive maintenance."
            }
            Self::Curator => {
                "manages the project's agents, missions and corpus through the corpus server. \
                 No sandbox or open internet; corpus wipe and project lifecycle stay above it."
            }
        }
    }

    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Researcher => &RESEARCHER_TOOLS,
            Self::Tester | Self::Super => &CORPUS_TOOLS,
            Self::Curator => &[],
        }
    }

    pub fn admin_tools(self) -> &'static [&'static str] {
        match self {
            Self::Super => &SUPER_ADMIN_TOOLS,
            Self::Curator => &CURATOR_TOOLS,
            Self::Researcher | Self::Tester => &CONTRIBUTOR_TOOLS,
        }
    }

    pub fn allows(self, tool: &str) -> bool {
        let mut key = if tool.starts_with("corpus_") {
            tool.to_string()
        } else {
            format!("corpus_{tool}")
        };
        if key == "corpus_attack_save" {
            key = "corpus_probe_save".to_string();
        }
        self.tools().contains(&key.as_str())
    }

    pub fn grants_web(self) -> bool {
        matches!(self, Self::Researcher | Self::Super)
    }

    /// Host shell access could forge the session's project-scoped identity.
    pub fn shell_would_defeat_gate(self) -> bool {
        true
    }

    /// Cap a requested subagent role under the primary session authority.
    pub fn cap_under(self, primary: Self) -> Self {
        match (primary, self) {
            (a, b) if a == b => a,
            (Self::Super, sub) => sub,
            (Self::Curator, _) => Self::Curator,
            (_, Self::Curator) => Self::Researcher,
            (Self::Tester, Self::Super) => Self::Tester,
            (Self::Tester, sub) => sub,
            (Self::Researcher, _) => Self::Researcher,
        }
    }
}

/// Infer the smallest role covering a legacy OpenCode permission block.
pub fn infer_role(config: &serde_json::Map<String, serde_json::Value>) -> AgentRole {
    let Some(permission) = config.get("permission").and_then(|value| value.as_object()) else {
        return AgentRole::Super;
    };
    let granted = |tool: &str| {
        !matches!(
            permission.get(tool).and_then(|value| value.as_str()),
            Some("deny") | Some("ask")
        )
    };
    let probe_granted = match (
        permission.contains_key("corpus_probe_save"),
        permission.contains_key("corpus_attack_save"),
    ) {
        (true, true) => granted("corpus_probe_save") && granted("corpus_attack_save"),
        (true, false) => granted("corpus_probe_save"),
        (false, true) => granted("corpus_attack_save"),
        (false, false) => true,
    };
    let wants_web = ["webfetch", "websearch"].iter().any(|tool| granted(tool));
    let needed: Vec<&str> = CORPUS_TOOLS
        .into_iter()
        // `sandbox_write` did not exist for older documents, so absence is
        // not interpreted as an intentional implicit grant during migration.
        .filter(|tool| *tool != "corpus_sandbox_write" || permission.contains_key(*tool))
        .filter(|tool| {
            if *tool == "corpus_probe_save" {
                probe_granted
            } else {
                granted(tool)
            }
        })
        .collect();
    AgentRole::LEGACY_INFERENCE_ORDER
        .into_iter()
        .find(|role| {
            needed.iter().all(|tool| role.allows(tool)) && (!wants_web || role.grants_web())
        })
        .unwrap_or(AgentRole::Super)
}
