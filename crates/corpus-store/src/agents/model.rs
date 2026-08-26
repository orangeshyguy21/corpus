//! Agent persistence and mutation data contracts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::AgentRole;
use crate::missions::MissionDeleteRequest;

/// The corpus metadata sidecar (`agent.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSidecar {
    pub name: String,
    #[serde(default)]
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
    /// The primary agent's capability ceiling, stored outside its editable
    /// OpenCode document. `None` means never assigned and reads as the safest
    /// role while remaining distinguishable for migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
    /// Per-subagent roles, capped by the primary because one MCP server serves
    /// the entire OpenCode session.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subagent_roles: BTreeMap<String, AgentRole>,
    /// Last-change provenance. Missing values identify sidecars written before
    /// provenance tracking rather than inventing an operator mutation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_by: Option<String>,
    /// Durable lifecycle intent consumed after assigned environments stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_requested: Option<MissionDeleteRequest>,
}

impl AgentSidecar {
    /// The effective role: missing legacy metadata never widens authority.
    pub fn role(&self) -> AgentRole {
        self.role.unwrap_or_default()
    }

    /// Whether a role has ever been explicitly assigned.
    pub fn has_role(&self) -> bool {
        self.role.is_some()
    }
}

/// One agent's role-migration result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleMigration {
    pub agent: String,
    /// `None` means the sidecar predates explicit role assignment.
    pub current: Option<AgentRole>,
    pub inferred: AgentRole,
    pub applied: bool,
    /// Inference came from an absent permission block, whose implicit allow
    /// semantics require operator review.
    pub needs_review: bool,
}

/// A loaded agent: corpus-owned sidecar metadata plus its OpenCode document.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub meta: AgentSidecar,
    pub doc: serde_json::Value,
}

/// Complete intent for creating one primary agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAgentRequest {
    pub project: String,
    pub slug: String,
    pub description: String,
    pub prompt: String,
    pub model: Option<String>,
    pub from: Option<String>,
    pub role: Option<AgentRole>,
}

/// Complete intent for adding one delegated agent to a primary agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddSubagentRequest {
    pub project: String,
    pub agent: String,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub model: Option<String>,
    pub role: Option<AgentRole>,
}

/// A mission source pin as the renderer presents it.
#[derive(Debug, Clone)]
pub struct SourcePin {
    /// Repository name declared by the plugin.
    pub name: String,
    /// Revision label selected for the mission.
    pub rev: String,
    /// Resolved commit used by the launch.
    pub sha: String,
}
