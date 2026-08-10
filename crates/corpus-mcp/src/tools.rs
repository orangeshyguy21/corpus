//! Tool implementations: each speaks the corpus plugin protocol. The MCP
//! server adds no new powers — it exposes the plugin's sandbox, targets,
//! oracles, and faucet with server-side enforcement (caps, verification
//! gates) that no prompt can talk its way around.

use std::path::PathBuf;

use corpus_core::{FaucetCall, Plugin};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Output cap fed back to the model.
const OUTPUT_CAP_BYTES: usize = 8 * 1024;

/// Shared server state. Corpus policy (session budget) lives here; the
/// environment policy (per-payment cap, sandbox, regtest-only) lives in
/// the plugin.
#[derive(Debug)]
pub struct Ctx {
    /// The environment plugin driving the harness.
    pub plugin: Plugin,
    /// Corpus store root (findings/, attacks/).
    pub store: PathBuf,
    /// Faucet spend within this server session (sats).
    pub faucet_spent_sats: u64,
    /// Per-session faucet budget.
    pub faucet_budget_sats: u64,
}

impl Ctx {
    /// Resolve from the environment.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let plugin_dir = std::env::var("CORPUS_PLUGIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("{home}/Sites/corpus/plugins/cdk-regtest")));
        let store = std::env::var("CORPUS_STORE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("{home}/Sites/corpus/store")));
        let plugin = Plugin::spawn(&plugin_dir)?;
        Ok(Self {
            plugin,
            store,
            faucet_spent_sats: 0,
            faucet_budget_sats: 1_000_000,
        })
    }
}

/// The tool catalog advertised in tools/list.
pub fn catalog() -> Value {
    json!([
        {
            "name": "target_info",
            "description": "Targets you may attack (sandbox-scoped mint URLs), available tools, and faucet limits. Call this first.",
            "inputSchema": {"type": "object", "properties": {}, "required": []}
        },
        {
            "name": "sandbox_exec",
            "description": "Execute a bash command inside the egress-denied sandbox. The sandbox has curl, jq, sqlite3, and cdk-cli at /opt/tools/cdk-cli. 120s timeout, output capped at 8KB.",
            "inputSchema": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"]
            }
        },
        {
            "name": "oracle_run",
            "description": "Run a host-side invariant oracle by name (e.g. 020-conservation). Returns verdict: hold | violated | inconclusive, with evidence log.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            }
        },
        {
            "name": "faucet",
            "description": "Regtest Lightning faucet. op=pay {invoice} pays a mint quote; op=invoice {amount_sat} creates an invoice you can melt into; op=balance. Regtest-only, per-payment cap, per-session budget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "op": {"type": "string", "enum": ["pay", "invoice", "balance"]},
                    "invoice": {"type": "string"},
                    "amount_sat": {"type": "integer"},
                    "memo": {"type": "string"}
                },
                "required": ["op"]
            }
        },
        {
            "name": "wallet_fund",
            "description": "Fund a sandbox wallet in ONE step: creates a mint quote with cdk-cli, pays it via the faucet, claims the proofs, returns the balance. Mechanical funding is harness business — spend your steps on the attack.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "work_dir": {"type": "string", "description": "wallet dir under /tmp, e.g. /tmp/w1"},
                    "amount_sat": {"type": "integer"},
                    "target": {"type": "integer", "description": "0 = first target (default), 1 = second"}
                },
                "required": ["work_dir", "amount_sat"]
            }
        },
        {
            "name": "finding_write",
            "description": "Record a security finding in the corpus. GATED: the oracle suite runs server-side first; findings without an oracle violation are marked unverified. Only call with a demonstrated PoC.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "severity": {"type": "string", "enum": ["low", "medium", "high", "critical"]},
                    "detail": {"type": "string"}
                },
                "required": ["title", "severity", "detail"]
            }
        },
        {
            "name": "attack_save",
            "description": "Save a reusable attack artifact into the corpus (attack.md + executable run.sh). Attacks are regression probes and benchmark cases.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "script": {"type": "string"}
                },
                "required": ["name", "description", "script"]
            }
        }
    ])
}

/// Dispatch a tools/call.
pub fn dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    match name {
        "target_info" => target_info(ctx),
        "sandbox_exec" => sandbox_exec(ctx, &require_str(args, "command")?),
        "oracle_run" => oracle_run(ctx, &require_str(args, "name")?),
        "faucet" => faucet(ctx, args),
        "wallet_fund" => wallet_fund(ctx, args),
        "finding_write" => finding_write(ctx, args),
        "attack_save" => attack_save(ctx, args),
        other => Err(Error::Args(format!("unknown tool: {other}"))),
    }
}

fn require_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Args(format!("missing string argument: {key}")))
}

fn require_u64(args: &Value, key: &str) -> Result<u64> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Args(format!("missing integer argument: {key}")))
}

fn target_info(ctx: &mut Ctx) -> Result<String> {
    let targets = ctx.plugin.targets()?;
    let tools = ctx.plugin.tools()?;
    Ok(serde_json::to_string_pretty(&json!({
        "targets": targets,
        "scope": "ONLY these URLs may be attacked; the sandbox cannot reach anything else",
        "tools_in_sandbox": tools,
        "faucet": {
            "max_payment_sats": 100000,
            "session_budget_sats": ctx.faucet_budget_sats,
            "spent_this_session": ctx.faucet_spent_sats
        },
        "funding_flow": "use the wallet_fund tool — it does quote -> pay -> claim deterministically"
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

fn sandbox_exec(ctx: &mut Ctx, command: &str) -> Result<String> {
    let result = ctx.plugin.sandbox_exec(command)?;
    let mut combined = result.output;
    if combined.len() > OUTPUT_CAP_BYTES {
        combined.truncate(OUTPUT_CAP_BYTES);
        combined.push_str("\n[truncated]");
    }
    combined.push_str(&format!("\n[exit {}]", result.exit_code));
    Ok(combined)
}

fn oracle_run(ctx: &mut Ctx, name: &str) -> Result<String> {
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(Error::Args("bad oracle name".to_string()));
    }
    let result = ctx.plugin.call_oracle(name)?;
    Ok(format!("verdict: {}\n{}", result.verdict, result.log))
}

fn faucet(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let op = require_str(args, "op")?;
    let call = FaucetCall {
        invoice: args.get("invoice").and_then(Value::as_str).map(str::to_string),
        amount_sat: args.get("amount_sat").and_then(Value::as_u64),
        memo: args.get("memo").and_then(Value::as_str).map(str::to_string),
    };
    match op.as_str() {
        "pay" => {
            if ctx.faucet_spent_sats >= ctx.faucet_budget_sats {
                return Ok(format!(
                    "[faucet refused] session budget of {} sat exhausted",
                    ctx.faucet_budget_sats
                ));
            }
        }
        "invoice" | "balance" => {}
        other => return Err(Error::Args(format!("unknown faucet op: {other}"))),
    }
    let result = ctx.plugin.faucet(&op, &call)?;
    if let Some(paid) = result.paid_sats {
        ctx.faucet_spent_sats += paid;
        return Ok(format!(
            "Payment succeeded ({paid} sat). Session spend: {} sat.",
            ctx.faucet_spent_sats
        ));
    }
    if op == "invoice" {
        if let Some(inv) = result.text.lines().find(|l| l.starts_with("lnbcrt")) {
            return Ok(format!("Invoice created: {inv}"));
        }
    }
    Ok(result.text)
}

/// Fund a sandbox wallet: quote -> faucet pay -> claim -> balance.
/// Deterministic harness work; saves the model fifteen fragile steps.
fn wallet_fund(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let work_dir = require_str(args, "work_dir")?;
    if !work_dir.starts_with("/tmp/")
        || !work_dir
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    {
        return Err(Error::Args("work_dir must be a simple path under /tmp/".to_string()));
    }
    let amount = require_u64(args, "amount_sat")?;
    let targets = ctx.plugin.targets()?;
    let target = if targets.len() > 1 {
        match args.get("target").and_then(Value::as_u64).unwrap_or(0) {
            0 => targets[0].clone(),
            _ => targets[1].clone(),
        }
    } else {
        targets.first().cloned().unwrap_or_default()
    };
    let cli = format!("/opt/tools/cdk-cli -n -w {work_dir}");

    // 1. Create the quote; cdk-cli prints the invoice then waits out its
    //    --wait-duration and returns — that timeout is expected.
    let quote_out = sandbox_exec(
        ctx,
        &format!("rm -rf {work_dir} && {cli} mint {target} {amount} --wait-duration 2"),
    )?;
    // BOLT11 HRP is "lnbcrt" + amount + "1" separator — match on "lnbcrt",
    // the amount sits between the currency code and the separator.
    let invoice = quote_out
        .split_whitespace()
        .find(|tok| tok.starts_with("lnbcrt"))
        .map(str::to_string)
        .ok_or_else(|| Error::Command(format!("no invoice in mint output: {quote_out}")))?;
    // cdk-cli prints "Quote: id=<uuid>, state=UNPAID, ..." — the claim
    // path is re-running mint with that quote id (`mint-pending` checks
    // pending *proofs*, not quotes).
    let quote_id = quote_out
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix("id="))
        .map(|id| id.trim_end_matches(',').to_string())
        .ok_or_else(|| Error::Command(format!("no quote id in mint output: {quote_out}")))?;

    // 2. Pay it via the faucet (session budget enforced in `faucet`).
    let pay_out = faucet(ctx, &json!({"op": "pay", "invoice": invoice}))?;
    if !pay_out.starts_with("Payment succeeded") {
        return Ok(format!("funding failed at payment: {pay_out}"));
    }

    // 3. Claim by quote id (the mint may need a moment to mark the quote
    //    PAID; --wait-duration lets the claim ride out that lag), then
    //    read the balance.
    let claim_out = sandbox_exec(
        ctx,
        &format!("{cli} mint {target} -q {quote_id} --wait-duration 15"),
    )?;
    let balance = sandbox_exec(ctx, &format!("{cli} balance"))?;
    Ok(format!(
        "wallet funded: {amount} sat on {target}\n{claim_out}\nbalance: {balance}"
    ))
}

/// The verification gate: the plugin's oracle suite runs server-side
/// before any finding is written; the verdict is recorded on the finding.
/// Works for ANY plugin via `oracles()` + `call_oracle()`.
fn finding_write(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let title = require_str(args, "title")?;
    let severity = require_str(args, "severity")?;
    let detail = require_str(args, "detail")?;

    let mut verified = false;
    let mut oracle_out = String::new();
    match ctx.plugin.oracles() {
        Ok(oracles) => {
            for oracle in &oracles {
                let line = match ctx.plugin.call_oracle(&oracle.name) {
                    Ok(result) => {
                        if result.verdict == "violated" {
                            verified = true;
                        }
                        format!("  {:<36} {}\n", oracle.name, result.verdict)
                    }
                    Err(error) => {
                        format!("  {:<36} ERROR ({error})\n", oracle.name)
                    }
                };
                oracle_out.push_str(&line);
            }
            oracle_out.push_str(&format!(
                "oracles: {} run, verified={verified}\n",
                oracles.len()
            ));
        }
        Err(error) => {
            oracle_out = format!("oracle suite failed to run: {error}\n");
        }
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let slug = slugify(&title);
    let dir = ctx.store.join("findings");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{ts}-{slug}.md"));
    let body = format!(
        "---\ntitle: {title}\nseverity: {severity}\noracle_verified: {verified}\ntimestamp: {ts}\n---\n\n\
         ## Detail\n\n{detail}\n\n## Oracle output at report time\n\n```\n{oracle_out}```\n"
    );
    std::fs::write(&path, &body)?;
    Ok(format!(
        "finding recorded: {} (oracle_verified: {verified})",
        path.display()
    ))
}

fn attack_save(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let name = require_str(args, "name")?;
    let description = require_str(args, "description")?;
    let script = require_str(args, "script")?;
    let slug = slugify(&name);
    if slug.is_empty() {
        return Err(Error::Args("name must contain alphanumerics".to_string()));
    }
    let dir = ctx.store.join("attacks").join(&slug);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("attack.md"), format!("# {name}\n\n{description}\n"))?;
    let run_path = dir.join("run.sh");
    std::fs::write(&run_path, &script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run_path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(format!("attack saved: {}", dir.display()))
}

fn slugify(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}