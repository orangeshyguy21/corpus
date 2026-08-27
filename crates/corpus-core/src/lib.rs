//! corpus-core: plugin protocol, plugin registry, model registry, and the
//! scoped corpus store (projects, agents, missions, corpus).
//!
//! The core library is UI-agnostic; the CLI and desktop app sit on top of it.
//! See `docs/architecture.md` for ownership and dependency direction.

mod agents {
    pub use corpus_store::agents::*;
}
pub mod audit {
    pub use corpus_store::audit::*;
}
mod error {
    pub use corpus_store::error::*;
}
mod findings {
    pub use corpus_store::findings::*;
}
pub mod frontmatter {
    pub use corpus_store::frontmatter::*;
}
pub mod launch;
mod models {
    pub use corpus_observe::models::*;
}
pub mod paths {
    pub use corpus_store::paths::*;
}
mod environment_session;
mod plugin;
mod plugin_distribution;
mod plugin_install;
pub mod refusal {
    pub use corpus_store::refusal::*;
}
mod registry;
mod sensitivity {
    pub use corpus_store::sensitivity::*;
}
mod srcrev;
mod store {
    pub use corpus_store::store::*;
}
#[cfg(test)]
mod test_support;

pub use agents::{
    infer_role, primary_handles, AddSubagentRequest, AgentRole, CreateAgentRequest, RoleMigration,
    CORPUS_TOOLS, CURATOR_TOOLS, LEGACY_CORPUS_TOOLS, SUPER_ADMIN_TOOLS,
};
pub use agents::{AgentConfig, AgentSidecar, SourcePin, DEFAULT_AGENT_NAME, OPENCODE_SCHEMA};
pub use corpus_observe::{
    EnvironmentDependency, PluginManifestVersion, PluginOrigin, PluginSource,
    ENVIRONMENT_PROTOCOL_V1, SUPPORTED_CAPABILITIES,
};
pub use corpus_store::{EnvironmentSessionId, EnvironmentSessionRecord, EnvironmentSessionState};
pub use environment_session::{
    close_environment_session, close_environment_session_key, open_environment_session,
};
pub use error::{Error, Result};
pub use findings::{
    finding_cards, query_findings, read_finding, scan_findings_cached, FindingCard,
    FindingIndexCache, FindingQuery, FindingReferenceSource, FindingScan, FindingScanStats,
    FindingSeverity, FindingSort, FindingTimeSource, FindingTitleSource, FindingWarning,
    FindingWriteResult, NewFinding, FINDING_PREFIX_LIMIT, FINDING_RESERVED_KEYS,
};
pub use launch::{
    activity_from_idle, agent_default_model, agent_file_stem, export_session, kill_tmux_session,
    kill_tmux_session_checked, live_tui_sessions, mission_run_state, opencode_agent_handle,
    opencode_control_password, run_idle_secs, session_conversation, session_raw_log,
    tui_attach_command, MissionActivity, MissionRunState, RunLine, RunSession, StopOutcome,
    WORKING_WINDOW_SECS,
};
pub use models::{
    model_list, ollama_models, ollama_models_refresh, ModelEntry, ModelList, ModelOption,
    ModelProviderGroup, ModelRegistry,
};
pub use paths::{
    corpus_admin_mcp_bin, corpus_mcp_bin, data_root, models_manifest, plugin_install_root,
    plugin_runtime_root, resource_root, resource_root_opt, sources_dir, store_root, HOME_ENV,
    MODELS_ENV, RESOURCES_ENV, SOURCES_DIR_ENV,
};
pub use plugin::{
    EnvironmentDescription, FaucetCall, FaucetResult, HelloResult, LifecycleLine,
    LifecycleProgress, OperationState, OperationStatus, OracleInfo, OracleResult, Plugin,
    PluginManifest, ProbeResult, ProtocolError, ProtocolV1Reply, SandboxExecResult, SourceInfo,
    TargetRecord, ToolRecord,
};
pub use plugin_distribution::{
    curated_plugin, curated_plugins, install_curated_plugin, install_curated_plugin_with,
    CuratedInstallPhase, CuratedPlugin, CuratedPluginRequirement,
};
pub use plugin_install::{
    call_plugin_lifecycle_cancellable, install_plugin_bundle, installed_record,
    plugin_bundle_digest, plugin_lifecycle_params, select_plugin_version, selected_version,
    verify_plugin_installation, InstallReceipt, InstallRecord,
};
pub use registry::{
    discover, find_plugin, plugin_catalog, plugin_sources, plugin_status, plugins_dir,
    prepare_source_pins, selected_plugin_status, validate_pin, PluginDir, PluginPreparedStatus,
    PluginStatus, SourceRevs,
};
pub use sensitivity::Sensitivity;
pub use srcrev::{
    ensure_source_tree, is_commit_sha, resolve_rev, revs_cache_fetched, selectable_revs,
    REV_CACHE_TTL_SECS,
};
pub use store::{
    corpus_cost, corpus_cost_cached, corpus_stats, fnv1a_hex, mission_logs, project_slug_env,
    slugify, store_root_env, validate_slug, AppPrefs, CategoryStat, CorpusCostCache, CorpusStats,
    CostReport, CostRow, EntryAccess, Mission, MissionCompletion, MissionControl,
    MissionDeleteRequest, MissionDispatch, MissionDispatchIdentity, MissionLaunchRequest,
    MissionLog, MissionRunRef, Project, RunWorkspace, Scope, Store, UsageSnapshot, AGENT_ENV,
    CATEGORIES, ENVIRONMENT_SESSION_ENV, LEGACY_ATTACKS, MISSION_ENV, PROBES, PROJECT_ENV, RUNS,
    RUN_ID_ENV, RUN_LOG_ENV, SOURCE_PINS_ENV, STORE_ENV, USAGE_SNAPSHOT_VERSION,
};
