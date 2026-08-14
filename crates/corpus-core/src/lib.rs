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
mod store;

pub use agents::{AgentConfig, AgentSidecar, CORE_SEEDS, OPENCODE_SCHEMA};
pub use error::{Error, Result};
pub use launch::{
    agent_default_model, agent_file_stem, export_session, kill_tmux_session, live_tui_sessions,
    tui_attach_command, RunLine, RunSession,
};
pub use models::{model_list, ollama_models, ModelEntry, ModelList, ModelOption, ModelProviderGroup, ModelRegistry};
pub use plugin::{
    FaucetCall, FaucetResult, OracleInfo, OracleResult, Plugin, PluginManifest, ProbeResult,
    SandboxExecResult, SourceInfo,
};
pub use registry::{discover, plugin_sources, plugin_status, plugins_dir, PluginDir, PluginStatus, SourceRevs};
pub use sensitivity::Sensitivity;
pub use store::{
    checksum, corpus_stats, fnv1a_hex, project_slug_env, store_root_env, validate_slug, CorpusStats,
    MigrationReport, MigrateOptions, Mission, Project, Scope, Store, CATEGORIES,
    DEFAULT_PROJECT_SLUG, PROJECT_ENV, STORE_ENV,
};