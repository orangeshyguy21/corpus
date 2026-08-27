//! Persisted corpus data, policy, and filesystem boundaries.
//!
//! Finding severity/discovery/writes, agent role policy, audit/refusal logs,
//! and project/mission/corpus CRUD live together so every consumer observes
//! one data contract. This crate contains no plugin process execution, source
//! fetch, run launch, tmux control, or UI/chat runtime.

pub mod accounting;
pub mod agents;
pub mod audit;
pub mod corpus_entries;
pub mod corpus_stats;
pub mod environment;
pub mod error;
mod filesystem;
pub mod findings;
pub mod frontmatter;
pub mod missions;
pub mod paths;
pub mod preferences;
pub mod probe_migration;
pub mod projects;
pub mod refusal;
pub mod run_records;
mod run_workspace;
pub mod sensitivity;
pub mod store;
pub mod yaml;

pub use agents::{
    infer_role, primary_handles, AddSubagentRequest, AgentConfig, AgentRole, AgentSidecar,
    CreateAgentRequest, RoleMigration, SourcePin, CORPUS_TOOLS, CURATOR_TOOLS, DEFAULT_AGENT_NAME,
    LEGACY_CORPUS_TOOLS, OPENCODE_SCHEMA, SUPER_ADMIN_TOOLS,
};
pub use environment::{EnvironmentSessionId, EnvironmentSessionRecord, EnvironmentSessionState};
pub use error::{Error, Result};
pub use findings::{
    finding_cards, query_findings, read_finding, scan_findings_cached, FindingCard,
    FindingIndexCache, FindingQuery, FindingReferenceSource, FindingScan, FindingScanStats,
    FindingSeverity, FindingSort, FindingTimeSource, FindingTitleSource, FindingWarning,
    FindingWriteResult, NewFinding, FINDING_PREFIX_LIMIT, FINDING_RESERVED_KEYS,
};
pub use probe_migration::ProbeMigration;
pub use run_records::{mission_logs, MissionLog, MISSION_ENV, RUNS, RUN_ID_ENV, RUN_LOG_ENV};
pub use sensitivity::Sensitivity;
pub use store::{
    corpus_cost, corpus_cost_cached, corpus_stats, fnv1a_hex, project_slug_env, slugify,
    store_root_env, validate_slug, AppPrefs, CategoryStat, CorpusCostCache, CorpusStats,
    CostReport, CostRow, EntryAccess, Mission, MissionCompletion, MissionControl,
    MissionDeleteRequest, MissionDispatch, MissionDispatchIdentity, MissionLaunchRequest,
    MissionRunRef, Project, RunWorkspace, Scope, Store, UsageSnapshot, AGENT_ENV, CATEGORIES,
    ENVIRONMENT_SESSION_ENV, LEGACY_ATTACKS, PROBES, PROJECT_ENV, SOURCE_PINS_ENV, STORE_ENV,
    USAGE_SNAPSHOT_VERSION,
};
