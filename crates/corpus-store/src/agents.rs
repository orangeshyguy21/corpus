//! Project-scoped agents and their enforced role policy.
//!
//! An agent is a directory `store/projects/<p>/agents/<slug>/` wrapping an
//! opencode config:
//!
//! ```text
//! agent.yaml    corpus metadata sidecar (name, created, cloned_from)
//! opencode.json THE config — a schema-valid opencode document: $schema +
//!               "agent" map (primary [+ subagents]); model/description/
//!               prompt/permission/temperature per entry
//! prompts/*.md optional prompt bodies resolved by `{file:}` refs
//! ```
//!
//! `opencode.json` is consumed by opencode as-is (unknown top-level keys are
//! rejected by opencode's schema, so the document stays clean); corpus
//! metadata lives in the `agent.yaml` sidecar, never in the JSON.
//!
//! The renderer materializes a project's agents into the PROJECT's own
//! `.opencode/agent/<name>.md` (inside its run directory,
//! `store/projects/<p>/var/run/` — see `Store::provision_run_dir`) — one
//! file per `agent` map entry, frontmatter carrying description/mode/
//! model/temperature/permission and a body of the prompt with `{file:}`
//! refs inlined from the agent dir. The dir is corpus-managed: a launch
//! first clears the previous generated set, then renders EVERY agent of
//! the launched project, so the agent list opencode shows is scoped to
//! the project (and subagent names stay bare so the primary's `task:`
//! permission keys match verbatim). Every render BINDS the agent to its
//! project: `store/projects/*` permission patterns are rewritten to the
//! concrete project, wildcard read-allows gain the corpus boundary, the
//! trust red lines (`benchmarks/**`, `plugins/**` read denies) are
//! injected unconditionally, and a Corpus scope section names the exact
//! corpus dir — agents stay in their own project's corpus.

mod model;
mod mutations;
mod permissions;
mod rendering;
mod repository;
mod roles;
mod validation;

pub use model::{
    AddSubagentRequest, AgentConfig, AgentSidecar, CreateAgentRequest, RoleMigration, SourcePin,
};
pub use rendering::primary_handles;
pub use roles::{infer_role, AgentRole, CORPUS_TOOLS, CURATOR_TOOLS, SUPER_ADMIN_TOOLS};

/// The OpenCode config schema reference written into generated agent documents.
pub const OPENCODE_SCHEMA: &str = "https://opencode.ai/config.json";

/// The placeholder display name a freshly created agent carries until the
/// operator renames it. Deliberately not the slug, so the UI shows it as an
/// editable label.
pub const DEFAULT_AGENT_NAME: &str = "new agent";

#[cfg(test)]
#[path = "agents/tests/mod.rs"]
mod tests;
