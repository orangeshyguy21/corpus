//! One-shot confirmation tokens for destructive admin tools.

use corpus_store::{fnv1a_hex, Store};

use crate::common::now;
use crate::error::{Error, Result};
use crate::Ctx;

/// A pending destructive-op confirmation minted by a dry-run call.
///
/// The type remains public because the scoped MCP adapter owns the map that it
/// lends to [`Ctx`]. Its contents are private so only this module can interpret
/// or consume confirmation state.
#[derive(Debug)]
pub struct PendingConfirm {
    op: String,
    target: String,
    expires_at: u64,
}

/// Short enough that an abandoned dry run cannot be replayed against a stale
/// target later.
const CONFIRM_TTL_SECS: u64 = 60;

/// Mint a one-shot confirm token for a destructive op (dry-run call), store
/// it with a short TTL, and return it with the dry-run summary.
///
/// This computes and states the consequences before anything commits and
/// requires the target to be named twice. In operator chat, a person can read
/// the dry run between the two calls. It is not authorization for autonomous
/// callers: scoped curators therefore receive no destructive tools.
pub(crate) fn mint_confirm(
    ctx: &mut Ctx<'_>,
    op: &str,
    target: &str,
    summary: &str,
) -> Result<String> {
    let nonce = format!("{}|{}", target, now());
    // Provenance-grade fingerprint, not an authentication key.
    let token = fnv1a_hex(format!("{op}|{target}|{nonce}").as_bytes());
    ctx.pending_confirms.insert(
        token.clone(),
        PendingConfirm {
            op: op.to_string(),
            target: target.to_string(),
            expires_at: now() + CONFIRM_TTL_SECS,
        },
    );
    Ok(format!(
        "{summary}\n\nconfirm_token: {token} (one-shot, {}s TTL)\n\
         Call the same op again with confirm_token to commit.",
        CONFIRM_TTL_SECS
    ))
}

/// Complete a destructive op with a matching, unexpired token.
///
/// Removing before validation makes every presented token single-use,
/// including mismatched, expired, and mutation-failing attempts.
pub(crate) fn confirm_and_run<R: std::fmt::Display>(
    ctx: &mut Ctx<'_>,
    op: &str,
    target: &str,
    token: &str,
    run: impl FnOnce(&Store) -> Result<R>,
) -> Result<String> {
    let pending = ctx.pending_confirms.remove(token).ok_or_else(|| {
        Error::Args(
            "invalid or expired confirm_token — re-run the dry-run to mint a fresh one".into(),
        )
    })?;
    if pending.op != op || pending.target != target {
        return Err(Error::Args(
            "confirm_token does not match this op+target".to_string(),
        ));
    }
    if pending.expires_at < now() {
        return Err(Error::Args(
            "confirm_token expired — re-run the dry-run".to_string(),
        ));
    }
    let result = run(ctx.store)?;
    Ok(format!("{}\n[confirmed with one-shot token]", result))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn context<'a>(store: &'a Store, confirms: &'a mut HashMap<String, PendingConfirm>) -> Ctx<'a> {
        Ctx {
            store,
            pending_confirms: confirms,
        }
    }

    fn token_from(response: &str) -> &str {
        response
            .lines()
            .find_map(|line| line.strip_prefix("confirm_token: "))
            .and_then(|line| line.split_whitespace().next())
            .expect("confirmation response token")
    }

    #[test]
    fn mint_and_confirm_preserve_output_and_single_use() {
        let store = Store::new(std::env::temp_dir().join(format!(
            "corpus-admin-confirm-output-{}",
            std::process::id()
        )));
        let mut confirms = HashMap::new();
        let mut ctx = context(&store, &mut confirms);
        let dry_run = mint_confirm(&mut ctx, "delete", "p/x", "DRY RUN").unwrap();
        assert!(dry_run.contains("(one-shot, 60s TTL)"));
        let token = token_from(&dry_run).to_string();

        let committed =
            confirm_and_run(&mut ctx, "delete", "p/x", &token, |_| Ok("deleted")).unwrap();
        assert_eq!(committed, "deleted\n[confirmed with one-shot token]");
        assert!(confirm_and_run(&mut ctx, "delete", "p/x", &token, |_| Ok("again")).is_err());
    }

    #[test]
    fn mismatch_expiry_and_failed_mutation_each_consume_the_token() {
        let store = Store::new(std::env::temp_dir().join(format!(
            "corpus-admin-confirm-consumption-{}",
            std::process::id()
        )));
        let mut confirms = HashMap::new();

        for (token, pending) in [
            (
                "mismatch",
                PendingConfirm {
                    op: "delete".into(),
                    target: "p/x".into(),
                    expires_at: now() + CONFIRM_TTL_SECS,
                },
            ),
            (
                "expired",
                PendingConfirm {
                    op: "delete".into(),
                    target: "p/x".into(),
                    expires_at: now().saturating_sub(1),
                },
            ),
            (
                "failure",
                PendingConfirm {
                    op: "delete".into(),
                    target: "p/x".into(),
                    expires_at: now() + CONFIRM_TTL_SECS,
                },
            ),
        ] {
            confirms.insert(token.into(), pending);
        }
        let mut ctx = context(&store, &mut confirms);

        assert!(confirm_and_run(&mut ctx, "delete", "p/y", "mismatch", |_| Ok("no")).is_err());
        assert!(confirm_and_run(&mut ctx, "delete", "p/x", "expired", |_| Ok("no")).is_err());
        assert!(
            confirm_and_run::<String>(&mut ctx, "delete", "p/x", "failure", |_| Err(Error::Args(
                "mutation failed".into()
            )))
            .is_err()
        );
        assert!(ctx.pending_confirms.is_empty());
    }
}
