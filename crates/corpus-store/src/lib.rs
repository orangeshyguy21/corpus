//! Persisted corpus data, policy, and filesystem boundaries.
//!
//! Finding severity/discovery/writes, agent role policy, audit/refusal logs,
//! and project/mission/corpus CRUD live together so every consumer observes
//! one data contract. This crate contains no plugin process execution, source
//! fetch, run launch, tmux control, or UI/chat runtime.

pub mod agents;
pub mod audit;
pub mod error;
pub mod environment;
pub mod findings;
pub mod frontmatter;
pub mod paths;
pub mod refusal;
pub mod sensitivity;
pub mod store;

pub use agents::{
    infer_role, primary_handles, AgentConfig, AgentRole, AgentSidecar, RoleMigration, SourcePin,
    CORPUS_TOOLS, CURATOR_TOOLS, DEFAULT_AGENT_NAME, OPENCODE_SCHEMA, SUPER_ADMIN_TOOLS,
};
pub use error::{Error, Result};
pub use environment::{
    EnvironmentSessionId, EnvironmentSessionRecord, EnvironmentSessionState,
};
pub use findings::{
    finding_cards, query_findings, read_finding, scan_findings_cached, FindingCard,
    FindingIndexCache, FindingQuery, FindingReferenceSource, FindingScan, FindingScanStats,
    FindingSeverity, FindingSort, FindingTimeSource, FindingTitleSource, FindingWarning,
    FindingWriteResult, NewFinding, FINDING_PREFIX_LIMIT, FINDING_RESERVED_KEYS,
};
pub use sensitivity::Sensitivity;
pub use store::{
    corpus_cost, corpus_cost_cached, corpus_stats, fnv1a_hex, mission_logs, project_slug_env,
    slugify, store_root_env, validate_slug, AppPrefs, CategoryStat, CorpusCostCache, CorpusStats,
    CostReport, CostRow, EntryAccess, Mission, MissionCompletion, MissionControl, MissionDeleteRequest,
    MissionDispatch, MissionLaunchRequest, MissionLog, MissionRunRef, Project, Scope, Store, AGENT_ENV, CATEGORIES,
    ENVIRONMENT_SESSION_ENV, MISSION_ENV, PROJECT_ENV, RUNS, RUN_ID_ENV, RUN_LOG_ENV,
    SOURCE_PINS_ENV, STORE_ENV,
};
