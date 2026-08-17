//! Error types for corpus-mcp.

use std::io;

/// Errors surfaced inside the MCP server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Stdio transport failure.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// Malformed JSON on the wire.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A tool argument was invalid.
    #[error("invalid arguments: {0}")]
    Args(String),

    /// No usable project scope: the server could not establish which
    /// project's corpus it serves, so every scoped tool refuses rather
    /// than guessing one.
    #[error("scope error: {0}")]
    Scope(String),

    /// A plugin or plugin protocol error.
    #[error("plugin error: {0}")]
    Plugin(#[from] corpus_core::Error),

    /// An external command failed.
    #[error("command failed: {0}")]
    Command(String),
}

/// Result alias for tool handlers.
pub type Result<T> = std::result::Result<T, Error>;
