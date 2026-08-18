//! corpus-core: plugin protocol, plugin registry, model registry, and the
//! scoped corpus store (projects, agents, missions, corpus).
//!
//! The core library is UI-agnostic; the CLI/TUI and the desktop app sit on
//! top of it. See dev/architecture.md and dev/data-model-plan.md for the
//! design.

mod agents;
pub mod audit;
mod error;
pub mod frontmatter;
pub mod launch;
mod models;
pub mod paths;
mod plugin;
pub mod refusal;
mod registry;
mod sensitivity;
mod srcrev;
mod store;

pub use agents::{AgentConfig, AgentSidecar, DEFAULT_AGENT_NAME, OPENCODE_SCHEMA, SourcePin};
pub use error::{Error, Result};
pub use launch::{
    activity_from_idle, agent_default_model, agent_file_stem, export_session, kill_tmux_session,
    live_tui_sessions, mission_run_state, opencode_agent_handle, run_idle_secs, session_conversation,
    session_raw_log, tui_attach_command, MissionActivity, MissionRunState, RunLine, RunSession,
    WORKING_WINDOW_SECS,
};
pub use models::{model_list, ollama_models, ModelEntry, ModelList, ModelOption, ModelProviderGroup, ModelRegistry};
pub use paths::{
    corpus_mcp_bin, data_root, resource_root, resource_root_opt, sources_dir,
    store_root, HOME_ENV, RESOURCES_ENV,
};
pub use plugin::{
    FaucetCall, FaucetResult, OracleInfo, OracleResult, Plugin, PluginManifest, ProbeResult,
    SandboxExecResult, SourceInfo,
};
pub use agents::{
    infer_role, primary_handles, AgentRole, RoleMigration, CORPUS_TOOLS, CURATOR_TOOLS,
};
pub use registry::{discover, plugin_sources, plugin_status, plugins_dir, prepare_source_pins, validate_pin, PluginDir, PluginStatus, SourceRevs};
pub use sensitivity::Sensitivity;
pub use srcrev::{ensure_source_tree, is_commit_sha, resolve_rev, revs_cache_fetched, selectable_revs, REV_CACHE_TTL_SECS};
pub use store::{
    corpus_cost, corpus_stats, fnv1a_hex, mission_logs, project_slug_env, slugify,
    store_root_env, validate_slug, AppPrefs, CategoryStat, CorpusStats, CostReport, CostRow,
    EntryAccess, Mission, MissionLog, Project, Scope, Store, CATEGORIES,
    AGENT_ENV, PROJECT_ENV, RUN_LOG_ENV, RUNS, SOURCE_PINS_ENV, STORE_ENV,
};