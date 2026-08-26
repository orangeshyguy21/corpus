use corpus_store::AgentRole;
use schemars::JsonSchema;
use serde::Deserialize;

use super::super::registry::{
    AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability, ToolKind, ToolPolicy,
};

pub(super) const WRITE_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Write,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::Category("agents"),
    refresh: RefreshPolicy::Area("agents"),
};

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(super) enum AgentRoleArg {
    Super,
    Curator,
    Tester,
    Researcher,
}

impl From<AgentRoleArg> for AgentRole {
    fn from(role: AgentRoleArg) -> Self {
        match role {
            AgentRoleArg::Super => Self::Super,
            AgentRoleArg::Curator => Self::Curator,
            AgentRoleArg::Tester => Self::Tester,
            AgentRoleArg::Researcher => Self::Researcher,
        }
    }
}
