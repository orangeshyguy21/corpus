//! Tool implementations: each speaks the corpus plugin protocol. The MCP
//! server adds no new powers — it exposes the plugin's sandbox, targets,
//! oracles, and faucet with server-side enforcement (caps, verification
//! gates) that no prompt can talk its way around. Write tools land in the
//! project corpus (the ONLY corpus scope).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use corpus_core::refusal::Gate;
use corpus_core::{
    AgentRole, FaucetCall, FindingSeverity, NewFinding, OracleInfo, OracleResult, Plugin,
    ProbeResult, Scope, Store,
};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use corpus_admin::PendingConfirm;

/// Output cap fed back to the model.
const OUTPUT_CAP_BYTES: usize = 8 * 1024;
/// Per-oracle evidence cap persisted into a finding. Seven independent
/// bounded records are more useful than one suite-wide truncation that can
/// silently erase the oracle which actually violated.
const ORACLE_LOG_CAP_BYTES: usize = 16 * 1024;
const ORACLE_SUITE_CAP: usize = 64;

fn valid_oracle_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn validate_oracle_catalog(oracles: Vec<OracleInfo>) -> Result<Vec<OracleInfo>> {
    if oracles.len() > ORACLE_SUITE_CAP {
        return Err(Error::Plugin(format!(
            "oracle catalog contains {} entries; maximum is {ORACLE_SUITE_CAP}",
            oracles.len()
        )));
    }
    let mut names = BTreeSet::new();
    for oracle in &oracles {
        if !valid_oracle_name(&oracle.name) {
            return Err(Error::Plugin(format!(
                "oracle catalog contains invalid name {:?}",
                oracle.name
            )));
        }
        if !names.insert(oracle.name.as_str()) {
            return Err(Error::Plugin(format!(
                "oracle catalog contains duplicate name {:?}",
                oracle.name
            )));
        }
    }
    Ok(oracles)
}

fn bounded_oracle_log(mut log: String) -> String {
    if log.len() <= ORACLE_LOG_CAP_BYTES {
        return log;
    }
    let mut end = ORACLE_LOG_CAP_BYTES;
    while !log.is_char_boundary(end) {
        end -= 1;
    }
    log.truncate(end);
    log.push_str("\n[evidence truncated by corpus]");
    log
}

fn validate_oracle_result(name: &str, mut result: OracleResult) -> Result<OracleResult> {
    if !matches!(
        result.verdict.as_str(),
        "hold" | "violated" | "inconclusive"
    ) {
        return Err(Error::Plugin(format!(
            "oracle {name} returned invalid verdict {:?}",
            result.verdict
        )));
    }
    result.log = bounded_oracle_log(result.log);
    Ok(result)
}

/// Shared server state. Corpus policy (session budget, scoped writes,
/// verification gates) lives here; the environment policy (per-payment cap,
/// sandbox, regtest-only) lives in the plugin.
#[derive(Debug)]
pub struct Ctx {
    /// The environment plugin driving the harness, when one could be
    /// resolved. `None` whenever resolution failed; `probe_notes` then carries
    /// the reason and every sandbox tool refuses.
    pub plugin: Option<Plugin>,
    /// Corpus store root (projects/).
    pub store: Store,
    /// The write scope: which project corpus reads and writes resolve to,
    /// or why none could be established.
    ///
    /// `Err` refuses every scoped tool with that message. Fail-closed for
    /// the same reason as `role`: this used to default to the project named
    /// `default`, so a launch that lost `CORPUS_PROJECT` wrote a whole
    /// mission's findings into another project's corpus and reported
    /// success.
    pub scope: std::result::Result<Scope, String>,
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
    /// The capability ceiling of the agent this server is serving, resolved
    /// from the run's identity (`CORPUS_OPENCODE_AGENT`) at startup.
    ///
    /// `Err` means the identity could not be established — the server then
    /// refuses every sandbox tool with that message rather than guessing.
    /// Fail-closed on purpose: a gate that is bypassed by UNSETTING a
    /// variable is not a gate. The `--role` flag is the explicit escape
    /// hatch for manual invocation.
    ///
    /// This is the ONLY capability authority; the agent's permission block
    /// is opencode's business and is never consulted here.
    pub role: std::result::Result<AgentRole, String>,
    /// Pending destructive-op confirmations keyed by their one-shot token.
    /// Minted by a dry-run call; consumed by the token-bearing re-call.
    pub pending_confirms: HashMap<String, PendingConfirm>,
    /// The mission's resolved source pins (`repo -> sha`, from
    /// CORPUS_SOURCE_PINS at launch) — forwarded to the plugin on every
    /// sandbox_exec so the sandbox mounts the recorded revs.
    pub source_pins: Option<serde_json::Map<String, Value>>,
    /// Durable v1 plugin session selected before the agent process spawned.
    pub environment_session: Option<String>,
    /// The basename of the current run's transcript in the project corpus
    /// `runs/` (from CORPUS_RUN_LOG at launch). Surfaced in `target_info`
    /// and used as the default `run_log` for `technique_save` when the
    /// agent omits it — the sandbox has no host FS and cannot enumerate
    /// `runs/`, so without this the agent must guess.
    pub run_log: Option<String>,
}

/// Minimum interval between re-probes while the gate is closed.
const REPROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

impl Ctx {
    /// Resolve from the environment.
    pub fn from_env() -> Result<Self> {
        // Everything this process changes carries the identity it was
        // launched as, so an agent built by a curator says so in its own
        // sidecar.
        let store = match std::env::var(corpus_core::AGENT_ENV) {
            Ok(agent) if !agent.trim().is_empty() => {
                Store::from_env().with_actor(format!("curator:{}", agent.trim()))
            }
            _ => Store::from_env(),
        };
        // The project gate runs FIRST: the plugin binding is a property of
        // the project, so a server that cannot say which project it serves
        // cannot say which environment it drives either.
        let scope = Scope::from_env_strict(&store);
        // A missing plugin is NOT process-fatal: management-only roles still
        // need their scoped store tools, while sandbox tools refuse through
        // the probe gate.
        let mut plugin = match resolve_plugin_dir(&store, &scope) {
            Ok(dir) => match Plugin::spawn(&dir) {
                Ok(plugin) => Ok(plugin),
                Err(e) => Err(format!("plugin at {}: {e}", dir.display())),
            },
            Err(e) => Err(e.to_string()),
        };
        let environment_session = std::env::var(corpus_core::ENVIRONMENT_SESSION_ENV)
            .ok()
            .filter(|value| !value.is_empty());
        // Probe the environment once at startup; the result gates every
        // tool call (version-pin mismatch included).
        let probe = match plugin.as_mut() {
            Ok(plugin) if plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1 => {
                let result = environment_session
                    .as_deref()
                    .ok_or_else(|| corpus_core::Error::Store("v1 plugin run has no CORPUS_ENVIRONMENT_SESSION".into()))
                    .and_then(|session| {
                        let record = store.load_environment_session_key(&plugin.manifest().name, session)?;
                        let proven = scope.as_ref().map_err(|error| corpus_core::Error::Store(error.clone()))?;
                        if record.id.project != proven.project
                            || record.state != corpus_core::EnvironmentSessionState::Ready
                            || record.plugin_digest != corpus_core::plugin_bundle_digest(plugin.dir())?
                        {
                            return Err(corpus_core::Error::Store(
                                "environment session record does not match project, ready state, or selected plugin bytes".into(),
                            ));
                        }
                        let plugin_id = plugin.manifest().name.clone();
                        let state_dir = store
                            .plugin_runtime_dir(&plugin_id)?
                            .join("state")
                            .join(session);
                        let source_cache = store.source_cache_dir();
                        let sources: Vec<Value> = plugin
                            .manifest()
                            .sources
                            .iter()
                            .filter_map(|source| {
                                record.source_shas.get(&source.id).map(|sha| {
                                    json!({
                                        "id": source.id,
                                        "sha": sha,
                                        "host_path": source_cache.join(&source.id).join(sha),
                                        "mount": source.mount,
                                    })
                                })
                            })
                            .collect();
                        let params = json!({
                            "session_id": session,
                            "state_dir": state_dir,
                            "source_cache": source_cache,
                            "sources": sources,
                            "project": record.id.project,
                            "mission": record.id.mission,
                            "run": record.id.generation,
                        });
                        plugin.hello()?;
                        plugin.call_v1("session_probe", Some(params))
                    });
                match result {
                    Ok(value) => ProbeResult {
                        ready: value.get("ready").and_then(Value::as_bool).unwrap_or(false),
                        notes: value.get("notes").and_then(Value::as_str).unwrap_or("session probed").to_string(),
                        running_version: value.get("running_version").and_then(Value::as_str).map(str::to_string),
                        expected_tag: value.get("expected_tag").and_then(Value::as_str).map(str::to_string),
                    },
                    Err(e) => ProbeResult { ready: false, notes: format!("session probe failed: {e}"), running_version: None, expected_tag: None },
                }
            }
            Ok(plugin) => plugin.probe().unwrap_or_else(|e| ProbeResult {
                ready: false,
                notes: format!("probe failed: {e}"),
                running_version: None,
                expected_tag: None,
            }),
            Err(why) => ProbeResult {
                ready: false,
                notes: why.clone(),
                running_version: None,
                expected_tag: None,
            },
        };
        let source_pins = std::env::var(corpus_core::SOURCE_PINS_ENV)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| v.as_object().cloned());
        let run_log = std::env::var(corpus_core::RUN_LOG_ENV).ok().filter(|s| !s.is_empty());
        // The role is resolved against a PROVEN scope: an agent name is only
        // meaningful inside a project, so a scope failure is a role failure.
        let role = scope
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|scope| resolve_role(&store, scope));
        if let Err(why) = &scope {
            // Loud, because every scoped tool is about to be refused.
            eprintln!("corpus-mcp: no project scope resolved — {why}");
        }
        if let Err(why) = &role {
            eprintln!("corpus-mcp: no agent role resolved — {why}");
        }
        if let Err(why) = &plugin {
            eprintln!("corpus-mcp: no environment plugin — {why}");
        }
        Ok(Self {
            plugin: plugin.ok(),
            store,
            scope,
            faucet_spent_sats: 0,
            faucet_budget_sats: 1_000_000,
            probe_ready: probe.ready,
            probe_notes: probe.notes,
            last_probe: std::time::Instant::now(),
            role,
            pending_confirms: HashMap::new(),
            source_pins,
            environment_session,
            run_log,
        })
    }

    /// A Ctx for tests: probe pre-cleared, no environment read, an explicit
    /// role. Not `#[cfg(test)]` because the integration tests in `tests/`
    /// are separate crates; it exists so adding a field here doesn't force
    /// every one of them to restate the whole struct.
    pub fn for_test(plugin: Plugin, store: Store, scope: Scope, role: AgentRole) -> Self {
        Self {
            plugin: Some(plugin),
            store,
            scope: Ok(scope),
            faucet_spent_sats: 0,
            faucet_budget_sats: 1_000_000,
            probe_ready: true,
            probe_notes: String::new(),
            last_probe: std::time::Instant::now(),
            role: Ok(role),
            pending_confirms: HashMap::new(),
            source_pins: None,
            environment_session: None,
            run_log: None,
        }
    }

    /// The scope every scoped tool resolves through — the ONE choke point,
    /// so a server with no proven project refuses uniformly and early,
    /// instead of each tool inventing a fallback and failing later with an
    /// unrelated message. The `team` argument is accepted for
    /// backward-compatibility but ignored: the corpus is project-level only.
    ///
    /// The project must still EXIST at call time: it is checked at startup,
    /// but a project can be deleted under a live server.
    fn write_scope(&self, _args: &Value) -> Result<Scope> {
        let scope = self.scope.clone().map_err(Error::Scope)?;
        if !self.store.project_dir(&scope.project).join("project.yaml").is_file() {
            return Err(Error::Scope(format!(
                "project {:?} does not exist under {} — projects come into being deliberately, \
                 never as a side effect of a tool call",
                scope.project,
                self.store.root().display()
            )));
        }
        Ok(scope)
    }
}

/// The corpus category directory for a write. Created on demand — a
/// freshly wiped project has no category dirs — but only ever INSIDE a
/// project `write_scope` already proved exists, so a `create_dir_all` here
/// can no longer conjure a whole corpus tree for a mis-scoped server.
fn category_dir(ctx: &Ctx, scope: &Scope, category: &str) -> Result<PathBuf> {
    let dir = scope.corpus_dir(&ctx.store).join(category);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The plugin directory this server drives. `CORPUS_PLUGIN_DIR` is the
/// explicit hand-run override; otherwise the binding comes from the
/// PROJECT record, which is where it is declared. A sole plugin in the
/// plugins dir is accepted as the last resort so the store-only admin
/// profile — which has no project scope by design — still starts.
fn resolve_plugin_dir(
    store: &Store,
    scope: &std::result::Result<Scope, String>,
) -> Result<PathBuf> {
    if let Some(dir) = std::env::var("CORPUS_PLUGIN_DIR").ok().filter(|s| !s.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    if let Ok(scope) = scope {
        let project = corpus_core::Project::load(store, &scope.project)
            .map_err(|e| Error::Scope(format!("project {:?}: {e}", scope.project)))?;
        return corpus_core::find_plugin(&project.plugin)
            .map_err(|error| Error::Scope(error.to_string()))?
            .map(|plugin| plugin.dir)
            .ok_or_else(|| Error::Scope(format!("plugin {:?} is not installed", project.plugin)));
    }
    let mut found = corpus_core::plugin_catalog().unwrap_or_default();
    match found.len() {
        1 => Ok(found.remove(0).dir),
        _ => Err(Error::Scope(format!(
            "cannot resolve a plugin: {} names no project, and {} holds {} plugins — set \
             CORPUS_PLUGIN_DIR to run this server by hand",
            corpus_core::PROJECT_ENV,
            "the effective plugin catalog",
            found.len()
        ))),
    }
}

/// Resolve the run's capability ceiling from its identity, against a scope
/// that is ALREADY proven. Every failure is distinct: debugging a blanket
/// deny-all across three different causes is otherwise miserable.
///
/// An explicit `--role <name>` argv flag overrides the agent lookup — the
/// escape hatch for invoking the server by hand. It is no more secure than
/// the env var (anything that can spawn this binary can pass it), and it is
/// deliberately checked HERE, after the project gate: it used to return
/// first, so `--role super` with no `CORPUS_PROJECT` yielded a
/// full-capability server writing into whatever project happened to be the
/// default.
fn resolve_role(store: &Store, scope: &Scope) -> std::result::Result<AgentRole, String> {
    let argv: Vec<String> = std::env::args().collect();
    if let Some(pos) = argv.iter().position(|a| a == "--role") {
        let raw = argv.get(pos + 1).ok_or("--role needs a value")?;
        return AgentRole::parse(raw)
            .ok_or_else(|| {
                format!("--role {raw:?} is not one of {}", AgentRole::names())
            });
    }
    let agent = std::env::var(corpus_core::AGENT_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "{} is unset — a mission launch always sets it; pass --role <{}> \
                 to run this server by hand",
                corpus_core::AGENT_ENV,
                AgentRole::names()
            )
        })?;
    let config = store.load_agent(&scope.project, &agent).map_err(|e| {
        format!(
            "agent {:?} not found in project {:?} ({e}) — the launched identity must name an agent \
             directory under store/projects/<project>/agents/",
            agent, scope.project
        )
    })?;
    // An agent that predates roles reads as the safest role, never as a
    // permissive default.
    Ok(config.meta.role())
}

/// The catalog as advertised to THIS run: the full sandbox catalog minus
/// anything the resolved role cannot call. One server serves one identity,
/// so filtering here is safe — and it stops a researcher burning turns on
/// tools it will be refused, and keeps attack-relevant tool descriptions
/// out of a low-trust agent's context. An unresolved role advertises
/// nothing, matching the deny-all `dispatch` applies.
pub fn catalog_for(role: &std::result::Result<AgentRole, String>) -> Value {
    let Ok(role) = role else {
        return Value::Array(Vec::new());
    };
    // Sandbox and management are separate namespaces, but Super receives
    // both. Curator naturally reduces to only the scoped management half.
    let mut out = catalog();
    if let Some(list) = out.as_array_mut() {
        list.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .map(|n| role.allows(n))
                .unwrap_or(false)
        });
        if let Some(admin) = crate::admin::scoped_catalog(role.admin_tools()).as_array() {
            list.extend(admin.iter().cloned());
        }
    }
    out
}

/// The tool catalog advertised in tools/list.
pub fn catalog() -> Value {
    json!([
        {
            "name": "target_info",
            "description": "Typed targets, pinned sources, sandbox tools, limits, and provenance for this environment session. Call this first.",
            "inputSchema": {"type": "object", "properties": {}, "required": []}
        },
        {
            "name": "sandbox_exec",
            "description": "Execute a bounded command inside this environment session's egress-denied sandbox. Call target_info first for the typed tools and source mounts available in this plugin.",
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
                    "memo": {"type": "string"},
                    "idempotency_key": {"type": "string", "description": "required for pay/invoice; reuse exactly when retrying the same intended mutation"}
                },
                "required": ["op"]
            }
        },
        {
            "name": "wallet_fund",
            "description": "Ask the environment plugin to fund a session-scoped attacker wallet in one bounded, idempotent operation. Availability and limits are reported by target_info.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "work_dir": {"type": "string", "description": "optional plugin-defined sandbox wallet directory"},
                    "amount_sat": {"type": "integer"},
                    "target_id": {"type": "string", "description": "typed target id returned by target_info; omit for the plugin default"},
                    "idempotency_key": {"type": "string", "description": "unique per intended funding; reuse exactly on retry"}
                },
                "required": ["amount_sat", "idempotency_key"]
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
                    "detail": {"type": "string"},
                    "path": {"type": "string", "description": "Optional path beneath findings/. A .md path names the file; a path with no extension or a trailing slash names a containing folder. Existing files are never overwritten."},
                    "metadata": {"type": "object", "description": "Optional project-defined frontmatter. Corpus-owned keys such as title, severity, timestamp, sensitivity, verification, and provenance are refused."}
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
            "description": "Save a technique card into the project corpus. Working notes — no oracle gate — but run_log MUST name an existing file in the project corpus runs/. status: fired | analyzed-only | unresolved-lead. Write one after every mission, negative results included. Omit run_log to default to this mission's transcript (returned by target_info as run_log).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "status": {"type": "string", "enum": ["fired", "analyzed-only", "unresolved-lead"]},
                    "body": {"type": "string"},
                    "run_log": {"type": "string", "description": "basename of an existing file in runs/. Omit to default to this mission's run_log (from target_info)."}
                },
                "required": ["name", "status", "body"]
            }
        }
    ])
}

/// What a role grants, for a refusal message. A role holding no corpus
/// tools used to render `(allowed: )` — a blank that reads as a bug in the
/// server rather than as an answer about the agent.
fn grants_line(role: AgentRole) -> String {
    let corpus: Vec<&str> = role
        .tools()
        .iter()
        .map(|t| t.trim_start_matches("corpus_"))
        .collect();
    let admin = role.admin_tools();
    match (corpus.is_empty(), admin.is_empty()) {
        (true, true) => "nothing".to_string(),
        (false, true) => format!("sandbox tools: {}", corpus.join(", ")),
        (true, false) => format!("project-management tools: {}", admin.join(", ")),
        (false, false) => format!(
            "sandbox tools: {}; project-management tools: {}",
            corpus.join(", "),
            admin.join(", ")
        ),
    }
}

/// `args` with `project` overwritten by the proven scope. Clones rather
/// than mutating in place so a caller-supplied value never reaches a
/// handler: the 17 sites in admin.rs that read `project` all see the same
/// unforgeable answer, and scoping is enforced in ONE place instead of
/// seventeen.
fn with_project(args: &Value, project: &str) -> Value {
    let mut map = args.as_object().cloned().unwrap_or_default();
    map.insert("project".to_string(), Value::String(project.to_string()));
    Value::Object(map)
}

/// Management tools that only look. Not audited: the log is a record of
/// acts, and a line per `agent_list` would bury the ones that matter.
const READ_ONLY_MANAGEMENT: [&str; 11] = [
    "agent_list",
    "agent_get",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_await",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "finding_list",
    "model_list",
];

/// What a call acted on, in the project's own terms.
fn audit_target(name: &str, args: &Value) -> String {
    let arg = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or("?");
    match name {
        n if n.starts_with("agent_") => format!("agents/{}", arg("agent")),
        n if n.starts_with("mission_") => format!("missions/{}", arg("mission")),
        "entry_delete" => format!("corpus/{}", arg("path")),
        "entry_move" => format!("corpus/{} -> {}", arg("from"), arg("to")),
        "entry_write" => format!("corpus/{}", arg("path")),
        other => other.to_string(),
    }
}

/// The arguments, minus the injected project, as the intent line's detail.
/// Truncated: a whole `agent_save` document would drown the log, and the
/// resulting agent is readable from the store anyway.
fn summarize_args(args: &Value) -> String {
    let mut summary = args.clone();
    if let Some(map) = summary.as_object_mut() {
        map.remove("project");
    }
    let mut text = summary.to_string();
    if text.len() > 400 {
        text.truncate(400);
        text.push('…');
    }
    text
}

/// Tools that change the project's agent set, after which the set may no
/// longer be closed under delegation.
const AGENT_MUTATORS: [&str; 8] = [
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_set",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "agent_delete",
];

/// A curator manages ordinary project agents but cannot mint or repurpose a
/// `super` identity. Super agents can reach every research capability, so
/// authoring one is reserved to an existing Super or the host operator.
fn enforce_curator_agent_ceiling(
    ctx: &Ctx,
    name: &str,
    args: &Value,
    project: &str,
) -> Result<()> {
    let requested_role = args.get("role").and_then(Value::as_str);
    if requested_role == Some(AgentRole::Super.as_str()) {
        return Err(Error::refused(
            Gate::Role,
            format!(
                "refusing {name}: granting the super role is operator-owned; a curator may grant researcher, tester, or curator"
            ),
        ));
    }

    if name == "agent_new"
        && args.get("from").and_then(Value::as_str).is_some()
        && requested_role.is_none()
    {
        return Err(Error::refused(
            Gate::Role,
            "refusing agent_new: super authority is operator-owned; a curator cloning configuration with 'from' must choose an explicit non-super role so inherited permissions cannot infer super",
        ));
    }

    if name == "agent_clone" {
        let from = args.get("from").and_then(Value::as_str).unwrap_or_default();
        if !from.is_empty()
            && ctx
                .store
                .load_agent(project, from)
                .is_ok_and(|agent| agent.meta.role() == AgentRole::Super)
        {
            return Err(Error::refused(
                Gate::Role,
                format!(
                    "refusing agent_clone: {project}/{from} is a super agent, and copying that capability is operator-owned"
                ),
            ));
        }
    }

    let mutates_existing = matches!(
        name,
        "agent_save"
            | "agent_set"
            | "agent_set_role"
            | "agent_set_permission"
            | "agent_subagent_add"
            | "agent_subagent_remove"
    );
    if mutates_existing {
        let agent = args.get("agent").and_then(Value::as_str).unwrap_or_default();
        if !agent.is_empty()
            && ctx
                .store
                .load_agent(project, agent)
                .is_ok_and(|config| config.meta.role() == AgentRole::Super)
        {
            return Err(Error::refused(
                Gate::Role,
                format!(
                    "refusing {name}: {project}/{agent} is a super agent; editing or downgrading it is operator-owned"
                ),
            ));
        }
    }
    Ok(())
}

/// Project-management tools served to an IN-PROJECT agent.
///
/// Three things separate this from the `corpus-admin-mcp` operator
/// profile, which is untouched:
///   1. the ROLE decides the catalog, not an argv flag;
///   2. the project is INJECTED from the proven scope and never read from
///      the caller;
///   3. the agent set is re-checked afterwards, so an edit that breaks
///      delegation is reported now rather than at the next launch.
fn scoped_management_dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    let role = match &ctx.role {
        Err(why) => {
            return Err(Error::refused(
                Gate::Identity,
                format!("refusing {name}: this run has no resolved agent role — {why}"),
            ));
        }
        Ok(role) => *role,
    };
    if !role.admin_tools().contains(&name) {
        // Deliberately NOT "unknown tool": the tool exists, this role does
        // not hold it. Reporting a permissions problem as a typo sends a
        // model hunting for a spelling it will never find.
        return Err(Error::refused(
            Gate::Role,
            format!(
                "refusing {name}: {name} manages a project, and agent role {:?} does not. \
                 This role grants {}.",
                role.as_str(),
                grants_line(role)
            ),
        ));
    }
    let scope = ctx.write_scope(args)?;
    let args = with_project(args, &scope.project);

    // Read-only tools are not worth a line each — the log is for acts, and
    // burying them in `agent_list` calls would make it unreadable.
    let mutating = !READ_ONLY_MANAGEMENT.contains(&name);
    // ONE source for who is acting: the store already carries it, and it
    // is what stamps the sidecars — a second derivation here could
    // disagree with the files it is supposed to explain.
    let actor = ctx.store.actor().to_string();
    let target = audit_target(name, &args);
    if mutating {
        // Intent BEFORE the act, and a failure to record REFUSES it. The
        // whole case for trusting this role is that its acts are visible
        // afterwards; an unwritable log costs it its powers, not its
        // accountability.
        corpus_core::audit::append(
            &ctx.store,
            &scope.project,
            &corpus_core::audit::AuditRecord::new(
                &actor,
                name,
                &target,
                corpus_core::audit::Outcome::Intent,
                summarize_args(&args),
            ),
        )
        .map_err(|e| {
            Error::Args(format!(
                "refusing {name}: it cannot be recorded, and an act this role cannot account \
                 for does not happen — {e}"
            ))
        })?;
    }

    let ceiling = if role == AgentRole::Curator {
        enforce_curator_agent_ceiling(ctx, name, &args, &scope.project)
    } else {
        Ok(())
    };
    let result = ceiling.and_then(|()| crate::admin::dispatch(ctx, name, &args));
    if mutating {
        let (outcome, detail) = match &result {
            Ok(text) => (corpus_core::audit::Outcome::Ok, text.clone()),
            Err(error) => (corpus_core::audit::Outcome::Refused, error.to_string()),
        };
        // Best-effort: the intent line is already down, so the act is
        // visible either way, and failing here would refuse a call that has
        // already happened.
        let _ = corpus_core::audit::append(
            &ctx.store,
            &scope.project,
            &corpus_core::audit::AuditRecord::new(&actor, name, &target, outcome, detail),
        );
    }
    let mut out = result?;
    if AGENT_MUTATORS.contains(&name) {
        if let Err(why) = ctx.store.check_project_delegation(&scope.project) {
            // A warning, not an error: the write already landed, and undoing
            // it would be a second act nobody asked for. But an agent set
            // that is not closed under delegation makes the NEXT launch
            // refuse to render the whole project, so saying nothing here
            // means finding out at the worst moment.
            out.push_str(&format!(
                "\n\n[warning] this project's agent set is no longer closed under delegation, \
                 so the next launch will refuse to render it: {why}"
            ));
        }
    }
    Ok(out)
}

/// Dispatch a tools/call, and record every refusal.
///
/// The recording lives HERE, at the one door every tool call passes
/// through, rather than at each gate. A per-gate call would log the
/// refusals someone remembered to instrument, and the interesting refusal
/// is always the one nobody anticipated — so the log would be silent
/// exactly where it is needed. Wrapping the door instead makes it total:
/// every `Err` that reaches a caller leaves a line, including
/// `unknown tool` and anything a future gate adds without touching this
/// function.
///
/// Recording is best-effort by construction (`refusal::record` returns
/// `()`), so the result is returned unchanged whatever the log does. An
/// observer that can alter what it observes is worse than no observer.
pub fn dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    let result = dispatch_inner(ctx, name, args);
    if let Err(error) = &result {
        let mut entry =
            corpus_core::refusal::RefusalRecord::new(name, error.gate(), error.to_string());
        entry.actor = ctx.store.actor().to_string();
        // Absent when resolving the role is what failed: `Gate::Identity`
        // says so, and inventing one would claim the run had a ceiling.
        entry.role = ctx.role.as_ref().ok().map(|r| r.as_str().to_string());
        entry.args = summarize_args(args);
        entry.run_log = ctx.run_log.clone();
        let project = ctx.scope.as_ref().ok().map(|s| s.project.clone());
        corpus_core::refusal::record(&ctx.store, project.as_deref(), &entry);
    }
    result
}

/// The dispatch proper. Split from [`dispatch`] only so that every exit
/// path from it is observed.
fn dispatch_inner(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    // Project-management tools route FIRST — ahead of the corpus-tool gate
    // and ahead of the probe. A curator's work is entirely store-side, so a
    // dead mint or an absent plugin must not stop it from fixing the very
    // project whose configuration is broken.
    if crate::admin::ADMIN_TOOLS.contains(&name) {
        return scoped_management_dispatch(ctx, name, args);
    }
    // The ROLE gate runs first — before the probe — so a refused call never
    // drives a docker/curl re-probe, and so an agent outside its ceiling
    // gets the same answer whether or not the arena happens to be healthy.
    // This is the authority: the agent's opencode permission block is
    // opencode's to enforce and is never consulted here.
    //
    // Only KNOWN tools are judged here. An unrecognized name falls through
    // to the match below and reports "unknown tool" — a typo must not be
    // explained as a permissions problem.
    let known = corpus_core::CORPUS_TOOLS
        .iter()
        .any(|t| t.trim_start_matches("corpus_") == name);
    if known {
        match &ctx.role {
            Err(why) => {
                return Err(Error::refused(
                    Gate::Identity,
                    format!("refusing {name}: this run has no resolved agent role — {why}"),
                ));
            }
            Ok(role) if !role.allows(name) => {
                return Err(Error::refused(
                    Gate::Role,
                    format!(
                        "refusing {name}: agent role {:?} does not grant it. This role grants {}.",
                        role.as_str(),
                        grants_line(*role)
                    ),
                ));
            }
            Ok(_) => {}
        }
    }
    // Fail loud: while the environment probe says not-ready (mints down,
    // version-pin mismatch, arena torn down), no tool runs. The notes ARE
    // the error so the agent sees exactly what to fix. The probe is a
    // startup snapshot — environments RECOVER (mints get restarted
    // mid-session), so while gated, re-probe (rate-limited) before
    // refusing: a closed gate must heal itself.
    if !ctx.probe_ready && ctx.last_probe.elapsed() >= REPROBE_INTERVAL {
        ctx.last_probe = std::time::Instant::now();
        let v1 = ctx.plugin.as_ref().is_some_and(|plugin| {
            plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1
        });
        let reprobe = if v1 {
            match v1_call_params(ctx, json!({})) {
                Ok((_, params)) => resilient(ctx, |plugin| {
                    plugin.hello()?;
                    plugin
                        .call_v1("session_probe", Some(params))
                        .map(|value| ProbeResult {
                            ready: value
                                .get("ready")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            notes: value
                                .get("notes")
                                .and_then(Value::as_str)
                                .unwrap_or("session probed")
                                .to_string(),
                            running_version: value
                                .get("running_version")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            expected_tag: value
                                .get("expected_tag")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                }),
                Err(error) => Err(error),
            }
        } else {
            resilient(ctx, |plugin| plugin.probe())
        };
        match reprobe {
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
        return Err(Error::refused(
            Gate::Probe,
            format!("harness not ready — probe: {}", ctx.probe_notes),
        ));
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
        // The one refusal that means the corpus server had no opinion. It
        // is gated as `Unknown` rather than `Args` so a reader can tell
        // "we turned this away" from "we never recognized it" — the
        // difference between debugging our gate and debugging opencode's.
        other => Err(Error::refused(
            Gate::Unknown,
            format!("unknown tool: {other}"),
        )),
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
    // `Harness`, not `Scope`: a missing plugin is a dead environment, not
    // an unresolved project. It was reported as a scope error before the
    // refusal log existed, where the distinction cost nothing; now it is a
    // field an operator filters on, and a line reading `gate: scope` would
    // send them auditing CORPUS_PROJECT for a broken docker.
    let plugin = ctx.plugin.as_mut().ok_or_else(|| {
        Error::refused(
            Gate::Harness,
            format!("no environment plugin: {}", ctx.probe_notes.as_str()),
        )
    })?;
    match f(plugin) {
        Ok(value) => Ok(value),
        Err(error) => {
            if matches!(
                error,
                corpus_core::Error::PluginClosed(_) | corpus_core::Error::Plugin { .. }
            ) {
                if let Some(plugin) = ctx.plugin.as_mut() {
                    let _ = plugin.restart();
                }
            }
            Err(Error::Plugin(error.to_string()))
        }
    }
}

fn require_u64(args: &Value, key: &str) -> Result<u64> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| Error::Args(format!("missing integer argument: {key}")))
}

/// Where the pinned source is, FOR THIS CALLER.
///
/// The same trees sit at two addresses, each reachable by exactly one tool:
/// `sources/<name>/<sha>` in the run cwd, read by opencode's own file
/// tools, and `/opt/src/<name>`, which exists only inside a `sandbox_exec`
/// command. So the mount is named only to a role that can enter the
/// sandbox. The host path is RELATIVE because the trees are reached through
/// a symlink pointing outside the run dir: the render writes
/// `external_directory: deny` for every role but `super`, and `super` gets
/// no key at all, so opencode's own default decides — which in practice has
/// denied. Relative is the form that works for all four.
fn source_paths(ctx: &Ctx, sources: &[corpus_core::SourceInfo]) -> Value {
    let sandboxed = matches!(&ctx.role, Ok(role) if role.allows("sandbox_exec"));
    let pinned: Vec<Value> = sources
        .iter()
        .map(|source| {
            let mut entry = json!({
                "name": source.name,
                "repo": source.repo,
                "tag": source.tag,
                "sha": source.sha,
                "path": format!("sources/{}/{}", source.name, source.sha),
            });
            if sandboxed {
                entry["path_inside_sandbox_exec"] = json!(source.mount);
            }
            entry
        })
        .collect();
    let mut out = json!({
        "note": "the pinned upstream source — target implementation and spec. The only sanctioned source; prefer it over memory and over anything you read on the internet.",
        "read_with": "your own file tools. `path` is relative to your working directory — use it as given, and do not rewrite it to an absolute path: the trees live outside this directory and only the relative form is readable.",
        "pinned": pinned
    });
    if sandboxed {
        out["also_inside_sandbox_exec"] = json!(
            "within a sandbox_exec command — and ONLY there — the same trees are mounted read-only at `path_inside_sandbox_exec`. That path does not exist on the host, so your own file tools cannot open it; `path` is the one to read with those."
        );
    }
    out
}

fn target_info(ctx: &mut Ctx) -> Result<String> {
    if ctx
        .plugin
        .as_ref()
        .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
    {
        let (session, params) = v1_call_params(ctx, json!({}))?;
        let declared = ctx
            .plugin
            .as_ref()
            .map(|plugin| plugin.manifest().sources.clone())
            .unwrap_or_default();
        let description: corpus_core::EnvironmentDescription = resilient(ctx, |plugin| {
            let value = plugin.call_v1("describe", Some(params))?;
            Ok(serde_json::from_value(value)?)
        })?;
        let sources: Vec<Value> = declared
            .iter()
            .filter_map(|source| {
                let sha = ctx.source_pins.as_ref()?.get(&source.id)?.as_str()?;
                Some(json!({
                    "id": source.id,
                    "sha": sha,
                    "path": format!("sources/{}/{sha}", source.id),
                    "path_inside_sandbox_exec": source.mount,
                }))
            })
            .collect();
        return Ok(serde_json::to_string_pretty(&json!({
            "environment_session": session,
            "targets": description.targets,
            "scope": "ONLY these typed targets may be attacked",
            "sources": sources,
            "tools_in_sandbox": description.tools,
            "limits": description.limits,
            "provenance": description.provenance,
            "run_log": ctx.run_log,
        }))?);
    }
    let targets = resilient(ctx, |p| p.targets())?;
    let tools = resilient(ctx, |p| p.tools())?;
    let pins = ctx.source_pins.clone();
    let sources = resilient(ctx, |p| p.sources_with_sources(pins.as_ref()))?;
    Ok(serde_json::to_string_pretty(&json!({
        "targets": targets,
        "scope": "ONLY these URLs may be attacked; the sandbox cannot reach anything else",
        "sources": source_paths(ctx, &sources),
        "tools_in_sandbox": tools,
        "faucet": {
            "max_payment_sats": 100000,
            "session_budget_sats": ctx.faucet_budget_sats,
            "spent_this_session": ctx.faucet_spent_sats
        },
        "funding_flow": "use the wallet_fund tool — it does quote -> pay -> claim deterministically",
        "run_log": ctx.run_log.clone(),
        "run_log_note": "the basename of THIS mission's transcript in runs/. Cite it as the `run_log` argument to technique_save (or omit run_log entirely to default to this)."
    }))
    .unwrap_or_else(|_| "{}".to_string()))
}

fn sandbox_exec(ctx: &mut Ctx, command: &str) -> Result<String> {
    let result = if ctx
        .plugin
        .as_ref()
        .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
    {
        let (_, params) = v1_call_params(ctx, json!({"command": command}))?;
        resilient(ctx, |plugin| {
            let value = plugin.call_v1("sandbox_exec", Some(params))?;
            Ok(serde_json::from_value(value)?)
        })?
    } else {
        let pins = ctx.source_pins.clone();
        resilient(ctx, |p| p.sandbox_exec_with_sources(command, pins.as_ref()))?
    };
    let mut combined = result.output;
    if combined.len() > OUTPUT_CAP_BYTES {
        combined.truncate(OUTPUT_CAP_BYTES);
        combined.push_str("\n[truncated]");
    }
    combined.push_str(&format!("\n[exit {}]", result.exit_code));
    Ok(combined)
}

fn oracle_run(ctx: &mut Ctx, name: &str) -> Result<String> {
    if !valid_oracle_name(name) {
        return Err(Error::Args("bad oracle name".to_string()));
    }
    let result = if ctx
        .plugin
        .as_ref()
        .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
    {
        let (_, params) = v1_call_params(ctx, json!({"name": name}))?;
        resilient(ctx, |plugin| {
            let value = plugin.call_v1("call_oracle", Some(params))?;
            Ok(serde_json::from_value(value)?)
        })?
    } else {
        resilient(ctx, |p| p.call_oracle(name))?
    };
    let result = validate_oracle_result(name, result)?;
    Ok(format!("verdict: {}\n{}", result.verdict, result.log))
}

fn faucet(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let op = require_str(args, "op")?;
    let idempotency_key = match op.as_str() {
        "pay" | "invoice" => Some(require_str(args, "idempotency_key")?),
        _ => args
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let call = FaucetCall {
        invoice: args.get("invoice").and_then(Value::as_str).map(str::to_string),
        amount_sat: args.get("amount_sat").and_then(Value::as_u64),
        memo: args.get("memo").and_then(Value::as_str).map(str::to_string),
        idempotency_key,
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
    let result = if ctx
        .plugin
        .as_ref()
        .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
    {
        let (_, params) = v1_call_params(ctx, json!({"op": op, "call": call}))?;
        resilient(ctx, |plugin| {
            let value = plugin.call_v1("faucet", Some(params))?;
            Ok(serde_json::from_value(value)?)
        })?
    } else {
        resilient(ctx, |p| p.faucet(&op, &call))?
    };
    if let Some(paid) = result.paid_sats {
        ctx.faucet_spent_sats = ctx.faucet_spent_sats.saturating_add(paid);
        return Ok(format!(
            "Payment succeeded ({paid} sat, hash {}). Session spend: {} sat.",
            result.payment_hash.as_deref().unwrap_or("unreported"),
            ctx.faucet_spent_sats
        ));
    }
    if result.idempotent_replay {
        return Ok(format!(
            "Payment already succeeded ({} sat, hash {}); no additional session charge.",
            result.amount_sat.unwrap_or_default(),
            result.payment_hash.as_deref().unwrap_or("unreported")
        ));
    }
    if op == "invoice" {
        if let Some(inv) = result.text.lines().find(|l| l.starts_with("lnbcrt")) {
            return Ok(format!("Invoice created: {inv}"));
        }
    }
    Ok(result.text)
}

/// Ask the plugin to fund a session-scoped attacker wallet. Quote/payment/
/// claim mechanics and output parsing are environment implementation details.
fn wallet_fund(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let amount = require_u64(args, "amount_sat")?;
    let _idempotency_key = require_str(args, "idempotency_key")?;
    if ctx.faucet_spent_sats.saturating_add(amount) > ctx.faucet_budget_sats {
        return Ok(format!(
            "[faucet refused] session budget of {} sat exhausted",
            ctx.faucet_budget_sats
        ));
    }
    if ctx
        .plugin
        .as_ref()
        .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
    {
        let (_, params) = v1_call_params(ctx, args.clone())?;
        let result = resilient(ctx, |plugin| plugin.call_v1("wallet_fund", Some(params)))?;
        let charged = result
            .get("charged_sats")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                if result.get("funded").and_then(Value::as_bool) == Some(true) {
                    amount
                } else {
                    0
                }
            });
        ctx.faucet_spent_sats = ctx.faucet_spent_sats.saturating_add(charged);
        return Ok(serde_json::to_string_pretty(&result)?);
    }
    let params = args.as_object().cloned().unwrap_or_default();
    let pins = ctx.source_pins.clone();
    let result = resilient(ctx, |plugin| plugin.wallet_fund_legacy(params, pins.as_ref()))?;
    if result.get("funded").and_then(Value::as_bool) == Some(true) {
        ctx.faucet_spent_sats = ctx.faucet_spent_sats.saturating_add(amount);
    }
    Ok(serde_json::to_string_pretty(&result)?)
}

fn require_environment_session(ctx: &Ctx) -> Result<String> {
    ctx.environment_session
        .clone()
        .ok_or_else(|| Error::Scope("v1 plugin call has no durable environment session".into()))
}

/// Add corpus-owned runtime/source locations to every v1 session call. A
/// plugin process is deliberately one-shot and cannot rediscover these paths
/// from its immutable bundle, so the caller repeats the full explicit context.
fn v1_call_params(ctx: &Ctx, extra: Value) -> Result<(String, Value)> {
    let session = require_environment_session(ctx)?;
    let mut params = extra.as_object().cloned().unwrap_or_default();
    params.insert("session_id".into(), Value::String(session.clone()));
    let Some(plugin) = ctx.plugin.as_ref() else {
        return Ok((session, Value::Object(params)));
    };
    let plugin_id = plugin.manifest().name.clone();
    if let Ok(record) = ctx.store.load_environment_session_key(&plugin_id, &session) {
        let state_dir = ctx
            .store
            .plugin_runtime_dir(&plugin_id)
            .map_err(|error| Error::Plugin(error.to_string()))?
            .join("state")
            .join(&session);
        let source_cache = ctx.store.source_cache_dir();
        let sources: Vec<Value> = plugin
            .manifest()
            .sources
            .iter()
            .filter_map(|source| {
                record.source_shas.get(&source.id).map(|sha| {
                    json!({
                        "id": source.id,
                        "sha": sha,
                        "host_path": source_cache.join(&source.id).join(sha),
                        "mount": source.mount,
                    })
                })
            })
            .collect();
        params.insert("state_dir".into(), json!(state_dir));
        params.insert("source_cache".into(), json!(source_cache));
        params.insert("sources".into(), Value::Array(sources));
        params.insert("project".into(), json!(record.id.project));
        params.insert("mission".into(), json!(record.id.mission));
        params.insert("run".into(), json!(record.id.generation));
    }
    Ok((session, Value::Object(params)))
}

/// The verification gate: the plugin's oracle suite runs server-side
/// before any finding is written; the verdict is recorded on the finding.
/// Works for ANY plugin via `oracles()` + `call_oracle()`. The finding
/// lands in the project corpus (default class: embargoed).
fn finding_write(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let title = require_str(args, "title")?;
    let severity_raw = require_str(args, "severity")?;
    let severity = FindingSeverity::parse(&severity_raw).ok_or_else(|| {
        Error::Args(format!(
            "invalid finding severity {severity_raw:?}; expected critical, high, medium, or low"
        ))
    })?;
    let detail = require_str(args, "detail")?;
    let path = match args.get("path") {
        None => None,
        Some(Value::String(path)) => Some(path.clone()),
        Some(_) => return Err(Error::Args("path must be a string".into())),
    };
    let metadata: BTreeMap<String, Value> = match args.get("metadata") {
        None => BTreeMap::new(),
        Some(Value::Object(metadata)) => metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        Some(_) => return Err(Error::Args("metadata must be an object".into())),
    };
    let scope = ctx.write_scope(args)?;

    let mut verified = false;
    let mut oracle_out = String::new();
    let oracle_list = if ctx
        .plugin
        .as_ref()
        .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
    {
        v1_call_params(ctx, json!({})).and_then(|(_, params)| {
            resilient(ctx, |plugin| {
                let value = plugin.call_v1("oracles", Some(params))?;
                Ok(serde_json::from_value(value)?)
            })
        })
    } else {
        resilient(ctx, |p| p.oracles())
    };
    match oracle_list.and_then(validate_oracle_catalog) {
        Ok(oracles) => {
            for oracle in &oracles {
                let result = if ctx
                    .plugin
                    .as_ref()
                    .is_some_and(|plugin| plugin.manifest().manifest_version == corpus_core::PluginManifestVersion::V1)
                {
                    v1_call_params(ctx, json!({"name": oracle.name})).and_then(|(_, params)| {
                        resilient(ctx, |plugin| {
                            let value = plugin.call_v1("call_oracle", Some(params))?;
                            Ok(serde_json::from_value(value)?)
                        })
                    })
                } else {
                    resilient(ctx, |p| p.call_oracle(&oracle.name))
                };
                let line = match result {
                    Ok(result) => match validate_oracle_result(&oracle.name, result) {
                        Ok(result) => {
                            if result.verdict == "violated" {
                                verified = true;
                            }
                            let mut evidence =
                                format!("  {:<36} {}\n", oracle.name, result.verdict);
                            if result.log.is_empty() {
                                evidence.push_str("    evidence: <empty>\n");
                            } else {
                                evidence.push_str("    evidence:\n");
                                for line in result.log.lines() {
                                    evidence.push_str("      ");
                                    evidence.push_str(line);
                                    evidence.push('\n');
                                }
                            }
                            evidence
                        }
                        Err(error) => format!("  {:<36} ERROR ({error})\n", oracle.name),
                    },
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
    let finding = NewFinding {
        title,
        severity,
        detail,
        timestamp: ts,
        oracle_verified: verified,
        oracle_output: oracle_out,
        path,
        metadata,
        run_log: ctx.run_log.clone(),
        actor: Some(ctx.store.actor().to_string()),
        source_pins: ctx.source_pins.clone(),
    };
    let written = ctx
        .store
        .write_finding(&scope.project, &finding)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "finding recorded in {}: {} (reference: {}, oracle_verified: {verified}, sensitivity: embargoed)",
        scope.project,
        written.path.display(),
        written.reference
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
    let dir = category_dir(ctx, &scope, "attacks")?.join(&slug);
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
    // run_log defaults to this mission's transcript (CORPUS_RUN_LOG at
    // launch) when the agent omits it — the sandbox has no host FS and
    // cannot enumerate runs/ to discover the name.
    let run_log = args
        .get("run_log")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| ctx.run_log.clone())
        .ok_or_else(|| {
            Error::Args(
                "run_log not provided and no CORPUS_RUN_LOG is set for this mission \
                 (call target_info to see the current run_log, then pass it here)"
                    .to_string(),
            )
        })?;
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
    let dir = category_dir(ctx, &scope, "techniques")?;
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
