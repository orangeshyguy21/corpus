//! corpus-core: plugin protocol, plugin registry, and model registry.
//!
//! The core library is UI-agnostic; the CLI/TUI and (later) the desktop
//! app sit on top of it. See docs/architecture.md for the design.

mod error;
mod models;
mod plugin;
mod registry;

pub use error::Error;
pub use models::{ModelEntry, ModelRegistry};
pub use plugin::{
    FaucetCall, FaucetResult, OracleInfo, OracleResult, Plugin, PluginManifest, ProbeResult,
    SandboxExecResult, SourceInfo,
};
pub use registry::{discover, plugins_dir, PluginDir};
