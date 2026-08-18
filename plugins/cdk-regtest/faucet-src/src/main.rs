//! corpus-faucet — regtest Lightning faucet for cdk-regtest agents. HOST-SIDE ONLY.
//!
//! Drop-in replacement for faucet.sh: same CLI, the same stdout markers
//! (`PAID_SATS=<n>` for pay, a bare `lnbcrt…` bolt11 for invoice, a bare
//! integer for balance) and the same `faucet: error: <reason>` on stderr +
//! non-zero exit on failure, so the plugin parses it unchanged.
//!
//! Unlike the shell faucet it talks to CLN node "two" directly over its unix
//! socket via the `cln-rpc` crate — exactly how cdk's own integration tests
//! fund a wallet (crates/cdk-integration-tests/.../ln_client/cln_client.rs) —
//! with no dependency on `lightning-cli`, nix, or `timeout`.
//!
//! Safety rules (parity with faucet.sh — this is the no-mainnet guard):
//!   - regtest only: the invoice must be bcrt (lnbcrt…);
//!   - amountless invoices are refused (unbounded payment);
//!   - per-payment / per-invoice cap (CORPUS_FAUCET_MAX_SATS, default 100_000 sat).
//!
//! Environment (unchanged from faucet.sh): reads /tmp/cdk_regtest_env for
//! CDK_ITESTS_DIR and connects to $CDK_ITESTS_DIR/cln/two/regtest/lightning-rpc
//! (override the socket directly with CORPUS_CLN_RPC).

use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use cln_rpc::model::requests::{InvoiceRequest, ListfundsRequest, XpayRequest};
use cln_rpc::primitives::{Amount, AmountOrAny};
use cln_rpc::{ClnRpc, Request, Response};
use lightning_invoice::{Bolt11Invoice, Currency};

const DEFAULT_MAX_SATS: u64 = 100_000;
const ENV_FILE: &str = "/tmp/cdk_regtest_env";

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        // Match faucet.sh's `die`: single line the plugin greps for.
        eprintln!("faucet: error: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().unwrap_or_default().as_str() {
        "pay" => {
            let invoice = args.next().context("usage: corpus-faucet pay <bolt11>")?;
            cmd_pay(&invoice).await
        }
        "invoice" => {
            let amount = args
                .next()
                .context("usage: corpus-faucet invoice <amount_sat> [memo]")?;
            cmd_invoice(&amount, args.next()).await
        }
        "balance" => cmd_balance().await,
        other => bail!(
            "usage: corpus-faucet pay <bolt11> | invoice <amount_sat> [memo] | balance (got '{other}')"
        ),
    }
}

fn max_sats() -> u64 {
    std::env::var("CORPUS_FAUCET_MAX_SATS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_SATS)
}

/// Resolve the CLN "two" rpc socket: CORPUS_CLN_RPC wins, else derive it from
/// CDK_ITESTS_DIR (env, else sourced from /tmp/cdk_regtest_env).
fn rpc_path() -> Result<String> {
    if let Ok(p) = std::env::var("CORPUS_CLN_RPC") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    Ok(format!("{}/cln/two/regtest/lightning-rpc", itests_dir()?))
}

fn itests_dir() -> Result<String> {
    if let Ok(d) = std::env::var("CDK_ITESTS_DIR") {
        if !d.is_empty() {
            return Ok(d);
        }
    }
    let content = std::fs::read_to_string(ENV_FILE)
        .with_context(|| format!("regtest env not found ({ENV_FILE}); is `just regtest` running?"))?;
    for line in content.lines() {
        // lines look like: export CDK_ITESTS_DIR="/path"
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("CDK_ITESTS_DIR=") {
            let val = rest.trim().trim_matches('"').trim_matches('\'');
            if !val.is_empty() {
                return Ok(val.to_string());
            }
        }
    }
    bail!("CDK_ITESTS_DIR missing from {ENV_FILE}")
}

async fn connect() -> Result<ClnRpc> {
    let path = rpc_path()?;
    if !Path::new(&path).exists() {
        bail!("faucet node (cln two) not available at {path}");
    }
    ClnRpc::new(Path::new(&path))
        .await
        .with_context(|| format!("connect to cln rpc at {path}"))
}

async fn cmd_pay(bolt11: &str) -> Result<()> {
    // regtest-only guard (parity with faucet.sh:59-67).
    if !bolt11.to_ascii_lowercase().starts_with("lnbcrt") {
        bail!("refused: not a regtest (lnbcrt) invoice");
    }
    let invoice =
        Bolt11Invoice::from_str(bolt11).map_err(|e| anyhow!("undecodable invoice: {e}"))?;
    if invoice.currency() != Currency::Regtest {
        bail!("refused: invoice currency is not bcrt");
    }
    let amount_sats = invoice
        .amount_milli_satoshis()
        .ok_or_else(|| anyhow!("refused: amountless invoice (unbounded payment)"))?
        / 1000;
    let cap = max_sats();
    if amount_sats > cap {
        bail!("refused: {amount_sats} sat exceeds per-payment cap ({cap} sat)");
    }

    eprintln!("paying {amount_sats} sat...");
    let mut cln = connect().await?;
    // Mirror cdk's ClnClient::pay_invoice exactly (XpayRequest, all fields).
    cln.call_typed(&XpayRequest {
        invstring: bolt11.to_string(),
        amount_msat: None,
        label: None,
        dev_use_shadow: None,
        retry_for: Some(60),
        maxdelay: None,
        localinvreqid: None,
        maxfee: None,
        partial_msat: None,
        payer_note: None,
        layers: None,
    })
    .await
    .map_err(|e| anyhow!("payment failed or timed out: {e}"))?;

    // Machine-readable line the plugin parses (grep -o 'PAID_SATS=[0-9]*').
    println!("PAID_SATS={amount_sats}");
    Ok(())
}

async fn cmd_invoice(amount_str: &str, memo: Option<String>) -> Result<()> {
    let amount_sats: u64 = amount_str
        .parse()
        .map_err(|_| anyhow!("amount must be a positive integer (sat)"))?;
    if amount_sats == 0 {
        bail!("amount must be > 0");
    }
    let cap = max_sats();
    if amount_sats > cap {
        bail!("refused: {amount_sats} sat exceeds per-invoice cap ({cap} sat)");
    }
    let memo = memo.unwrap_or_else(|| "corpus faucet".to_string());
    let label = format!("corpus-{}", unique_suffix());

    let mut cln = connect().await?;
    let resp = cln
        .call(Request::Invoice(InvoiceRequest {
            amount_msat: AmountOrAny::Amount(Amount::from_sat(amount_sats)),
            description: memo,
            label,
            expiry: None,
            fallbacks: None,
            preimage: None,
            cltv: None,
            deschashonly: None,
            exposeprivatechannels: None,
        }))
        .await
        .map_err(|e| anyhow!("invoice creation failed: {e}"))?;
    match resp {
        Response::Invoice(r) => {
            println!("{}", r.bolt11);
            Ok(())
        }
        _ => bail!("unexpected cln response to invoice"),
    }
}

async fn cmd_balance() -> Result<()> {
    let mut cln = connect().await?;
    let resp = cln
        .call(Request::ListFunds(ListfundsRequest { spent: None }))
        .await
        .map_err(|e| anyhow!("listfunds failed: {e}"))?;
    match resp {
        Response::ListFunds(f) => {
            // Sum on-chain outputs + channel balances, in msat (matches faucet.sh:104).
            let mut msat: u64 = 0;
            for o in f.outputs {
                msat += o.amount_msat.msat();
            }
            for c in f.channels {
                msat += c.our_amount_msat.msat();
            }
            println!("{}", msat / 1000);
            Ok(())
        }
        _ => bail!("unexpected cln response to listfunds"),
    }
}

fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos}-{}", std::process::id())
}
