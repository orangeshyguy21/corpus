//! Tool implementations: each speaks the corpus plugin protocol. The MCP
//! server adds no new powers — it exposes the plugin's sandbox, targets,
//! oracles, and faucet with server-side enforcement (caps, verification
//! gates) that no prompt can talk its way around. Write tools land in the
//! project corpus (the ONLY corpus scope).

use std::collections::HashMap;
use std::path::PathBuf;

use corpus_core::{FaucetCall, Plugin, ProbeResult, Scope, Store};
use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Output cap fed back to the model.
const OUTPUT_CAP_BYTES: usize = 8 * 1024;

/// Shared server state. Corpus policy (session budget, scoped writes,
/// verification gates) lives here; the environment policy (per-payment cap,
/// sandbox, regtest-only) lives in the plugin.
#[derive(Debug)]
pub struct Ctx {
    /// The environment plugin driving the harness.
    pub plugin: Plugin,
    /// Corpus store root (projects/, templates/).
    pub store: Store,
    /// Default write scope: which project corpus writes land in.
    pub scope: Scope,
    /// Faucet spend within this server session (sats).
    pub faucet_spent_sats: u64,
    /// Per-session faucet budget.
    pub faucet_budget_sats: u64,
    /// Probe result captured at startup. The server refuses tool calls
    /// while `ready` is false — fail loud, never silently misleading.
    pub probe_ready: bool,
    /// Probe notes explaining exactly what is wrong.
    pub probe_notes: String,
    /// When the probe last ran — re-probes while gated are rate-limited
    /// so a polling model cannot hammer docker/curl in a tight loop.
    pub last_probe: std::time::Instant,
    /// Admin profile on: the corpus-admin tool group is exposed and the
    /// probe-required gate is bypassed (admin is store-only, host-side).
    pub admin: bool,
    /// Pending destructive-op confirmations keyed by their one-shot token.
    /// Minted by a dry-run call; consumed by the token-bearing re-call.
    pub pending_confirms: HashMap<String, PendingConfirm>,
}

/// A pending destructive-op confirmation: a single-use, short-TTL token
/// minted by a dry-run call and consumed by the token-bearing re-call that
/// actually mutates the store. `key` is the token (hash of op+target+nonce).
#[derive(Debug)]
pub struct PendingConfirm {
    pub op: String,
    pub target: String,
    /// Epoch seconds at which the token expires.
    pub expires_at: u64,
}

/// Minimum interval between re-probes while the gate is closed.
const REPROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

impl Ctx {
    /// Resolve from the environment.
    pub fn from_env() -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let plugin_dir = std::env::var("CORPUS_PLUGIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("{home}/Sites/corpus/plugins/cdk-regtest")));
        let mut plugin = Plugin::spawn(&plugin_dir)?;
        // Probe the environment once at startup; the result gates every
        // tool call (version-pin mismatch included).
        let probe = plugin.probe().unwrap_or_else(|e| ProbeResult {
            ready: false,
            notes: format!("probe failed: {e}"),
        });
        Ok(Self {
            plugin,
            store: Store::from_env(),
            scope: Scope::from_env(),
            faucet_spent_sats: 0,
            faucet_budget_sats: 1_000_000,
            probe_ready: probe.ready,
            probe_notes: probe.notes,
            last_probe: std::time::Instant::now(),
            admin: false,
            pending_confirms: HashMap::new(),
        })
    }

    /// The scope for a write: the configured project scope. The `team`
    /// argument is accepted for backward-compatibility but ignored —
    /// the corpus is project-level only.
    fn write_scope(&self, _args: &Value) -> Result<Scope> {
        Ok(self.scope.clone())
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
            "description": "Record a security finding in the project corpus. GATED: the oracle suite runs server-side first; findings without an oracle violation are marked unverified. Findings default to sensitivity: embargoed. Only call with a demonstrated PoC.",
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
            "description": "Save a reusable attack artifact into the project corpus (attack.md + executable run.sh). Attacks are regression probes and benchmark cases.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "description": {"type": "string"},
                    "script": {"type": "string"}
                },
                "required": ["name", "description", "script"]
            }
        },
        {
            "name": "technique_save",
            "description": "Save a technique card into the project corpus. Working notes — no oracle gate — but run_log MUST name an existing file in the project corpus runs/. status: fired | analyzed-only | unresolved-lead. Write one after every mission, negative results included.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "status": {"type": "string", "enum": ["fired", "analyzed-only", "unresolved-lead"]},
                    "body": {"type": "string"},
                    "run_log": {"type": "string", "description": "basename of an existing file in runs/, e.g. 1786392937-operator-call-target-info.log"}
                },
                "required": ["name", "status", "body", "run_log"]
            }
        }
    ])
}

/// Dispatch a tools/call.
pub fn dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    // Fail loud: while the environment probe says not-ready (mints down,
    // version-pin mismatch, arena torn down), no tool runs. The notes ARE
    // the error so the agent sees exactly what to fix. The probe is a
    // startup snapshot — environments RECOVER (mints get restarted
    // mid-session), so while gated, re-probe (rate-limited) before
    // refusing: a closed gate must heal itself.
    if !ctx.probe_ready && ctx.last_probe.elapsed() >= REPROBE_INTERVAL {
        ctx.last_probe = std::time::Instant::now();
        match resilient(ctx, |p| p.probe()) {
            Ok(probe) => {
                ctx.probe_ready = probe.ready;
                ctx.probe_notes = probe.notes;
            }
            Err(error) => {
                ctx.probe_notes = format!("probe failed: {error}");
            }
        }
    }
    if !ctx.probe_ready {
        return Err(Error::Args(format!(
            "harness not ready — probe: {}",
            ctx.probe_notes
        )));
    }
    match name {
        "target_info" => target_info(ctx),
        "sandbox_exec" => sandbox_exec(ctx, &require_str(args, "command")?),
        "oracle_run" => oracle_run(ctx, &require_str(args, "name")?),
        "faucet" => faucet(ctx, args),
        "wallet_fund" => wallet_fund(ctx, args),
        "finding_write" => finding_write(ctx, args),
        "attack_save" => attack_save(ctx, args),
        "technique_save" => technique_save(ctx, args),
        other => Err(Error::Args(format!("unknown tool: {other}"))),
    }
}

fn require_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Args(format!("missing string argument: {key}")))
}

/// Run a plugin call; if the plugin died or wedged (timeout kills the
/// process tree), respawn it so the NEXT call recovers, and surface the
/// original error. The failed call itself is NEVER retried — the work may
/// have completed server-side (e.g. a faucet payment).
fn resilient<T>(
    ctx: &mut Ctx,
    f: impl FnOnce(&mut corpus_core::Plugin) -> std::result::Result<T, corpus_core::Error>,
) -> Result<T> {
    match f(&mut ctx.plugin) {
        Ok(value) => Ok(value),
        Err(error) => {
            if matches!(
                error,
                corpus_core::Error::PluginClosed(_) | corpus_core::Error::Plugin { .. }
            ) {
                let _ = ctx.plugin.restart();
            }
            Err(Error::Plugin(error))
        }
    }
}

fn require_u64(args: &Value, key: &str) -> Result<u64> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Args(format!("missing integer argument: {key}")))
}

fn target_info(ctx: &mut Ctx) -> Result<String> {
    let targets = resilient(ctx, |p| p.targets())?;
    let tools = resilient(ctx, |p| p.tools())?;
    let sources = resilient(ctx, |p| p.sources())?;
    Ok(serde_json::to_string_pretty(&json!({
        "targets": targets,
        "scope": "ONLY these URLs may be attacked; the sandbox cannot reach anything else",
        "sources_in_sandbox": {
            "note": "pinned upstream source, read-only at /opt/src/<name> — the research corpus (target implementation + NUT spec). This is the only sanctioned source; you have no host filesystem.",
            "mounted": sources
        },
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
    let result = resilient(ctx, |p| p.sandbox_exec(command))?;
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
    let result = resilient(ctx, |p| p.call_oracle(name))?;
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
    let result = resilient(ctx, |p| p.faucet(&op, &call))?;
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
    let targets = resilient(ctx, |p| p.targets())?;
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
/// Works for ANY plugin via `oracles()` + `call_oracle()`. The finding
/// lands in the project corpus (default class: embargoed).
fn finding_write(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let title = require_str(args, "title")?;
    let severity = require_str(args, "severity")?;
    let detail = require_str(args, "detail")?;
    let scope = ctx.write_scope(args)?;

    let mut verified = false;
    let mut oracle_out = String::new();
    match resilient(ctx, |p| p.oracles()) {
        Ok(oracles) => {
            for oracle in &oracles {
                let line = match resilient(ctx, |p| p.call_oracle(&oracle.name)) {
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
    let dir = scope.corpus_dir(&ctx.store).join("findings");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{ts}-{slug}.md"));
    let body = format!(
        "---\ntitle: {title}\nseverity: {severity}\noracle_verified: {verified}\nsensitivity: embargoed\ntimestamp: {ts}\n---\n\n\
         ## Detail\n\n{detail}\n\n## Oracle output at report time\n\n```\n{oracle_out}```\n"
    );
    std::fs::write(&path, &body)?;
    Ok(format!(
        "finding recorded in {}: {} (oracle_verified: {verified}, sensitivity: embargoed)",
        scope.project, path.display()
    ))
}

fn attack_save(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let name = require_str(args, "name")?;
    let description = require_str(args, "description")?;
    let script = require_str(args, "script")?;
    let scope = ctx.write_scope(args)?;
    let slug = slugify(&name);
    if slug.is_empty() {
        return Err(Error::Args("name must contain alphanumerics".to_string()));
    }
    let dir = scope.corpus_dir(&ctx.store).join("attacks").join(&slug);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("attack.md"),
        format!("---\nsensitivity: internal\n---\n# {name}\n\n{description}\n"),
    )?;
    let run_path = dir.join("run.sh");
    std::fs::write(&run_path, &script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&run_path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(format!(
        "attack saved in {}: {}",
        scope.project,
        dir.display()
    ))
}

/// Save a technique card into the project corpus. Working notes — no
/// oracle gate (findings remain the gated artifact) — but the card MUST
/// cite an existing run log, enforced here server-side: the project corpus
/// runs/.
fn technique_save(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let name = require_str(args, "name")?;
    let status = require_str(args, "status")?;
    let body = require_str(args, "body")?;
    let run_log = require_str(args, "run_log")?;
    let scope = ctx.write_scope(args)?;

    if !matches!(status.as_str(), "fired" | "analyzed-only" | "unresolved-lead") {
        return Err(Error::Args(
            "status must be fired | analyzed-only | unresolved-lead".to_string(),
        ));
    }
    // run_log is a basename only — never a path — and must exist.
    let run_log = run_log.trim();
    if run_log.is_empty()
        || run_log.contains('/')
        || run_log.contains('\\')
        || run_log == "."
        || run_log == ".."
    {
        return Err(Error::Args(
            "run_log must be a plain basename (no path)".to_string(),
        ));
    }
    let found_log = scope
        .runs_dirs(&ctx.store)
        .iter()
        .map(|dir| dir.join(run_log))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            Error::Args(format!(
                "run_log must name an existing file in runs/ (project corpus; not found: {run_log})"
            ))
        })?;

    let slug = slugify(&name);
    if slug.is_empty() {
        return Err(Error::Args("name must contain alphanumerics".to_string()));
    }
    let dir = scope.corpus_dir(&ctx.store).join("techniques");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{slug}.md"));
    let overwrote = path.exists();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let card = format!(
        "---\nname: {name}\nstatus: {status}\nrun_log: {run_log}\nsensitivity: internal\ntimestamp: {ts}\n---\n\n{body}\n"
    );
    std::fs::write(&path, &card)?;
    Ok(format!(
        "technique card saved in {}: {} (status: {status}, run_log: {}, overwrote existing: {overwrote})",
        scope.project,
        path.display(),
        found_log.display()
    ))
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