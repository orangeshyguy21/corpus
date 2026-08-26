//! Declarative tool definitions shared by catalog and dispatch.

use corpus_store::MissionRunRef;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCapability {
    Admin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationPolicy {
    None,
    Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditPolicy {
    None,
    Category(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshPolicy {
    None,
    Area(&'static str),
}

pub(crate) struct ToolPolicy {
    pub(crate) capability: ToolCapability,
    pub(crate) kind: ToolKind,
    pub(crate) confirmation: ConfirmationPolicy,
    pub(crate) audit: AuditPolicy,
    pub(crate) refresh: RefreshPolicy,
}

type Handler = fn(&mut Ctx<'_>, &Value, Option<&MissionRunRef>) -> Result<String>;

pub(crate) struct ToolDefinition {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) input_schema: fn() -> Value,
    pub(crate) handler: Handler,
    pub(crate) policy: ToolPolicy,
}

impl ToolDefinition {
    pub(crate) fn catalog_entry(&self) -> Value {
        self.validate();
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": (self.input_schema)(),
        })
    }

    pub(crate) fn invoke(
        &self,
        ctx: &mut Ctx<'_>,
        args: &Value,
        origin: Option<&MissionRunRef>,
    ) -> Result<String> {
        self.validate();
        (self.handler)(ctx, args, origin)
    }

    fn validate(&self) {
        assert!(!self.name.is_empty(), "tool name must not be empty");
        assert!(
            !self.description.is_empty(),
            "{} description must not be empty",
            self.name
        );
        match self.policy.capability {
            ToolCapability::Admin => {}
        }
        match self.policy.kind {
            ToolKind::Read => {
                assert!(matches!(self.policy.confirmation, ConfirmationPolicy::None));
                assert!(matches!(self.policy.audit, AuditPolicy::None));
                assert!(matches!(self.policy.refresh, RefreshPolicy::None));
            }
            ToolKind::Write => {
                assert!(matches!(self.policy.confirmation, ConfirmationPolicy::None));
                assert!(matches!(self.policy.audit, AuditPolicy::Category(_)));
                assert!(matches!(self.policy.refresh, RefreshPolicy::Area(_)));
            }
            ToolKind::Destructive => {
                assert!(matches!(
                    self.policy.confirmation,
                    ConfirmationPolicy::Token
                ));
                assert!(matches!(self.policy.audit, AuditPolicy::Category(_)));
                assert!(matches!(self.policy.refresh, RefreshPolicy::Area(_)));
            }
        }
        if let AuditPolicy::Category(category) = self.policy.audit {
            assert!(
                !category.is_empty(),
                "{} audit category is empty",
                self.name
            );
        }
        if let RefreshPolicy::Area(area) = self.policy.refresh {
            assert!(!area.is_empty(), "{} refresh area is empty", self.name);
        }
    }
}

const DEFINITIONS: [&ToolDefinition; 35] = [
    &super::projects::LIST,
    &super::projects::NEW,
    &super::projects::CLONE,
    &super::projects::DELETE,
    &super::projects::REBIND,
    &super::agents::LIST,
    &super::agents::GET,
    &super::agents::NEW,
    &super::agents::SAVE,
    &super::agents::CLONE,
    &super::agents::COPY,
    &super::agents::SET,
    &super::agents::SET_ROLE,
    &super::agents::SET_PERMISSION,
    &super::agents::SUBAGENT_ADD,
    &super::agents::SUBAGENT_REMOVE,
    &super::agents::DELETE,
    &super::missions::LIST,
    &super::missions::GET,
    &super::missions::STATUS,
    &super::missions::AWAIT,
    &super::missions::NEW,
    &super::missions::LAUNCH,
    &super::missions::DELETE,
    &super::missions::SET_BUDGET,
    &super::missions::SET_PINS,
    &super::corpus::WIPE,
    &super::corpus::STATS,
    &super::corpus::LIST,
    &super::corpus::READ,
    &super::corpus::FINDING_LIST,
    &super::corpus::ENTRY_DELETE,
    &super::corpus::ENTRY_MOVE,
    &super::corpus::ENTRY_WRITE,
    &super::models::DEFINITION,
];

pub(crate) fn input_schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("tool input schema must serialize to JSON")
}

pub(crate) fn parse_args<T: DeserializeOwned>(tool: &str, value: &Value) -> Result<T> {
    serde_json::from_value(value.clone())
        .map_err(|error| Error::Args(format!("{tool} arguments: {error}")))
}

pub(crate) fn definition(name: &str) -> Option<&'static ToolDefinition> {
    DEFINITIONS
        .iter()
        .copied()
        .find(|definition| definition.name == name)
}

pub(crate) fn catalog_entries() -> impl Iterator<Item = Value> {
    DEFINITIONS
        .iter()
        .map(|definition| definition.catalog_entry())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_definition_is_unique_and_policy_complete() {
        let mut names = std::collections::BTreeSet::new();
        for tool in DEFINITIONS {
            assert!(names.insert(tool.name));
            let catalog = tool.catalog_entry();
            assert_eq!(catalog["name"], tool.name);
            assert!(catalog["inputSchema"].is_object());
            assert!(definition(tool.name).is_some());
        }
    }
}
