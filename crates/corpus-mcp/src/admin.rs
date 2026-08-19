//! Scoped adapter from the research MCP context into the narrow admin library.

pub use corpus_admin::{catalog, scoped_catalog, ADMIN_TOOLS, DESTRUCTIVE_OPS};

use serde_json::Value;

use crate::error::Result;
use crate::tools::Ctx;

pub fn dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    let mut admin = corpus_admin::Ctx {
        store: &ctx.store,
        pending_confirms: &mut ctx.pending_confirms,
    };
    corpus_admin::dispatch(&mut admin, name, args)
}
