//! Error types for corpus-core.

use std::io;
use std::path::PathBuf;

/// Top-level error for the corpus core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Filesystem failure.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// JSON (de)serialization failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML (de)serialization failure.
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// TOML (de)serialization failure.
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),

    /// A store layout violation (invalid slug, missing project/team, a
    /// refused promotion, etc.).
    #[error("store error: {0}")]
    Store(String),

    /// A plugin manifest is missing or malformed.
    #[error("invalid plugin manifest at {0}: {1}")]
    Manifest(PathBuf, String),

    /// A plugin returned an error or violated the protocol.
    #[error("plugin {plugin} error: {message}")]
    Plugin {
        /// Plugin name.
        plugin: String,
        /// Human-readable detail.
        message: String,
    },

    /// The plugin process died or produced no reply.
    #[error("plugin {0} closed its protocol stream unexpectedly")]
    PluginClosed(String),
}

/// Result alias for corpus-core.
pub type Result<T> = std::result::Result<T, Error>;
