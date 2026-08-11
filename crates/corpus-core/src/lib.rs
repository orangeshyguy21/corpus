//! corpus-core: plugin protocol, plugin registry, model registry, and the
//! scoped corpus store (core templates, projects, teams, corpora).
//!
//! The core library is UI-agnostic; the CLI/TUI and (later) the desktop
//! app sit on top of it. See docs/architecture.md and dev/data-model-plan.md
//! for the design.

mod error;
pub mod frontmatter;
mod models;
mod plugin;
mod registry;
mod sensitivity;
mod store;
mod templates;

pub use error::{Error, Result};
pub use models::{ModelEntry, ModelRegistry};
pub use plugin::{
    FaucetCall, FaucetResult, OracleInfo, OracleResult, Plugin, PluginManifest, ProbeResult,
    SandboxExecResult, SourceInfo,
};
pub use registry::{discover, plugins_dir, PluginDir};
pub use sensitivity::Sensitivity;
pub use store::{
    checksum, core_agent_instances, project_slug_env, store_root_env, team_slug_env, validate_slug,
    AgentInstance, MigrationReport, MigrateOptions, Project, Promoted, Scope, Store, TeamSpec,
    CATEGORIES, DEFAULT_PROJECT_SLUG, DEFAULT_TEAM_SLUG, PROJECT_ENV, STORE_ENV, TEAM_ENV,
};
pub use templates::{AgentTemplate, PermissionTemplate, PromptTemplate, Templates};
