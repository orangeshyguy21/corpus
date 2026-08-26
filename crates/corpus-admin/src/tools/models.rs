//! Typed arguments and generated schema for model discovery.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::Ctx;
use corpus_store::MissionRunRef;

use super::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};

const DESCRIPTION: &str = "Discover the models available to opencode launches: the exact provider/model id strings (with display names) that are valid in an agent config's model field or a launch arg, as reported by `opencode models --verbose`. Use 'filter' to narrow by substring (id or display name) instead of printing the whole catalog, and 'refresh' to bypass the TTL cache when the catalog changed. Always resolve an id through this list before writing it into an agent config — never guess one.";

pub(crate) static DEFINITION: ToolDefinition = ToolDefinition {
    name: "model_list",
    description: DESCRIPTION,
    input_schema: input_schema::<ModelListArgs>,
    handler,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Read,
        confirmation: ConfirmationPolicy::None,
        audit: AuditPolicy::None,
        refresh: RefreshPolicy::None,
    },
};

#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(default)]
pub(crate) struct ModelListArgs {
    /// Optional case-insensitive substring matched against id and display name.
    pub(crate) filter: Option<String>,
    /// Bypass the TTL cache and re-pull OpenCode's catalog.
    pub(crate) refresh: bool,
}

impl ModelListArgs {
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        parse_args(DEFINITION.name, value)
    }
}

/// Resolve the exact OpenCode launch model ids accepted by agent configs.
fn handler(_ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args = ModelListArgs::parse(value)?;
    let filter = args.filter.map(|filter| filter.to_lowercase());
    let list = corpus_observe::model_list(args.refresh)
        .map_err(|error| Error::Args(format!("opencode model catalog unavailable: {error}")))?;
    let mut lines = Vec::new();
    let mut total = 0usize;
    for group in &list.groups {
        let mut first_in_group = true;
        for model in &group.models {
            total += 1;
            if let Some(filter) = &filter {
                if !model.id.to_lowercase().contains(filter)
                    && !model.name.to_lowercase().contains(filter)
                {
                    continue;
                }
            }
            if first_in_group {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(format!("== {}", group.label));
                first_in_group = false;
            }
            lines.push(format!("  {}  ({})", model.id, model.name));
        }
    }
    if lines.is_empty() {
        return Ok(format!(
            "no model matches {:?} ({} models in the catalog) — widen or drop the filter",
            filter.unwrap_or_default(),
            total
        ));
    }
    let header = match &filter {
        Some(filter) => format!(
            "{} of {} models matching {filter:?} — ids as in `opencode models --verbose`:",
            lines.iter().filter(|line| line.starts_with("  ")).count(),
            total
        ),
        None => format!(
            "{total} models available — ids as in `opencode models --verbose` (use these exact strings in agent configs):"
        ),
    };
    Ok(format!("{header}\n{}", lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_schema_and_deserializer_share_the_model_list_contract() {
        let schema = DEFINITION.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["refresh"]["type"], "boolean");
        assert!(schema["properties"]["filter"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("case-insensitive")));
        assert!(schema
            .get("required")
            .and_then(Value::as_array)
            .is_none_or(Vec::is_empty));

        assert_eq!(
            ModelListArgs::parse(&json!({})).unwrap(),
            ModelListArgs::default()
        );
        assert_eq!(
            ModelListArgs::parse(&json!({
                "filter": "qwen",
                "refresh": true,
                "future_optional_field": "ignored for compatibility"
            }))
            .unwrap(),
            ModelListArgs {
                filter: Some("qwen".into()),
                refresh: true,
            }
        );
        assert!(ModelListArgs::parse(&json!({"refresh": "yes"})).is_err());
    }

    #[test]
    fn admin_dispatch_resolves_model_list_through_its_definition() {
        let store = corpus_store::Store::new(std::env::temp_dir().join("unused-model-list-store"));
        let mut pending_confirms = std::collections::HashMap::new();
        let mut ctx = Ctx {
            store: &store,
            pending_confirms: &mut pending_confirms,
        };

        let error = crate::dispatch(&mut ctx, DEFINITION.name, &json!({"refresh": "yes"}))
            .expect_err("typed arguments must be checked before model discovery");
        assert!(error.to_string().contains("model_list arguments"));
    }
}
