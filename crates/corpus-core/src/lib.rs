//! corpus-core: plugin protocol, plugin registry, model registry, and the
//! scoped corpus store (projects, agents, missions, corpus).
//!
//! The core library is UI-agnostic; the CLI/TUI and the desktop app sit on
//! top of it. See dev/architecture.md and dev/data-model-plan.md for the
//! design.

mod agents;
mod error;
pub mod frontmatter;
pub mod launch;
mod models;
mod plugin;
mod registry;
mod sensitivity;
mod srcrev;
mod store;

pub use agents::{AgentConfig, AgentSidecar, CORE_SEEDS, OPENCODE_SCHEMA, SourcePin};
pub use error::{Error, Result};
pub use launch::{
    agent_default_model, agent_file_stem, export_session, kill_tmux_session, live_tui_sessions,
    run_idle_secs, session_raw_log, tui_attach_command, RunLine, RunSession,
};
pub use models::{model_list, ollama_models, ModelEntry, ModelList, ModelOption, ModelProviderGroup, ModelRegistry};
pub use plugin::{
    FaucetCall, FaucetResult, OracleInfo, OracleResult, Plugin, PluginManifest, ProbeResult,
    SandboxExecResult, SourceInfo,
};
pub use agents::{infer_role, AgentRole, RoleMigration, CORPUS_TOOLS};
pub use registry::{discover, plugin_sources, plugin_status, plugins_dir, prepare_source_pins, PluginDir, PluginStatus, SourceRevs};
pub use sensitivity::Sensitivity;
pub use srcrev::{ensure_source_tree, resolve_rev, revs_cache_fetched, selectable_revs, REV_CACHE_TTL_SECS};
pub use store::{
    checksum, corpus_cost, corpus_stats, fnv1a_hex, mission_logs, project_slug_env, slugify,
    store_root_env, validate_slug, CategoryStat, CorpusStats, CostReport, CostRow,
    MigrationReport, MigrateOptions, Mission, MissionLog, Project, Scope, Store, CATEGORIES,
    AGENT_ENV, DEFAULT_PROJECT_SLUG, PROJECT_ENV, RUN_LOG_ENV, RUNS, SOURCE_PINS_ENV, STORE_ENV,
};