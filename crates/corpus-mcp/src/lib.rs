//! corpus-mcp: MCP server exposing the corpus harness — sandbox, oracles,
//! faucet, gated findings, and the scoped store write tools — to agents.
//!
//! The lib exposes the tool implementations so integration tests can build a
//! `Ctx` against a fixture store and echo plugin without speaking the MCP
//! wire protocol.

#![recursion_limit = "256"]

pub mod admin;
pub mod error {
    pub use corpus_admin::error::*;
}
pub mod tools;
