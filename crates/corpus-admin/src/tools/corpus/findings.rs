use std::collections::BTreeSet;

use corpus_store::{FindingQuery, FindingSeverity, FindingSort, MissionRunRef};
use schemars::JsonSchema;
use serde::{de::Error as _, Deserialize, Deserializer};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

pub(crate) static LIST: ToolDefinition = ToolDefinition {
    name: "finding_list",
    description: "List recursively discovered findings as structured JSON. Missing or invalid severity remains visible as unrated with metadata warnings. Filters never read full Markdown bodies.",
    input_schema: input_schema::<FindingListArgs>,
    handler: finding_list,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Read,
        confirmation: ConfirmationPolicy::None,
        audit: AuditPolicy::None,
        refresh: RefreshPolicy::None,
    },
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct FindingListArgs {
    project: String,
    /// Optional severity or array of severities; omitted means every rated severity.
    severity: Option<SeverityFilter>,
    /// Include findings with missing/invalid severity (default true).
    #[serde(default = "default_include_unrated")]
    include_unrated: bool,
    /// Case-insensitive title, reference, and relative-path search.
    text: Option<String>,
    /// Finding result order (default newest).
    #[serde(default)]
    sort: FindingSortArg,
    /// Maximum result count.
    #[schemars(range(min = 1))]
    limit: Option<usize>,
}

fn default_include_unrated() -> bool {
    true
}

#[derive(Debug, Clone, Copy, JsonSchema, PartialEq, Eq, PartialOrd, Ord)]
#[schemars(rename_all = "lowercase")]
enum SeverityArg {
    Critical,
    High,
    Medium,
    Low,
}

impl SeverityArg {
    fn parse(raw: &str) -> std::result::Result<Self, String> {
        match raw {
            "critical" => Ok(Self::Critical),
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            _ => Err(format!(
                "invalid finding severity {raw:?}; expected critical, high, medium, or low"
            )),
        }
    }
}

impl From<SeverityArg> for FindingSeverity {
    fn from(value: SeverityArg) -> Self {
        match value {
            SeverityArg::Critical => Self::Critical,
            SeverityArg::High => Self::High,
            SeverityArg::Medium => Self::Medium,
            SeverityArg::Low => Self::Low,
        }
    }
}

#[derive(Debug, JsonSchema, PartialEq, Eq)]
#[schemars(untagged)]
enum SeverityFilter {
    One(SeverityArg),
    Many(BTreeSet<SeverityArg>),
}

impl SeverityFilter {
    fn into_store(self) -> BTreeSet<FindingSeverity> {
        match self {
            Self::One(severity) => [severity.into()].into_iter().collect(),
            Self::Many(severities) => severities.into_iter().map(Into::into).collect(),
        }
    }
}

impl<'de> Deserialize<'de> for SeverityFilter {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        match value {
            Value::String(raw) => SeverityArg::parse(&raw)
                .map(Self::One)
                .map_err(D::Error::custom),
            Value::Array(values) => {
                let mut severities = BTreeSet::new();
                for value in values {
                    let raw = value.as_str().ok_or_else(|| {
                        D::Error::custom("severity array entries must be strings")
                    })?;
                    severities.insert(SeverityArg::parse(raw).map_err(D::Error::custom)?);
                }
                Ok(Self::Many(severities))
            }
            _ => Err(D::Error::custom(
                "severity must be a string or an array of strings",
            )),
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
#[schemars(rename_all = "lowercase")]
enum FindingSortArg {
    #[default]
    Newest,
    Severity,
}

impl From<FindingSortArg> for FindingSort {
    fn from(value: FindingSortArg) -> Self {
        match value {
            FindingSortArg::Newest => Self::Newest,
            FindingSortArg::Severity => Self::Severity,
        }
    }
}

fn finding_list(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: FindingListArgs = parse_args(LIST.name, value)?;
    if args.limit == Some(0) {
        return Err(Error::Args("limit must be a positive integer".into()));
    }
    let query = FindingQuery {
        severities: args
            .severity
            .map(SeverityFilter::into_store)
            .unwrap_or_default(),
        include_unrated: args.include_unrated,
        text: args.text,
        sort: args.sort.into(),
        limit: args.limit,
    };
    let cards = corpus_store::finding_cards(ctx.store, &args.project)
        .map_err(|error| Error::Args(error.to_string()))?;
    let cards = corpus_store::query_findings(&cards, &query);
    let findings: Vec<Value> = cards
        .iter()
        .map(|card| {
            json!({
                "path": card.path.to_string_lossy(),
                "title": card.title,
                "title_source": card.title_source.as_str(),
                "severity": card.severity.map(|severity| severity.as_str()),
                "unrated": card.severity.is_none(),
                "timestamp": card.timestamp,
                "time_source": card.time_source.map(|source| source.as_str()),
                "reference": card.reference,
                "reference_source": card.reference_source.as_str(),
                "status": card.status,
                "oracle_verified": card.oracle_verified,
                "sensitivity": card.sensitivity.map(|value| value.as_str()),
                "warnings": card.warnings.iter().map(|warning| warning.as_str()).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "project": args.project,
        "count": findings.len(),
        "findings": findings,
    }))
    .map_err(Error::Json)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn generated_schema_describes_the_complete_finding_query_contract() {
        let schema = LIST.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project"]));
        assert_eq!(schema["properties"]["include_unrated"]["type"], "boolean");
        assert_eq!(schema["properties"]["limit"]["minimum"], 1);
        let encoded = serde_json::to_string(&schema).unwrap();
        for value in [
            "critical",
            "high",
            "medium",
            "low",
            "newest",
            "severity",
            "uniqueItems",
        ] {
            assert!(encoded.contains(value), "schema omitted {value}: {encoded}");
        }
    }

    #[test]
    fn typed_query_defaults_and_validation_match_the_advertised_contract() {
        let defaults: FindingListArgs = parse_args(LIST.name, &json!({"project": "p"})).unwrap();
        assert_eq!(
            defaults,
            FindingListArgs {
                project: "p".into(),
                severity: None,
                include_unrated: true,
                text: None,
                sort: FindingSortArg::Newest,
                limit: None,
            }
        );
        let selected: FindingListArgs = parse_args(
            LIST.name,
            &json!({
                "project": "p",
                "severity": ["critical", "high", "high"],
                "include_unrated": false,
                "text": "needle",
                "sort": "severity",
                "limit": 4,
                "future_field": true
            }),
        )
        .unwrap();
        assert!(!selected.include_unrated);
        assert_eq!(selected.text.as_deref(), Some("needle"));
        assert_eq!(selected.sort, FindingSortArg::Severity);
        assert_eq!(selected.limit, Some(4));
        assert_eq!(selected.severity.unwrap().into_store().len(), 2);

        for bad in [
            json!({"project": "p", "severity": "urgent"}),
            json!({"project": "p", "severity": ["high", 2]}),
            json!({"project": "p", "include_unrated": "yes"}),
            json!({"project": "p", "text": false}),
            json!({"project": "p", "sort": "oldest"}),
            json!({"project": "p", "limit": -1}),
        ] {
            assert!(
                parse_args::<FindingListArgs>(LIST.name, &bad).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn finding_list_is_an_immediate_side_effect_free_read() {
        assert_eq!(LIST.policy.kind, ToolKind::Read);
        assert_eq!(LIST.policy.confirmation, ConfirmationPolicy::None);
        assert_eq!(LIST.policy.audit, AuditPolicy::None);
        assert_eq!(LIST.policy.refresh, RefreshPolicy::None);
    }

    #[test]
    fn zero_limit_is_rejected_before_finding_discovery() {
        let store = corpus_store::Store::new(
            std::env::temp_dir().join("unused-finding-list-zero-limit-store"),
        );
        let mut pending_confirms = HashMap::new();
        let mut ctx = Ctx {
            store: &store,
            pending_confirms: &mut pending_confirms,
        };
        let error = crate::dispatch(
            &mut ctx,
            LIST.name,
            &json!({"project": "missing", "limit": 0}),
        )
        .expect_err("zero cannot become an unbounded query")
        .to_string();
        assert!(error.contains("positive integer"), "{error}");
        assert!(!error.contains("project not found"), "{error}");
    }
}
