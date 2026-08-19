//! Errors shared by admin handlers and the research MCP adapter.

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

    /// A gate turned the call away. Carries WHICH gate, so the refusal log
    /// records a value the operator can filter on rather than re-deriving
    /// it from prose written for a model.
    ///
    /// Renders as the bare message, exactly like the `Args` refusals it
    /// replaces: the caller must see the same words it always did. The
    /// `gate` is for the log, not the wire.
    #[error("{message}")]
    Refused {
        gate: corpus_store::refusal::Gate,
        message: String,
    },

    /// No usable project scope: the server could not establish which
    /// project's corpus it serves, so every scoped tool refuses rather
    /// than guessing one.
    #[error("scope error: {0}")]
    Scope(String),

    /// A plugin or plugin protocol error.
    #[error("plugin error: {0}")]
    Plugin(String),

    /// An external command failed.
    #[error("command failed: {0}")]
    Command(String),
}

impl Error {
    /// Build a refusal. The message is what the caller sees, verbatim.
    pub fn refused(gate: corpus_store::refusal::Gate, message: impl Into<String>) -> Self {
        Self::Refused { gate, message: message.into() }
    }

    /// Which gate this error belongs to, for the refusal log.
    ///
    /// Everything that is not an explicit `Refused` still has an honest
    /// answer: bad arguments are `Args`, a dead plugin or a failed command
    /// is `Harness`. Classifying here rather than at each call site keeps
    /// the log total — every error that reaches dispatch gets a line,
    /// including the ones nobody thought to categorize.
    pub fn gate(&self) -> corpus_store::refusal::Gate {
        use corpus_store::refusal::Gate;
        match self {
            Self::Refused { gate, .. } => *gate,
            Self::Args(_) | Self::Json(_) => Gate::Args,
            Self::Scope(_) => Gate::Scope,
            Self::Io(_) | Self::Plugin(_) | Self::Command(_) => Gate::Harness,
        }
    }
}

/// Result alias for tool handlers.
pub type Result<T> = std::result::Result<T, Error>;
