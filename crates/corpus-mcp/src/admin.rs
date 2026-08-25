//! Scoped adapter from the research MCP context into the narrow admin library.

pub use corpus_admin::{catalog, scoped_catalog, ADMIN_TOOLS, DESTRUCTIVE_OPS};

use serde_json::Value;

use crate::error::Result;
use crate::tools::Ctx;

pub fn dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    let origin = if name == "mission_launch" {
        match &ctx.run_origin {
            Ok(origin) => origin.as_ref(),
            Err(why) => {
                return Err(crate::error::Error::refused(
                    corpus_core::refusal::Gate::Identity,
                    format!("refusing mission_launch: run origin is unresolved — {why}"),
                ));
            }
        }
    } else {
        None
    };
    let mut admin = corpus_admin::Ctx {
        store: &ctx.store,
        pending_confirms: &mut ctx.pending_confirms,
    };
    corpus_admin::dispatch_with_origin(&mut admin, name, args, origin)
}
