//! The EMBEDDED goose runtime backend for the management chat
//! (dev/decisions.md chunk 1) — the ONLY place in corpus-app that names a
//! goose/GDK type. Quarantined behind `crate::chat`'s public event/command
//! types; nothing in this module is exported.
//!
//! Runtime (operator decision 2026-08-14): goose's `Agent` runs IN-PROCESS as
//! a source-level dependency (git-dep at a pinned rev — see
//! dev/decisions.md.md "Bumps are deliberate events"). It replaces the old
//! `chat/acp.rs`, which spawned a managed `goose acp` subprocess and spoke
//! Agent Client Protocol. The [`Chat`] seam (`crate::chat`) is UNCHANGED; only
//! this transport swapped. Git history keeps acp.rs; the fallback story lives
//! in dev/decisions.md.md's record.
//!
//! Tool source: `corpus-mcp --admin` is spawned DIRECTLY as a stdio MCP
//! extension (a CORPUS subprocess, our own protocol — not a goose subprocess).
//! The confirm ritual is in-process and STRONGER than the ACP round-trip: a
//! tool call that needs approval surfaces as [`ChatEvent::PermissionRequest`]
//! and is released only by the operator's inline [`ChatCommand::Approve`];
//! goose's `tool_confirmation_router` delivers the decision BEFORE the tool is
//! dispatched. The model never sees a confirmation token — interception is
//! before execution (server-side corpus-mcp token gate stays as backstop).

use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};

use super::{ChatCommand, ChatEvent};
use crate::chat::team::TeamRole;

/// The operator's own GOOSE_INPUT_LIMIT, captured at app start BEFORE we
/// install per-model values (an operator setting always wins over the
/// per-model profile).
static OPERATOR_INPUT_LIMIT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Process-wide goose environment, set ONCE at app start (from `main`) —
/// never per-session. goose's `Config::global()` is a OnceLock (first
/// reader wins) and `get_param` consults ENV before the config file, so
/// these must be in place before ANY goose call; a pre-existing operator
/// value always wins (env-overridable).
///
/// - `GOOSE_STREAM_TIMEOUT`: goose's default is 120s — a thinking model
///   (qwen3.8:27b) reasons for MINUTES before its first visible chunk, so
///   the default killed healthy turns ("Ollama stream stalled", the
///   2026-08-14 stall). 900s covers slow local turns.
/// - `GOOSE_INPUT_LIMIT` is NOT set here: it is per-MODEL (chat_input_limit),
///   applied at session setup; an operator-set value is captured and wins.
pub fn init_goose_env() {
    let _ = OPERATOR_INPUT_LIMIT.set(std::env::var("GOOSE_INPUT_LIMIT").ok());
    fn set_default(key: &str, value: &str) {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, value);
        }
    }
    set_default("GOOSE_STREAM_TIMEOUT", "900");
    set_default("GOOSE_TELEMETRY_ENABLED", "false");
}

/// The per-model chat profile: Ollama `num_ctx` for this model. Big dense
/// models pay prefill + KV-cache cost per context token and the management
/// chat's turns are short, so trade context for turn latency on the heavy
/// weights (32k on a 27B Q8 was a driver of the multi-minute stalls).
/// An operator-set GOOSE_INPUT_LIMIT always wins (captured in
/// init_goose_env).
fn chat_input_limit(model: &str) -> String {
    if let Some(Some(limit)) = OPERATOR_INPUT_LIMIT.get() {
        return limit.clone();
    }
    let m = model.to_lowercase();
    for heavy in ["27b", "32b", "35b", "70b", "120b"] {
        if m.contains(heavy) {
            return "16384".to_string();
        }
    }
    "32768".to_string()
}

/// Spawn the backend for `project` on a background thread, returning our
/// project-scoped session id. `role` selects the team shape: an
/// `Operator`/`Orchestrator` runs all-or-none admin tools; a specialist
/// (`CorpusInspector`, …) registers ONLY its scoped domain (chunk 2). With the
/// `chat-embed` feature OFF this is a no-op stub that reports a clear error
/// (headless `--no-default-features` build).
pub fn spawn_backend(
    project: &str,
    _model: &str,
    role: TeamRole,
    ev_tx: StdSender<ChatEvent>,
    _cmd_rx: StdReceiver<ChatCommand>,
) -> String {
    let session_id = crate::chat::panel::session_id(project);
    #[cfg(feature = "chat-embed")]
    {
        self::live::spawn_backend_impl(project, _model, role, ev_tx, _cmd_rx, session_id.clone());
    }
    #[cfg(not(feature = "chat-embed"))]
    {
        let _ = role;
        let _ = ev_tx.send(ChatEvent::Error(format!(
            "chat backend not compiled into this build (session {session_id}) — build with the `chat-embed` cargo feature (default)"
        )));
    }
    session_id
}

/// The live embedded-goose backer. Compiled only with the `chat-embed` feature.
#[cfg(feature = "chat-embed")]
mod live {
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
    use std::sync::Arc;

    use futures::StreamExt;

    use goose::agents::{Agent, AgentConfig, AgentEvent, GoosePlatform, SessionConfig};
    use goose::conversation::message::{ActionRequiredData, Message, MessageContentBlock};
    use goose::config::{
        GooseMode, PermissionManager, DEFAULT_EXTENSION_DESCRIPTION, DEFAULT_EXTENSION_TIMEOUT,
    };
    use goose::permission::permission_confirmation::PrincipalType;
    use goose::permission::{Permission, PermissionConfirmation};
    use goose::session::{SessionManager, SessionType};
    use tokio_util::sync::CancellationToken;

    use super::super::{ChatCommand, ChatEvent};
    use crate::chat::team::TeamRole;

    /// Drive the in-process goose agent on a background tokio runtime for the
    /// backend lifetime. Env mutation (GOOSE_PATH_ROOT / GOOSE_INPUT_LIMIT /
    /// telemetry off) is done here at thread start: this is a single chat
    /// session, and the runtime reads these leniently at startup.
    pub fn spawn_backend_impl(
        project: &str,
        model: &str,
        role: TeamRole,
        ev_tx: StdSender<ChatEvent>,
        cmd_rx: StdReceiver<ChatCommand>,
        session_id: String,
    ) {
        let project = project.to_owned();
        let model = model.to_owned();
        let ev_tx2 = ev_tx.clone();
        std::thread::Builder::new()
            .name("chat-embed".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ev_tx.send(ChatEvent::Error(format!(
                            "chat backend runtime start failed: {e}"
                        )));
                        return;
                    }
                };
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rt.block_on(run(project, model, role, session_id, ev_tx2, cmd_rx));
                }));
                if let Err(panic) = result {
                    let msg = panic_msg(&panic);
                    let _ = ev_tx.send(ChatEvent::Error(format!("chat backend crashed: {msg}")));
                }
            })
            .expect("spawn chat backend");
    }

    fn panic_msg(panic: &Box<dyn std::any::Any + Send>) -> String {
        match panic.downcast_ref::<&str>() {
            Some(s) => (*s).to_string(),
            None => match panic.downcast_ref::<String>() {
                Some(s) => s.clone(),
                None => "unknown panic".to_string(),
            },
        }
    }

    /// The project-scoped goose root: `<store>/projects/<project>/var/chat/`,
    /// ABSOLUTE (goose's GOOSE_PATH_ROOT validation silently drops relative
    /// paths). Store root resolution rides corpus-core's `store_root_env`
    /// (CORPUS_STORE, else the canonical default) — never cwd-relative.
    fn project_scope(project: &str) -> PathBuf {
        corpus_core::store_root_env()
            .join("projects")
            .join(project)
            .join("var/chat")
    }

    async fn run(
        project: String,
        model: String,
        role: TeamRole,
        session_id: String,
        ev_tx: StdSender<ChatEvent>,
        cmd_rx: StdReceiver<ChatCommand>,
    ) {
        // Harness diagnostics log — per-turn lifecycle, tool calls, usage,
        // errors (was a "symbolic log" that was opened and never written;
        // the 2026-08-14 stall audit had to reconstruct timings from the
        // session DB instead).
        std::fs::create_dir_all(project_scope(&project)).ok();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(project_scope(&project).join("chat.log"))
            .ok()
            .map(|f| std::sync::Arc::new(std::sync::Mutex::new(f)));
        log_line(&log, &format!("session {session_id} starting (role {role}, model {model})"));

        let (agent, goose_session) = match setup(&project, &model, role).await {
            Ok(pair) => pair,
            Err(e) => {
                log_line(&log, &format!("setup failed: {e}"));
                let _ = ev_tx.send(ChatEvent::Error(format!("chat backend setup failed: {e}")));
                return;
            }
        };

        // Bridge the blocking std cmd channel onto the runtime so the reply
        // stream task and the command loop can coexist. MUST be
        // spawn_blocking: a blocking std recv inside tokio::spawn parks the
        // current-thread runtime and deadlocks the command loop (the
        // "type a message, nothing happens" bug).
        let (cmd_tx, mut cmd_rx_async) = tokio::sync::mpsc::channel::<ChatCommand>(64);
        tokio::task::spawn_blocking(move || {
            while let Ok(cmd) = cmd_rx.recv() {
                if cmd_tx.blocking_send(cmd).is_err() {
                    break;
                }
            }
        });

        let _ = ev_tx.send(ChatEvent::Ready {
            session_id,
            project: project.clone(),
        });

        let mut turn: u64 = 0;
        // The operator's current app position, juiced into every turn.
        let context = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        // Turn serialization: exactly ONE live turn per session. A Send while
        // a turn is in flight QUEUES (two concurrent agent.reply calls on one
        // session interleave/corrupt the conversation — the old fire-and-
        // forget spawn). The cancellation token is PER-TURN: the old
        // session-wide token meant one Stop pre-cancelled every later turn.
        // A live turn is its cancel token PLUS its task handle: Stop cancels
        // the token (cooperative — the stream loop bails on it) AND aborts
        // the handle (hard backstop), so a turn stuck in a multi-minute
        // local generation dies immediately instead of running to the 900 s
        // stream timeout.
        let mut live: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;
        let mut queued: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        let (done_tx, mut done_rx) = tokio::sync::mpsc::channel::<u64>(8);
        let mut turn_started = std::time::Instant::now();
        // Confirmation-id → the agent that must receive the decision: the
        // main session OR a delegated specialist (a specialist's write tools
        // surface Approve/Reject in the panel like the main session's).
        let pending: PendingApprovals = std::sync::Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::new(),
        ));

        loop {
            tokio::select! {
                cmd = cmd_rx_async.recv() => {
                    let Some(cmd) = cmd else {
                        break; // sender dropped -> app gone
                    };
                    match cmd {
                        ChatCommand::Send(msg) => {
                            if live.is_some() {
                                log_line(&log, &format!("turn {} queued ({} deep)", turn + 1, queued.len() + 1));
                                queued.push_back(msg);
                                continue;
                            }
                            turn += 1;
                            turn_started = std::time::Instant::now();
                            log_line(&log, &format!("turn {turn} start: {}", crate::chat::truncate(&msg, 120)));
                            live = Some(spawn_turn(
                                &agent, &goose_session, &context, &ev_tx, &done_tx, &log,
                                &pending, &project, &model, turn, msg,
                            ));
                        }
                        ChatCommand::Stop => {
                            if let Some((token, handle)) = live.take() {
                                // Cooperative first (lets the stream loop close
                                // the provider connection cleanly), then a hard
                                // abort so nothing can outlive the operator's
                                // click. The aborted task never signals `done`,
                                // so retire the turn inline here.
                                token.cancel();
                                handle.abort();
                                let dropped = queued.len();
                                queued.clear();
                                log_line(&log, &format!(
                                    "turn {turn} stopped by operator ({dropped} queued dropped)"
                                ));
                                let _ = ev_tx.send(ChatEvent::Stopped { turn });
                                let _ = ev_tx.send(ChatEvent::TurnEnd { turn });
                            }
                        }
                        ChatCommand::SetContext(ctx) => {
                            *context.lock().unwrap() = ctx;
                        }
                        ChatCommand::Close => break,
                        ChatCommand::Approve { id } => {
                            deliver_confirmation(&agent, &pending, id, Permission::AllowOnce).await;
                        }
                        ChatCommand::Reject { id } => {
                            deliver_confirmation(&agent, &pending, id, Permission::DenyOnce).await;
                        }
                    }
                }
                finished = done_rx.recv() => {
                    let Some(_finished_turn) = finished else { break };
                    // A turn the operator already stopped took `live`; its task
                    // may still send a late `done` before the abort lands.
                    // Ignore it — the Stop handler already retired the turn.
                    if live.is_none() {
                        continue;
                    }
                    live = None;
                    log_line(&log, &format!("turn {turn} end ({:.1}s)", turn_started.elapsed().as_secs_f32()));
                    // TurnEnd is emitted HERE (after every event of the turn
                    // is already on the wire), never by the turn task — a
                    // stale task can no longer retire a newer turn.
                    let _ = ev_tx.send(ChatEvent::TurnEnd { turn });
                    if let Some(next) = queued.pop_front() {
                        turn += 1;
                        turn_started = std::time::Instant::now();
                        log_line(&log, &format!("turn {turn} start (dequeued): {}", crate::chat::truncate(&next, 120)));
                        live = Some(spawn_turn(
                            &agent, &goose_session, &context, &ev_tx, &done_tx, &log,
                            &pending, &project, &model, turn, next,
                        ));
                    }
                }
            }
        }
    }

    /// Confirmation-id → the agent holding the pending call (the main
    /// session or a delegated specialist). Read-only tools never enter this
    /// map (auto-released); write/destructive ids land here until the
    /// operator's Approve/Reject routes the decision to the RIGHT agent.
    type PendingApprovals =
        std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<Agent>>>>;

    /// Route an operator decision to the agent that owns the pending call
    /// (specialist confirmations are registered by run_specialist; the main
    /// session is the fallback).
    async fn deliver_confirmation(
        main_agent: &Arc<Agent>,
        pending: &PendingApprovals,
        id: String,
        permission: Permission,
    ) {
        let owner = pending.lock().unwrap().remove(&id);
        let agent = owner.as_ref().unwrap_or(main_agent);
        let _ = agent
            .tool_confirmation_router
            .deliver(
                id,
                PermissionConfirmation {
                    principal_type: PrincipalType::Tool,
                    permission,
                },
            )
            .await;
    }

    /// Start one turn task and return ITS cancellation token AND task
    /// handle. The token drives a cooperative stop (the stream loop selects
    /// on it); the handle is the hard-abort backstop. The task streams
    /// events, then signals `done_tx`; the command loop owns `TurnEnd` and
    /// the queued-next-turn handoff.
    #[allow(clippy::too_many_arguments)]
    fn spawn_turn(
        agent: &Arc<Agent>,
        goose_session: &str,
        context: &std::sync::Arc<std::sync::Mutex<String>>,
        ev_tx: &StdSender<ChatEvent>,
        done_tx: &tokio::sync::mpsc::Sender<u64>,
        log: &Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>,
        pending: &PendingApprovals,
        project: &str,
        model: &str,
        turn: u64,
        msg: String,
    ) -> (CancellationToken, tokio::task::JoinHandle<()>) {
        let token = CancellationToken::new();
        let _ = ev_tx.send(ChatEvent::TurnStart { turn });
        let ev = ev_tx.clone();
        let agent = agent.clone();
        let ctx = context.lock().unwrap().clone();
        let msg = if ctx.is_empty() {
            msg
        } else {
            format!(
                "[operator context — where the user is in the corpus app right now: {ctx}. Resolve references like \"this agent\" / \"this project\" / \"this page\" against it, and prefer your corpus-admin tools over filesystem exploration.]\n\n{msg}"
            )
        };
        let session_cfg = SessionConfig {
            // The REAL goose session id from create_session (its
            // second arg is a display NAME, not the id) — replying
            // against `project` here targeted a nonexistent
            // session.
            id: goose_session.to_string(),
            schedule_id: None,
            max_turns: None,
            retry_config: None,
        };
        let cancel = token.clone();
        let done = done_tx.clone();
        let log = log.clone();
        let pending = pending.clone();
        let project = project.to_string();
        let model = model.to_string();
        let handle = tokio::spawn(async move {
            translate_turn(agent, msg, session_cfg, cancel, ev, log, pending, project, model, turn).await;
            let _ = done.send(turn).await;
        });
        (token, handle)
    }

    /// Append one timestamped diagnostics line to the session's chat.log.
    /// Best-effort: diagnostics never break a turn.
    fn log_line(log: &Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>, line: &str) {
        use std::io::Write as _;
        let Some(log) = log else { return };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut f) = log.lock() {
            let _ = writeln!(f, "[{ts}] {line}");
        }
    }

    /// Build the agent, provider, session, and (per `role`) the corpus-admin
    /// stdio extension — the chunk-1 setup sequence, mirroring goose's own
    /// example (Agent::with_config not Agent::new, to avoid config-file
    /// construction). Team shape (chunk 2): a specialist registers the
    /// `corpus-mcp --admin` extension with `available_tools` = its scoped
    /// domain (goose refuses anything else BY CONSTRUCTION); the Orchestrator
    /// registers NO admin extension (no tools to call).
    async fn setup(project: &str, model: &str, role: TeamRole) -> anyhow::Result<(Arc<Agent>, String)> {
        let scope = project_scope(project);
        // Session-scoped env: redirect all goose config/data/state into the
        // project scope, and pin the model. (Process-wide knobs —
        // GOOSE_STREAM_TIMEOUT / GOOSE_INPUT_LIMIT / telemetry — are set ONCE
        // by `init_goose_env` at app start. NOTE: goose's Config::global()
        // is a OnceLock, so GOOSE_PATH_ROOT only reliably shapes the FIRST
        // session's global config; everything project-specific we need is
        // passed explicitly (SessionManager path, session working_dir).)
        std::env::set_var("GOOSE_PATH_ROOT", &scope);
        std::env::set_var("GOOSE_MODEL", model);
        // Per-model context size (operator's GOOSE_INPUT_LIMIT wins).
        std::env::set_var("GOOSE_INPUT_LIMIT", super::chat_input_limit(model));

        let session_mgr = SessionManager::new(scope.join("data"));
        let permission_mgr = PermissionManager::instance();
        let mut config = AgentConfig::new(
            Arc::new(session_mgr),
            permission_mgr,
            None,
            GooseMode::Approve,
            false,
            GoosePlatform::GooseCli,
        );
        // Skip HookManager::load disk reads at construction (the subagent arm
        // drops hook loading entirely; it has no other effect on our path).
        config.is_subagent = true;
        let agent = Arc::new(Agent::with_config(config));

        let provider = goose::providers::create_with_named_model("ollama", vec![]).await?;
        let model_config =
            goose::model_config::model_config_from_user_config("ollama", model)?;

        // Session identity is OUR term: reuse the project-scoped id. The
        // working_dir is the project SCOPE so summon's local discovery
        // (<working_dir>/.agents/agents) finds the specialist files
        // write_specialist_agents publishes — an empty working_dir made
        // delegation discovery resolve against the app's cwd (the "No
        // sources available for load/delegate" bug).
        let session = agent
            .config
            .session_manager
            .create_session(
                scope.clone(),
                project.to_string(),
                SessionType::Hidden,
                GooseMode::Approve,
            )
            .await?;

        agent
            .update_provider(provider, model_config, &session.id)
            .await?;

        // Identity: the chat agent is corpus's management assistant, NEVER
        // goose. goose's stock system.md opens with "You are a
        // general-purpose AI agent called goose…", which the model parrots
        // ("Hello! I'm goose"). A full system-prompt override keeps goose's
        // extension listing + response guidelines but replaces the identity
        // (the "our agent thinks it's goose" bug).
        agent
            .override_system_prompt(corpus_system_prompt(role, project))
            .await;

        match role {
            // The Orchestrator's capability surface is EMPTY by construction:
            // NO admin extension. (an empty `available_tools` means "all tools"
            // in goose, so absence — not an empty filter — is how we withhold
            // them). It delegates to specialists through OUR `delegate`
            // frontend tool (build_team_extension): goose's summon platform
            // extension was dropped because a delegated subagent inherits only
            // the PARENT session's extensions — the orchestrator holds none,
            // so summon subagents spawned with zero admin tools and per-
            // specialist scoping was impossible (audit 2026-08-14). Our
            // delegate spawns the specialist in-process with its own scoped
            // corpus-admin extension — scoping BY CONSTRUCTION.
            TeamRole::Orchestrator => {
                agent
                    .add_extension(build_team_extension(), &session.id)
                    .await
                    .map_err(|e| anyhow::anyhow!("could not register the corpus-team (delegation) extension: {e}"))?;
            }
            _ => {
                let scoped = build_corpus_admin_extension(role)?;
                agent.add_extension(scoped, &session.id).await.map_err(|e| {
                    anyhow::anyhow!(
                        "could not register the corpus-admin extension for role {role} (is corpus-mcp built?): {e}"
                    )
                })?;
            }
        }

        Ok((agent, session.id))
    }

    /// The management chat's system-prompt template (goose miniJinja
    /// flavour, rendered against goose's `SystemPromptContext`): the stock
    /// `system.md` identity ("You are a general-purpose AI agent called
    /// goose…") replaced with the corpus management assistant, while KEEPING
    /// goose's extension listing and response guidelines so tool use is
    /// unaffected. Role/project are formatted in here — goose's render
    /// context doesn't know them.
    pub(super) fn corpus_system_prompt(role: TeamRole, project: &str) -> String {
        format!(
            r#"You are the corpus management assistant — the operator-facing chat agent of corpus, a local-first vulnerability research platform. You are NOT goose; goose is only the runtime engine you happen to run on. Never introduce yourself as goose, and never claim to be created by Block or AAIF.

You are running as the **{role}** of the corpus management team ({role_desc}), scoped to project "{project}". Prefer your corpus-admin tools for store questions over guessing.

The current date and time is {{{{ current_date_time }}}}.

{{% if include_extensions and not code_execution_mode %}}

# Extensions

Extensions provide additional tools and context from different data sources and applications.
You can dynamically enable or disable extensions as needed to help complete tasks.

{{% if (extensions is defined) and extensions %}}
Because you dynamically load extensions, your conversation history may refer
to interactions with extensions that are not currently active. The currently
active extensions are below. Each of these extensions provides tools that are
in your tool specification.

{{% for extension in extensions %}}

## {{{{extension.name}}}}

{{% if extension.has_resources %}}
{{{{extension.name}}}} supports resources.
{{% endif %}}
{{% if extension.instructions %}}### Instructions
{{{{extension.instructions}}}}{{% endif %}}
{{% endfor %}}

{{% else %}}
No extensions are defined.
{{% endif %}}
{{% endif %}}

# Response Guidelines

Use Markdown formatting for all responses.
"#,
            role = role.label(),
            role_desc = crate::chat::team::role_description(role),
            project = project,
        )
    }

    /// The "corpus-team" FRONTEND extension: declares the `delegate` tool to
    /// the orchestrator model; EXECUTION is ours — goose yields frontend tool
    /// calls to the client and waits for `Agent::handle_tool_result`. The
    /// delegate spawns the specialist in-process with `available_tools` =
    /// exactly its domain ([`crate::chat::team::TeamRole::admin_tools`]), so
    /// a specialist is scoped BY CONSTRUCTION and the destructive set is
    /// unreachable for every delegate.
    pub(super) fn build_team_extension() -> goose::agents::ExtensionConfig {
        let roles: Vec<serde_json::Value> = crate::chat::team::DELEGATABLE_ROLES
            .iter()
            .map(|r| serde_json::Value::String(r.label().to_string()))
            .collect();
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "role": {
                    "type": "string",
                    "enum": roles,
                    "description": "the specialist to run the task"
                },
                "instructions": {
                    "type": "string",
                    "description": "a complete, self-contained task brief — the specialist sees none of this conversation"
                }
            },
            "required": ["role", "instructions"]
        });
        let tool = rmcp::model::Tool::new(
            "delegate".to_string(),
            "Delegate a sub-task to a scoped corpus specialist agent. The specialist runs with ONLY its own domain's corpus-admin tools (by construction) and returns a report. Use this for ALL store reads/writes — you hold no admin tools yourself."
                .to_string(),
            schema.as_object().expect("delegate schema is an object").clone(),
        );
        goose::agents::ExtensionConfig::Frontend {
            name: "corpus-team".into(),
            description: "Delegate sub-tasks to scoped corpus specialist agents".into(),
            tools: vec![tool],
            instructions: Some(team_instructions()),
            bundled: Some(true),
            available_tools: Vec::new(),
        }
    }

    /// The orchestrator's delegation instructions (the frontend extension's
    /// `instructions` — rendered into the system prompt by goose).
    fn team_instructions() -> String {
        let mut s = String::from(
            "You co-ordinate the corpus management team. You hold NO corpus-admin tools yourself; every store read or write goes through the `delegate` tool with the right specialist:\n",
        );
        for r in crate::chat::team::DELEGATABLE_ROLES {
            s.push_str(&format!(
                "- {}: {}\n",
                r.label(),
                crate::chat::team::role_description(*r)
            ));
        }
        s.push_str(
            "Give the specialist a complete, self-contained brief (it sees none of this conversation), then report its result back. Destructive operations (corpus_wipe, project_delete, agent_delete, mission_delete) are impossible for every specialist by construction — if the operator asks for one, tell them to switch the chat role to operator.",
        );
        s
    }

    /// The `corpus-mcp --admin` stdio extension. `Operator` passes an empty
    /// `available_tools` (= all tools, still approval-gated); a specialist
    /// passes exactly its scoped domain from [`crate::chat::team::TeamRole`],
    /// so goose's `is_tool_available` refuses every out-of-domain / destructive
    /// tool BY CONSTRUCTION.
    pub(super) fn build_corpus_admin_extension(role: TeamRole) -> anyhow::Result<goose::agents::ExtensionConfig> {
        let available_tools = role.admin_tools();
        Ok(goose::agents::ExtensionConfig::Stdio {
            name: "corpus-admin".into(),
            description: DEFAULT_EXTENSION_DESCRIPTION.into(),
            cmd: corpus_mcp_path(),
            args: vec!["--admin".into()],
            envs: Default::default(),
            env_keys: Vec::new(),
            timeout: Some(DEFAULT_EXTENSION_TIMEOUT),
            cwd: None,
            bundled: Some(true),
            available_tools,
        })
    }

    /// Locate the corpus-mcp binary. Order: explicit `CORPUS_MCP` override;
    /// next to the running executable (app: `target/<profile>/corpus-mcp`;
    /// tests: `target/<profile>/deps/../corpus-mcp`); last resort the
    /// build-time workspace target dir. (The old runtime `CARGO_MANIFEST_DIR`
    /// read falls back to cwd-relative `"."` in a packaged app — wrong.)
    fn corpus_mcp_path() -> String {
        if let Ok(p) = std::env::var("CORPUS_MCP") {
            return p;
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let sibling = dir.join("corpus-mcp");
                if sibling.exists() {
                    return sibling.to_string_lossy().into_owned();
                }
                let up = dir.join("../corpus-mcp");
                if up.exists() {
                    return up.to_string_lossy().into_owned();
                }
            }
        }
        format!("{}/../../target/debug/corpus-mcp", env!("CARGO_MANIFEST_DIR"))
    }

    /// Run one turn: `agent.reply(...)` then translate the streamed
    /// [`AgentEvent`]s into our [`ChatEvent`]s. Does NOT emit `TurnEnd` —
    /// the command loop owns turn lifecycle (serialization + queued
    /// handoff); this task only streams content.
    async fn translate_turn(
        agent: Arc<Agent>,
        message: String,
        session_cfg: SessionConfig,
        cancel: CancellationToken,
        ev: StdSender<ChatEvent>,
        log: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>,
        pending: PendingApprovals,
        project: String,
        model: String,
        turn: u64,
    ) {
        let user = Message::user().with_text(message);
        let stream = match agent.reply(user, session_cfg, Some(cancel.clone())).await {
            Ok(s) => s,
            Err(e) => {
                log_line(&log, &format!("turn {turn} reply failed: {e}"));
                let _ = ev.send(ChatEvent::Error(format!("reply failed: {e}")));
                return;
            }
        };
        let mut stream = Box::pin(stream);
        // Tool call id → tool name, for this turn: a successful WRITE tool
        // emits StoreMutated (the app's nav refresh) — ToolResponse carries
        // no name, only the id.
        let mut call_names: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        loop {
            // Race the next stream item against the cancel token so Stop
            // bails immediately: on cancel we drop the stream future, which
            // closes the provider connection and halts the local generation
            // — no waiting on goose to notice, no 900 s stream-timeout hang.
            let item = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    log_line(&log, &format!("turn {turn} cancelled mid-stream"));
                    break;
                }
                item = stream.next() => match item {
                    Some(item) => item,
                    None => break,
                },
            };
            match item {
                Ok(AgentEvent::Message(msg)) => {
                    // Delegate execution: a `delegate` FRONTEND tool call is
                    // ours to run — goose yields it and parks the turn until
                    // we answer via handle_tool_result. Spawn the specialist
                    // concurrently (the reply stream is mid-wait).
                    for block in &msg.content {
                        match block {
                            MessageContentBlock::FrontendToolRequest(req) => {
                                if let Ok(params) = &req.tool_call {
                                    if params.name.as_ref() == "delegate" {
                                        spawn_delegate(
                                            agent.clone(),
                                            &cancel,
                                            project.clone(),
                                            model.clone(),
                                            ev.clone(),
                                            log.clone(),
                                            pending.clone(),
                                            req.id.clone(),
                                            serde_json::to_value(&params.arguments)
                                                .unwrap_or(serde_json::Value::Null),
                                        );
                                    }
                                }
                            }
                            // Approval policy (chat::team::needs_approval):
                            // read-only tools are released IN-PROCESS without
                            // troubling the operator — a smart agent reads
                            // freely; only writes/destructive ops surface an
                            // Approve/Reject card (translate_message skips
                            // those too, via the same policy).
                            MessageContentBlock::ActionRequired(action) => {
                                if let ActionRequiredData::ToolConfirmation {
                                    id, tool_name, ..
                                } = &action.data
                                {
                                    if !crate::chat::team::needs_approval(tool_name) {
                                        let agent = agent.clone();
                                        let id = id.clone();
                                        let tool_name = tool_name.clone();
                                        let log = log.clone();
                                        tokio::spawn(async move {
                                            log_line(&log, &format!("auto-approved read-only tool: {tool_name}"));
                                            let _ = agent
                                                .tool_confirmation_router
                                                .deliver(
                                                    id,
                                                    PermissionConfirmation {
                                                        principal_type: PrincipalType::Tool,
                                                        permission: Permission::AllowOnce,
                                                    },
                                                )
                                                .await;
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    translate_message(msg, &ev, turn, &mut call_names);
                }
                Ok(AgentEvent::Usage(u)) => {
                    log_line(
                        &log,
                        &format!(
                            "turn {turn} usage: in={:?} out={:?} total={:?}",
                            u.usage.input_tokens, u.usage.output_tokens, u.usage.total_tokens
                        ),
                    );
                    let _ = ev.send(ChatEvent::Usage {
                        input_tokens: u.usage.input_tokens,
                        output_tokens: u.usage.output_tokens,
                        total_tokens: u.usage.total_tokens,
                    });
                }
                Ok(AgentEvent::MessageUsage { .. })
                | Ok(AgentEvent::McpNotification(_))
                | Ok(AgentEvent::HistoryReplaced(_)) => {}
                Err(e) => {
                    log_line(&log, &format!("turn {turn} stream error: {e}"));
                    let _ = ev.send(ChatEvent::Error(format!("reply stream: {e}")));
                }
            }
        }
    }

    /// Map a goose message's content blocks onto our event vocabulary:
    /// text → [`ChatEvent::TextChunk`], tool request → [`ChatEvent::ToolCallStart`],
    /// tool response → [`ChatEvent::ToolCallResult`] (+ [`ChatEvent::StoreMutated`]
    /// when a WRITE tool succeeds), and a pending tool
    /// confirmation → [`ChatEvent::PermissionRequest`] (released later by the
    /// command loop's Approve/Reject via the confirmation router).
    fn translate_message(
        msg: Message,
        ev: &StdSender<ChatEvent>,
        turn: u64,
        call_names: &mut std::collections::HashMap<String, String>,
    ) {
        for block in msg.content {
            match block {
                MessageContentBlock::Text(text) => {
                    let _ = ev.send(ChatEvent::TextChunk {
                        turn,
                        delta: text.text,
                    });
                }
                MessageContentBlock::Thinking(thinking) => {
                    // The model's thought process — the panel renders it as a
                    // collapsible thought card, chronological with text/tools.
                    let _ = ev.send(ChatEvent::ThinkingChunk {
                        turn,
                        delta: thinking.thinking,
                    });
                }
                MessageContentBlock::ToolRequest(req) => {
                    if let Ok(params) = req.tool_call {
                        call_names.insert(req.id.clone(), params.name.to_string());
                        let _ = ev.send(ChatEvent::ToolCallStart {
                            id: req.id.clone(),
                            name: params.name.to_string(),
                            args_json: serde_json::to_string(&params.arguments)
                                .unwrap_or_else(|_| "{}".into()),
                        });
                    }
                }
                MessageContentBlock::ToolResponse(resp) => {
                    let (is_error, output) = match resp.tool_result {
                        Ok(result) => {
                            let text = result
                                .content
                                .iter()
                                .filter_map(|b| b.as_text().map(|t| t.text.clone()))
                                .collect::<Vec<_>>()
                                .join("\n");
                            (result.is_error.unwrap_or(false), text)
                        }
                        Err(err) => (true, format!("tool error: {}", err.message)),
                    };
                    // A successful WRITE tool mutates the store — tell the
                    // app to refresh its nav.
                    if !is_error {
                        if let Some(name) = call_names.get(&resp.id) {
                            if let Some(area) = crate::chat::team::mutated_area(name) {
                                let _ = ev.send(ChatEvent::StoreMutated { area });
                            }
                        }
                    }
                    let _ = ev.send(ChatEvent::ToolCallResult {
                        id: resp.id.clone(),
                        is_error,
                        output,
                    });
                }
                MessageContentBlock::FrontendToolRequest(req) => {
                    // The delegate call, rendered as a tool card; it
                    // completes when run_specialist's ToolCallResult lands.
                    if let Ok(params) = &req.tool_call {
                        let args = serde_json::to_value(&params.arguments)
                            .unwrap_or(serde_json::Value::Null);
                        let role = args.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                        let _ = ev.send(ChatEvent::ToolCallStart {
                            id: req.id.clone(),
                            name: format!("delegate › {role}"),
                            args_json: serde_json::to_string(&params.arguments)
                                .unwrap_or_else(|_| "{}".into()),
                        });
                    }
                }
                MessageContentBlock::ActionRequired(action) => {
                    if let ActionRequiredData::ToolConfirmation {
                        id,
                        tool_name,
                        arguments,
                        prompt,
                    } = action.data
                    {
                        // Approval policy: read-only tools are released
                        // in-process by translate_turn — their card never
                        // reaches the panel.
                        if !crate::chat::team::needs_approval(&tool_name) {
                            continue;
                        }
                        // Surface the dry-run summary (the panel's inline
                        // Approve/Reject). The tool does NOT run until the
                        // operator approves via tool_confirmation_router.
                        let _ = ev.send(ChatEvent::PermissionRequest {
                            id,
                            tool: tool_name,
                            args_json: serde_json::to_string(&arguments)
                                .unwrap_or_else(|_| "{}".into()),
                            summary: prompt.unwrap_or_default(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Execute one `delegate` frontend tool call: run the specialist
    /// in-process, then answer the orchestrator's parked turn via
    /// `handle_tool_result`. The specialist runs on a CHILD cancellation
    /// token — the panel's stop button cuts both.
    fn spawn_delegate(
        orchestrator: Arc<Agent>,
        parent_cancel: &CancellationToken,
        project: String,
        model: String,
        ev: StdSender<ChatEvent>,
        log: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>,
        pending: PendingApprovals,
        call_id: String,
        args: serde_json::Value,
    ) {
        let role = args
            .get("role")
            .and_then(|v| v.as_str())
            .and_then(crate::chat::team::role_from_label);
        let instructions = args
            .get("instructions")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cancel = parent_cancel.child_token();
        tokio::spawn(async move {
            let result = match role {
                Some(role) if crate::chat::team::DELEGATABLE_ROLES.contains(&role) => {
                    run_specialist(
                        role,
                        instructions,
                        project,
                        model,
                        ev.clone(),
                        log,
                        pending,
                        call_id.clone(),
                        cancel,
                    )
                    .await
                }
                other => Err(format!(
                    "delegate refused: {other:?} is not a delegatable specialist (one of: {})",
                    crate::chat::team::DELEGATABLE_ROLES
                        .iter()
                        .map(|r| r.label())
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            };
            let (is_error, report) = match result {
                Ok(report) => (false, report),
                Err(e) => (true, e),
            };
            // Complete the panel's delegate card, then release the
            // orchestrator's parked turn with the specialist's report.
            let _ = ev.send(ChatEvent::ToolCallResult {
                id: call_id.clone(),
                is_error,
                output: report.clone(),
            });
            let content = rmcp::model::ContentBlock::text(report);
            let tool_result = if is_error {
                Ok(rmcp::model::CallToolResult::error(vec![content]))
            } else {
                Ok(rmcp::model::CallToolResult::success(vec![content]))
            };
            orchestrator.handle_tool_result(call_id, tool_result).await;
        });
    }

    /// Run one specialist agent to completion and return its report. The
    /// specialist is a FULL goose Agent on its own session (SubAgent type)
    /// with the corpus-admin extension scoped to exactly its domain. The
    /// SAME approval policy as the main session applies: read-only tools
    /// auto-release in-process; write tools surface an Approve/Reject card
    /// in the panel (routed back to THIS agent via `pending`); the
    /// destructive set is unreachable by construction. Its tool calls stream
    /// to the panel as `role›tool` cards keyed under the parent delegate
    /// call.
    async fn run_specialist(
        role: TeamRole,
        instructions: String,
        project: String,
        model: String,
        ev: StdSender<ChatEvent>,
        log: Option<std::sync::Arc<std::sync::Mutex<std::fs::File>>>,
        pending: PendingApprovals,
        parent_call_id: String,
        cancel: CancellationToken,
    ) -> Result<String, String> {
        log_line(
            &log,
            &format!("delegate › {role} start: {}", crate::chat::truncate(&instructions, 120)),
        );
        let scope = project_scope(&project);
        let session_mgr = SessionManager::new(scope.join("data"));
        let permission_mgr = PermissionManager::instance();
        let mut config = AgentConfig::new(
            Arc::new(session_mgr),
            permission_mgr,
            None,
            GooseMode::Approve,
            false,
            GoosePlatform::GooseCli,
        );
        config.is_subagent = true;
        let agent = Arc::new(Agent::with_config(config));

        let provider = goose::providers::create_with_named_model("ollama", vec![])
            .await
            .map_err(|e| format!("specialist provider: {e}"))?;
        let model_config = goose::model_config::model_config_from_user_config("ollama", &model)
            .map_err(|e| format!("specialist model config: {e}"))?;
        let session = agent
            .config
            .session_manager
            .create_session(
                scope.clone(),
                format!("delegate › {role}"),
                SessionType::SubAgent,
                GooseMode::Approve,
            )
            .await
            .map_err(|e| format!("specialist session: {e}"))?;
        agent
            .update_provider(provider, model_config, &session.id)
            .await
            .map_err(|e| format!("specialist provider update: {e}"))?;
        agent
            .override_system_prompt(corpus_system_prompt(role, &project))
            .await;
        let ext = build_corpus_admin_extension(role)
            .map_err(|e| format!("specialist extension: {e}"))?;
        agent
            .add_extension(ext, &session.id)
            .await
            .map_err(|e| format!("specialist extension register: {e}"))?;

        let user = Message::user().with_text(instructions);
        let session_cfg = SessionConfig {
            id: session.id.clone(),
            schedule_id: None,
            // A runaway specialist loops on its own dime; cap the iterations.
            max_turns: Some(15),
            retry_config: None,
        };
        let stream = agent
            .reply(user, session_cfg, Some(cancel))
            .await
            .map_err(|e| format!("specialist reply: {e}"))?;
        let mut stream = Box::pin(stream);
        let mut report = String::new();
        let mut n_calls = 0u32;
        // The specialist's tool-response ids map back to our synthetic
        // per-call ids (panel cards are keyed by id) — value is
        // (synthetic card id, bare tool name) for the StoreMutated check.
        let mut call_ids: std::collections::HashMap<String, (String, String)> =
            std::collections::HashMap::new();
        // Confirmation ids WE registered (removed on exit — a cancelled turn
        // must not leak stale approvals into the router).
        let mut registered: Vec<String> = Vec::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(AgentEvent::Message(msg)) => {
                    for block in msg.content {
                        match block {
                            MessageContentBlock::Text(t) => report.push_str(&t.text),
                            MessageContentBlock::ToolRequest(req) => {
                                if let Ok(params) = req.tool_call {
                                    n_calls += 1;
                                    let synthetic = format!("{parent_call_id}:{n_calls}");
                                    call_ids.insert(
                                        req.id.clone(),
                                        (synthetic.clone(), params.name.to_string()),
                                    );
                                    let _ = ev.send(ChatEvent::ToolCallStart {
                                        id: synthetic,
                                        name: format!("{role}›{}", params.name),
                                        args_json: serde_json::to_string(&params.arguments)
                                            .unwrap_or_else(|_| "{}".into()),
                                    });
                                }
                            }
                            MessageContentBlock::ToolResponse(resp) => {
                                let (is_error, output) = match resp.tool_result {
                                    Ok(result) => {
                                        let text = result
                                            .content
                                            .iter()
                                            .filter_map(|b| b.as_text().map(|t| t.text.clone()))
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        (result.is_error.unwrap_or(false), text)
                                    }
                                    Err(err) => (true, format!("tool error: {}", err.message)),
                                };
                                if let Some((synthetic, tool_name)) = call_ids.get(&resp.id) {
                                    // A successful specialist WRITE mutates
                                    // the store — same nav-refresh signal.
                                    if !is_error {
                                        if let Some(area) = crate::chat::team::mutated_area(tool_name) {
                                            let _ = ev.send(ChatEvent::StoreMutated { area });
                                        }
                                    }
                                    let _ = ev.send(ChatEvent::ToolCallResult {
                                        id: synthetic.clone(),
                                        is_error,
                                        output,
                                    });
                                }
                            }
                            MessageContentBlock::ActionRequired(action) => {
                                if let ActionRequiredData::ToolConfirmation {
                                    id,
                                    tool_name,
                                    arguments,
                                    prompt,
                                } = action.data
                                {
                                    if crate::chat::team::needs_approval(&tool_name) {
                                        // A specialist WRITE: surface the
                                        // card and park until the operator's
                                        // decision routes back to THIS agent.
                                        pending.lock().unwrap().insert(id.clone(), agent.clone());
                                        registered.push(id.clone());
                                        let _ = ev.send(ChatEvent::PermissionRequest {
                                            id,
                                            tool: format!("{role}›{tool_name}"),
                                            args_json: serde_json::to_string(&arguments)
                                                .unwrap_or_else(|_| "{}".into()),
                                            summary: prompt.unwrap_or_default(),
                                        });
                                    } else {
                                        // Read-only: release in-process.
                                        let agent = agent.clone();
                                        let log = log.clone();
                                        tokio::spawn(async move {
                                            log_line(&log, &format!("auto-approved read-only tool: {tool_name}"));
                                            let _ = agent
                                                .tool_confirmation_router
                                                .deliver(
                                                    id,
                                                    PermissionConfirmation {
                                                        principal_type: PrincipalType::Tool,
                                                        permission: Permission::AllowOnce,
                                                    },
                                                )
                                                .await;
                                        });
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Ok(AgentEvent::Usage(u)) => {
                    let _ = ev.send(ChatEvent::Usage {
                        input_tokens: u.usage.input_tokens,
                        output_tokens: u.usage.output_tokens,
                        total_tokens: u.usage.total_tokens,
                    });
                }
                Ok(AgentEvent::MessageUsage { .. })
                | Ok(AgentEvent::McpNotification(_))
                | Ok(AgentEvent::HistoryReplaced(_)) => {}
                Err(e) => {
                    log_line(&log, &format!("delegate › {role} stream error: {e}"));
                }
            }
        }
        log_line(
            &log,
            &format!("delegate › {role} end ({n_calls} tool calls, {} chars)", report.len()),
        );
        // Drop any un-resolved registrations (cancelled turn / abandoned
        // confirmation) so the router can't route a stale id to a dead agent.
        {
            let mut p = pending.lock().unwrap();
            for id in &registered {
                p.remove(id);
            }
        }
        if report.trim().is_empty() {
            report = "(specialist returned no report)".to_string();
        }
        Ok(report)
    }
}

#[cfg(all(test, feature = "chat-embed"))]
mod injection_probe {
    //! The deciding chunk-2 evidence: the scoped `corpus-admin` extension
    //! refuses an out-of-domain / destructive tool BY CONSTRUCTION — goose's
    //! own `ExtensionConfig::is_tool_available` returns false for a tool that
    //! is not listed in the scope's `available_tools`, independent of anything
    //! an instruction could tell the model. No Ollama needed; deterministic.

    use super::live::build_corpus_admin_extension;
    use crate::chat::team::TeamRole;

    #[test]
    fn inspector_extension_withholds_project_delete() {
        std::env::set_var("CARGO_MANIFEST_DIR", "/nonexistent");
        std::env::set_var("CORPUS_MCP", "/nonexistent/corpus-mcp");
        let ext = build_corpus_admin_extension(TeamRole::CorpusInspector)
            .expect("inspector extension builds");
        // The chain-destroying tool is withheld by construction.
        assert!(!ext.is_tool_available("project_delete"));
        // And so are all the other destructive tools.
        for t in crate::chat::team::DESTRUCTIVE_TOOLS {
            assert!(!ext.is_tool_available(t), "{t} must be withheld from inspector");
        }
        // The inspector's own read tool is available.
        assert!(ext.is_tool_available("corpus_read"));
        assert!(ext.is_tool_available("mission_list"));
    }

    #[test]
    fn specialist_extensions_never_grant_the_destructive_set() {
        for role in crate::chat::team::SPECIALIST_ROLES {
            if *role == TeamRole::CorpusInspector {
                continue; // covered above
            }
            if *role == TeamRole::Orchestrator {
                // Orchestrator registers NO extension; nothing to build.
                continue;
            }
            let ext = build_corpus_admin_extension(*role).unwrap_or_else(|_| {
                panic!("{role} extension builds")
            });
            for t in crate::chat::team::DESTRUCTIVE_TOOLS {
                assert!(
                    !ext.is_tool_available(t),
                    "{role} must withhold {t} by construction"
                );
            }
        }
    }

    #[test]
    fn orchestrator_delegates_through_the_team_frontend_extension() {
        use goose::agents::ExtensionConfig;
        let ext = super::live::build_team_extension();
        // It must be a FRONTEND extension named "corpus-team" — the tool is
        // declared to the model but executed by US (the summon platform
        // extension was dropped: its subagents inherit only parent-session
        // extensions, so per-specialist scoping was impossible).
        let (name, tools, instructions) = match &ext {
            ExtensionConfig::Frontend { name, tools, instructions, .. } => {
                (name.as_str(), tools, instructions)
            }
            other => panic!("orchestrator should load the Frontend corpus-team extension, got {other:?}"),
        };
        assert_eq!(name, "corpus-team");
        assert_eq!(tools.len(), 1, "exactly one tool: delegate");
        let tool = &tools[0];
        assert_eq!(tool.name.as_ref(), "delegate");
        // The role enum in the schema is EXACTLY the delegatable set — no
        // Operator, no Orchestrator (no self-delegation loops).
        let schema = serde_json::to_value(&tool.input_schema).expect("schema serializes");
        let roles: Vec<&str> = schema["properties"]["role"]["enum"]
            .as_array()
            .expect("role enum is an array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        let expected: Vec<&str> = crate::chat::team::DELEGATABLE_ROLES
            .iter()
            .map(|r| r.label())
            .collect();
        assert_eq!(roles, expected);
        assert!(!roles.contains(&"operator"));
        assert!(!roles.contains(&"orchestrator"));
        // The instructions name every specialist and forbid destruction.
        let instr = instructions.as_ref().expect("team instructions present");
        for r in crate::chat::team::DELEGATABLE_ROLES {
            assert!(instr.contains(r.label()), "instructions must name {r}");
        }
        for t in crate::chat::team::DESTRUCTIVE_TOOLS {
            assert!(instr.contains(t), "instructions must credit {t} as impossible");
        }
    }

    #[test]
    fn system_prompt_override_names_corpus_not_goose() {
        // The "our agent thinks it's goose" regression probe: the override
        // template carries the corpus identity (role + project formatted
        // in), never goose's stock identity line, and the format!-escaped
        // miniJinja tags survive intact for goose's render pass.
        let t = super::live::corpus_system_prompt(TeamRole::Orchestrator, "default");
        assert!(t.contains("corpus management assistant"));
        assert!(!t.contains("You are a general-purpose AI agent called goose"));
        assert!(t.contains("**orchestrator**"));
        assert!(t.contains("\"default\""));
        assert!(t.contains("{% for extension in extensions %}"));
        assert!(t.contains("{{ current_date_time }}"));
        assert!(t.contains("{{extension.name}}"));
    }

    /// Serializes the live probes: they mutate PROCESS-GLOBAL env
    /// (CORPUS_STORE, CORPUS_MCP, GOOSE_PATH_ROOT) and would stomp each
    /// other under cargo's parallel test threads.
    static LIVE_PROBE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The probe chat model: `CORPUS_PROBE_MODEL` wins; else the first of
    /// the preferred small models the local Ollama actually has (the
    /// operator's model garden changes — a hardcoded name made the probes
    /// rot). Panics with the available list when none fits.
    fn probe_model() -> String {
        let available: Vec<String> = corpus_core::ollama_models()
            .expect("ollama must be running for the live probe")
            .groups
            .into_iter()
            .flat_map(|g| g.models.into_iter().map(|m| m.model))
            .collect();
        if let Ok(m) = std::env::var("CORPUS_PROBE_MODEL") {
            assert!(available.iter().any(|a| a == &m), "CORPUS_PROBE_MODEL={m} not pulled; available: {available:?}");
            return m;
        }
        for preferred in ["qwen3.5:9b", "gemma4:e4b", "qwen3.8:27b-mlx"] {
            if available.iter().any(|a| a == preferred) {
                return preferred.to_string();
            }
        }
        panic!("no preferred probe model pulled; set CORPUS_PROBE_MODEL to one of: {available:?}");
    }

    /// END-TO-END LIVE PROBE (opt-in; needs Ollama + a built corpus-mcp):
    /// drives the REAL embedded backend against a small local model through
    /// the public ChatHandle seam — identity, tool use, the approval gate,
    /// and a real store mutation — the repeatable "use the harness to create
    /// projects and agents" check the operator asked for. Run:
    ///
    /// ```sh
    /// cargo build -p corpus-mcp -p corpus-app &&
    /// cargo test -p corpus-app --bin corpus-app live_end_to_end -- --ignored --nocapture
    /// ```
    ///
    /// Uses a throwaway CORPUS_STORE in the temp dir (never the real store)
    /// and the fast qwen3.5:9b chat model.
    #[test]
    #[ignore = "live probe: needs Ollama (qwen3.5:9b) and a built corpus-mcp"]
    fn live_end_to_end_operator_creates_project() {
        use crate::chat::{Chat, ChatEvent, ChatHandle, ChatPhase};

        let _guard = LIVE_PROBE_LOCK.lock().unwrap();
        // --- preconditions (skip loudly, don't fail the suite) ---
        let store = std::env::temp_dir().join(format!("corpus-live-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        std::fs::create_dir_all(&store).expect("probe store dir");
        std::env::set_var("CORPUS_STORE", &store);
        super::init_goose_env();
        let mcp = {
            let exe = std::env::current_exe().expect("current exe");
            let candidate = exe.parent().unwrap().join("../corpus-mcp");
            if candidate.exists() {
                candidate
            } else {
                panic!("corpus-mcp not built (expected at {candidate:?}) — cargo build -p corpus-mcp first");
            }
        };
        std::env::set_var("CORPUS_MCP", &mcp);
        // Ollama reachable + model pulled?
        let model = probe_model();

        let project = "liveprobe";
        let mut chat = ChatHandle::start_scoped(project, &model, crate::chat::team::TeamRole::Operator);

        // --- drive the session: wait Ready, send prompts, auto-approve ---
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut ready = false;
        let mut turn_open = false;
        let mut phase_idx = 0;
        let probe_project = format!("probe{}", std::process::id());
        let prompts = [
            "In one short sentence: what are you, and who made you?",
            &format!(
                "Use your project_new tool to create a corpus project named \"{probe_project}\" bound to the cdk-regtest plugin. Then confirm with project_list."
            ),
        ];
        let mut transcript = String::new();
        let mut sent_current = false;
        // Approval-policy accounting: which tools asked, which didn't.
        let mut approvals: Vec<String> = Vec::new();
        // A turn counts as drained only after ITS TurnStart was seen and its
        // TurnEnd arrived — otherwise the send→poll gap reads as "done"
        // before the model even runs (probe-side race).
        let mut saw_start = false;
        while std::time::Instant::now() < deadline {
            for ev in chat.poll_events() {
                match ev {
                    ChatEvent::Ready { .. } => ready = true,
                    ChatEvent::TurnStart { .. } => {
                        turn_open = true;
                        saw_start = true;
                    }
                    ChatEvent::TextChunk { delta, .. } => transcript.push_str(&delta),
                    ChatEvent::ThinkingChunk { .. } => {}
                    ChatEvent::ToolCallStart { name, .. } => {
                        eprintln!("[probe] tool › {name}");
                        transcript.push_str(&format!("\n[tool: {name}]\n"));
                    }
                    ChatEvent::ToolCallResult { .. } => {}
                    ChatEvent::PermissionRequest { id, tool, .. } => {
                        eprintln!("[probe] auto-approving {tool}");
                        approvals.push(tool.clone());
                        chat.approve(&id);
                    }
                    ChatEvent::TurnEnd { .. } => turn_open = false,
                    ChatEvent::Stopped { .. } => turn_open = false,
                    ChatEvent::Usage { .. } => {}
                    ChatEvent::StoreMutated { .. } => {}
                    ChatEvent::Error(e) => panic!("backend error during probe: {e}"),
                }
            }
            if ready && !turn_open && phase_idx < prompts.len() && !sent_current {
                eprintln!("[probe] send: {}", prompts[phase_idx]);
                chat.send(prompts[phase_idx]);
                sent_current = true;
            }
            if sent_current && saw_start && !turn_open {
                // Turn drained.
                phase_idx += 1;
                sent_current = false;
                saw_start = false;
            }
            if phase_idx >= prompts.len() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        assert!(phase_idx >= prompts.len(), "probe timed out; transcript so far:\n{transcript}");
        assert_eq!(chat.phase(), ChatPhase::Ready);

        // --- 1. identity: the agent is corpus's, never goose ---
        let first_reply_end = transcript.find("[tool:").unwrap_or(transcript.len());
        let first_reply = transcript[..first_reply_end].to_lowercase();
        assert!(
            !first_reply.contains("i'm goose") && !first_reply.contains("i am goose"),
            "identity regression — the agent called itself goose: {first_reply}"
        );

        // --- 2. the store mutation actually landed ---
        let created = store.join("projects").join(&probe_project);
        assert!(
            created.exists(),
            "project_new never landed on disk at {created:?}; transcript:\n{transcript}"
        );
        // --- 3. the approval policy held: the WRITE asked, the READ didn't ---
        assert!(
            approvals.iter().any(|t| t.contains("project_new")),
            "project_new (write) must require approval; approvals seen: {approvals:?}"
        );
        assert!(
            !approvals.iter().any(|t| t.contains("project_list")),
            "project_list (read-only) must NOT require approval; approvals seen: {approvals:?}"
        );
        eprintln!("[probe] OK — identity clean, project created at {created:?}");
        let _ = std::fs::remove_dir_all(&store);
    }

    /// LIVE PROBE 2 (opt-in): the ORCHESTRATOR delegation path — the model
    /// must call our `delegate` frontend tool, the specialist must run with
    /// its scoped corpus-admin tools (the thing summon could never do), and
    /// the mutation must land. Same preconditions as probe 1.
    #[test]
    #[ignore = "live probe: needs Ollama (qwen3.5:9b) and a built corpus-mcp"]
    fn live_end_to_end_orchestrator_delegates() {
        use crate::chat::{Chat, ChatEvent, ChatHandle};

        let _guard = LIVE_PROBE_LOCK.lock().unwrap();
        let store = std::env::temp_dir().join(format!("corpus-live-probe-orch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        std::fs::create_dir_all(&store).expect("probe store dir");
        std::env::set_var("CORPUS_STORE", &store);
        super::init_goose_env();
        let exe = std::env::current_exe().expect("current exe");
        let mcp = exe.parent().unwrap().join("../corpus-mcp");
        assert!(mcp.exists(), "corpus-mcp not built — cargo build -p corpus-mcp first");
        std::env::set_var("CORPUS_MCP", &mcp);
        let model = probe_model();

        let mut chat = ChatHandle::start_scoped(
            "liveprobe",
            &model,
            crate::chat::team::TeamRole::Orchestrator,
        );
        let probe_project = format!("orchprobe{}", std::process::id());
        let prompt = format!(
            "Create a corpus project named \"{probe_project}\" bound to the cdk-regtest plugin. You hold no admin tools — use the delegate tool with the right specialist."
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut ready = false;
        let mut turn_open = false;
        let mut saw_start = false;
        let mut sent = false;
        let mut delegated = false;
        let mut specialist_tool = false;
        let mut transcript = String::new();
        while std::time::Instant::now() < deadline {
            for ev in chat.poll_events() {
                match ev {
                    ChatEvent::Ready { .. } => ready = true,
                    ChatEvent::TurnStart { .. } => {
                        turn_open = true;
                        saw_start = true;
                    }
                    ChatEvent::TextChunk { delta, .. } => transcript.push_str(&delta),
                    ChatEvent::ThinkingChunk { .. } => {}
                    ChatEvent::ToolCallStart { name, .. } => {
                        eprintln!("[probe] tool › {name}");
                        if name.starts_with("delegate ›") {
                            delegated = true;
                        }
                        if name.contains('›') && !name.starts_with("delegate ›") {
                            specialist_tool = true;
                        }
                        transcript.push_str(&format!("\n[tool: {name}]\n"));
                    }
                    ChatEvent::ToolCallResult { .. } => {}
                    ChatEvent::PermissionRequest { id, tool, .. } => {
                        eprintln!("[probe] auto-approving {tool}");
                        chat.approve(&id);
                    }
                    ChatEvent::TurnEnd { .. } => turn_open = false,
                    ChatEvent::Stopped { .. } => turn_open = false,
                    ChatEvent::Usage { .. } => {}
                    ChatEvent::StoreMutated { .. } => {}
                    ChatEvent::Error(e) => panic!("backend error during probe: {e}"),
                }
            }
            if ready && !turn_open && !sent {
                eprintln!("[probe] send: {prompt}");
                chat.send(&prompt);
                sent = true;
            }
            if sent && saw_start && !turn_open {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        assert!(sent && saw_start && !turn_open, "probe timed out; transcript:\n{transcript}");
        assert!(delegated, "orchestrator never called delegate; transcript:\n{transcript}");
        assert!(
            specialist_tool,
            "specialist never used a scoped corpus-admin tool; transcript:\n{transcript}"
        );
        let created = store.join("projects").join(&probe_project);
        assert!(
            created.exists(),
            "delegated project_new never landed at {created:?}; transcript:\n{transcript}"
        );
        eprintln!("[probe] OK — orchestrator delegated, specialist executed, project at {created:?}");
        let _ = std::fs::remove_dir_all(&store);
    }

    /// LIVE PROBE 3 (opt-in): the depbot-session REGRESSION — "create an
    /// agent that scans deps for vulns" must succeed through `agent_new` in
    /// a handful of calls with zero tool errors (the 2026-08-14 session
    /// burned ~10 calls and three failures on clone-then-save + JSON-in-
    /// JSON). Budget: ≤ 4 tool calls, 0 errors, doc on disk.
    #[test]
    #[ignore = "live probe: needs Ollama (qwen3.5:9b) and a built corpus-mcp"]
    fn live_regression_depbot_agent_creation() {
        use crate::chat::{Chat, ChatEvent, ChatHandle};

        let _guard = LIVE_PROBE_LOCK.lock().unwrap();
        let store = std::env::temp_dir().join(format!("corpus-live-probe-depbot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&store);
        std::fs::create_dir_all(&store).expect("probe store dir");
        std::env::set_var("CORPUS_STORE", &store);
        super::init_goose_env();
        let exe = std::env::current_exe().expect("current exe");
        let mcp = exe.parent().unwrap().join("../corpus-mcp");
        assert!(mcp.exists(), "corpus-mcp not built — cargo build -p corpus-mcp first");
        std::env::set_var("CORPUS_MCP", &mcp);
        let model = probe_model();

        // Seed the project so a "researcher" base exists to model on.
        let project = "liveprobe";
        let core_store = corpus_core::Store::new(store.clone());
        core_store.create_project(project, "Live Probe", "cdk-regtest").expect("seed project");

        let mut chat = ChatHandle::start_scoped(project, &model, crate::chat::team::TeamRole::Operator);
        let prompt = "Create an agent called depbot that scans dependencies for vulnerabilities, \
                      modeled on the researcher agent (research role: reads sources and the web, \
                      writes findings/techniques/hypotheses, never executes). Use your tools.";

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let mut ready = false;
        let mut turn_open = false;
        let mut saw_start = false;
        let mut sent = false;
        let mut n_calls = 0u32;
        let mut n_errors = 0u32;
        let mut transcript = String::new();
        while std::time::Instant::now() < deadline {
            for ev in chat.poll_events() {
                match ev {
                    ChatEvent::Ready { .. } => ready = true,
                    ChatEvent::TurnStart { .. } => {
                        turn_open = true;
                        saw_start = true;
                    }
                    ChatEvent::TextChunk { delta, .. } => transcript.push_str(&delta),
                    ChatEvent::ThinkingChunk { .. } => {}
                    ChatEvent::ToolCallStart { name, .. } => {
                        n_calls += 1;
                        eprintln!("[probe] tool › {name}");
                        transcript.push_str(&format!("\n[tool: {name}]\n"));
                    }
                    ChatEvent::ToolCallResult { is_error, .. } => {
                        if is_error {
                            n_errors += 1;
                        }
                    }
                    ChatEvent::PermissionRequest { id, tool, .. } => {
                        eprintln!("[probe] auto-approving {tool}");
                        chat.approve(&id);
                    }
                    ChatEvent::TurnEnd { .. } => turn_open = false,
                    ChatEvent::Stopped { .. } => turn_open = false,
                    ChatEvent::Usage { .. } => {}
                    ChatEvent::StoreMutated { area } => {
                        eprintln!("[probe] store mutated: {area}");
                    }
                    ChatEvent::Error(e) => panic!("backend error during probe: {e}"),
                }
            }
            if ready && !turn_open && !sent {
                eprintln!("[probe] send: {prompt}");
                chat.send(prompt);
                sent = true;
            }
            if sent && saw_start && !turn_open {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
        assert!(sent && saw_start && !turn_open, "probe timed out; transcript:\n{transcript}");
        let doc = store
            .join("projects")
            .join(project)
            .join("agents")
            .join("depbot")
            .join("opencode.json");
        assert!(doc.exists(), "depbot never landed at {doc:?}; transcript:\n{transcript}");
        assert_eq!(n_errors, 0, "tool errors during creation; transcript:\n{transcript}");
        assert!(
            n_calls <= 4,
            "tool-call budget blown ({n_calls} > 4) — agent_new should make this short; transcript:\n{transcript}"
        );
        eprintln!("[probe] OK — depbot created in {n_calls} tool calls, 0 errors");
        let _ = std::fs::remove_dir_all(&store);
    }

    #[test]
    fn chat_input_limit_profiles_heavy_models() {
        // No operator override captured in this test process (the OnceLock
        // is only set from main) — the per-model profile applies.
        assert_eq!(super::chat_input_limit("qwen3.8:27b"), "16384");
        assert_eq!(super::chat_input_limit("hf.co/ggml-org/Qwen3.8-27B-GGUF:Q8_0"), "16384");
        assert_eq!(super::chat_input_limit("gpt-oss:120b"), "16384");
        assert_eq!(super::chat_input_limit("qwen3.5:9b"), "32768");
        assert_eq!(super::chat_input_limit("gemma4:e4b"), "32768");
    }

    #[test]
    fn orchestrator_extension_possesses_no_admin_tool() {
        // The orchestrator has no corpus-admin extension at all — prove it has
        // no admin capability by the role manifest (and never register one).
        assert!(TeamRole::Orchestrator.admin_tools().is_empty());
        assert!(!TeamRole::Orchestrator.has_scoped_admin());
    }
}