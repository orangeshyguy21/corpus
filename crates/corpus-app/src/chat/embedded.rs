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
    /// paths, and summon discovery joins against it).
    fn project_scope(project: &str) -> PathBuf {
        let store = std::env::var("CORPUS_STORE").unwrap_or_else(|_| "store".into());
        let base = PathBuf::from(store)
            .join("projects")
            .join(project)
            .join("var/chat");
        if base.is_absolute() {
            base
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(&base))
                .unwrap_or(base)
        }
    }

    async fn run(
        project: String,
        model: String,
        role: TeamRole,
        session_id: String,
        ev_tx: StdSender<ChatEvent>,
        cmd_rx: StdReceiver<ChatCommand>,
    ) {
        // Symbolic log — check this when the panel says "connecting…" forever.
        std::fs::create_dir_all(project_scope(&project)).ok();
        let _log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(project_scope(&project).join("chat.log"));

        let (agent, goose_session) = match setup(&project, &model, role).await {
            Ok(pair) => pair,
            Err(e) => {
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

        let cancel = CancellationToken::new();
        let mut turn: u64 = 0;
        // The operator's current app position, juiced into every turn.
        let context = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

        loop {
            let Some(cmd) = cmd_rx_async.recv().await else {
                break; // sender dropped -> app gone
            };
            match cmd {
                ChatCommand::Send(msg) => {
                    turn += 1;
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
                        id: goose_session.clone(),
                        schedule_id: None,
                        max_turns: None,
                        retry_config: None,
                    };
                    let cancel = cancel.clone();
                    // Each prompt runs its own turn task; the command loop keeps
                    // serving Approve/Reject so a pending tool confirmation is
                    // always releasable (and Stop works mid-turn).
                    tokio::spawn(async move {
                        translate_turn(agent, msg, session_cfg, cancel, ev, turn).await;
                    });
                }
                ChatCommand::Stop => cancel.cancel(),
                ChatCommand::SetContext(ctx) => {
                    *context.lock().unwrap() = ctx;
                }
                ChatCommand::Close => break,
                ChatCommand::Approve { id } => {
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
                }
                ChatCommand::Reject { id } => {
                    let _ = agent
                        .tool_confirmation_router
                        .deliver(
                            id,
                            PermissionConfirmation {
                                principal_type: PrincipalType::Tool,
                                permission: Permission::DenyOnce,
                            },
                        )
                        .await;
                }
            }
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
        // Redirect all goose config/data/state into the project scope.
        std::env::set_var("GOOSE_PATH_ROOT", &scope);
        // Explicit model + explicit context size (num_ctx). Never an ambient
        // default: passes the model by name and forces Ollama's input limit.
        std::env::set_var("GOOSE_MODEL", model);
        std::env::set_var("GOOSE_INPUT_LIMIT", "32768");
        std::env::set_var("GOOSE_TELEMETRY_ENABLED", "false");

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

        match role {
            // The Orchestrator's capability surface is EMPTY by construction:
            // NO admin extension. (an empty `available_tools` means "all tools"
            // in goose, so absence — not an empty filter — is how we withhold
            // them). It delegates to specialists through the summon platform
            // extension (goose's only public subagent hook; it drives
            // `run_subagent_task` internally).
            TeamRole::Orchestrator => {
                // Publish specialist discovery files so summon can delegate to
                // them, in the project scope (next to the session data).
                write_specialist_agents(&scope);
                agent
                    .add_extension(build_summon_extension(), &session.id)
                    .await
                    .map_err(|e| anyhow::anyhow!("could not register the summon (delegation) extension: {e}"))?;
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

    /// The summon platform extension — goose's in-process subagent hook. It
    /// adds the `delegate`/`load` tools; internally it calls (pub(crate))
    /// `run_subagent_task` to run each specialist with its own scoped tools.
    pub(super) fn build_summon_extension() -> goose::agents::ExtensionConfig {
        goose::agents::ExtensionConfig::Platform {
            name: "summon".into(),
            description: "Delegate sub-tasks to scoped specialist agents".into(),
            display_name: None,
            bundled: None,
            available_tools: Vec::new(),
        }
    }

    /// Write a discovery `.md` per specialist into `<scope>/.agents/agents/`
    /// (goose's summon filesystem registry). Each file's frontmatter gives the
    /// name+description summon shows the orchestration model; the body is the
    /// specialist's system prompt. Idempotent; best-effort (non-fatal).
    pub(super) fn write_specialist_agents(scope: &std::path::Path) {
        use crate::chat::team::{TeamRole, DESTRUCTIVE_TOOLS};
        let dir = scope.join(".agents").join("agents");
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        for role in crate::chat::team::SPECIALIST_ROLES {
            if *role == TeamRole::Orchestrator {
                continue;
            }
            let md = format!(
                "---\nname: {role}\ndescription: {desc}\n---\n\nYou are the **{role}** of the corpus management team. You operate ONLY on your scoped corpus-admin tools, by construction. You cannot and must not call: {destructive}. Answer from the store through your tools.\n",
                role = role.label(),
                desc = crate::chat::team::role_description(*role),
                destructive = DESTRUCTIVE_TOOLS.join(", "),
            );
            let _ = std::fs::write(dir.join(format!("{}.md", role.label())), md);
        }
    }

    /// The `corpus-mcp --admin` stdio extension. `Operator` passes an empty
    /// `available_tools` (= all tools, still approval-gated); a specialist
    /// passes exactly its scoped domain from [`crate::chat::team::TeamRole`],
    /// so goose's `is_tool_available` refuses every out-of-domain / destructive
    /// tool BY CONSTRUCTION.
    pub(super) fn build_corpus_admin_extension(role: TeamRole) -> anyhow::Result<goose::agents::ExtensionConfig> {
        let mcp_cmd = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
        let corpus_mcp = std::env::var("CORPUS_MCP")
            .unwrap_or_else(|_| format!("{}/target/debug/corpus-mcp", mcp_cmd));
        let available_tools = role.admin_tools();
        Ok(goose::agents::ExtensionConfig::Stdio {
            name: "corpus-admin".into(),
            description: DEFAULT_EXTENSION_DESCRIPTION.into(),
            cmd: corpus_mcp,
            args: vec!["--admin".into()],
            envs: Default::default(),
            env_keys: Vec::new(),
            timeout: Some(DEFAULT_EXTENSION_TIMEOUT),
            cwd: None,
            bundled: Some(true),
            available_tools,
        })
    }

    /// Run one turn: `agent.reply(...)` then translate the streamed
    /// [`AgentEvent`]s into our [`ChatEvent`]s.
    async fn translate_turn(
        agent: Arc<Agent>,
        message: String,
        session_cfg: SessionConfig,
        cancel: CancellationToken,
        ev: StdSender<ChatEvent>,
        turn: u64,
    ) {
        let user = Message::user().with_text(message);
        let stream = match agent.reply(user, session_cfg, Some(cancel)).await {
            Ok(s) => s,
            Err(e) => {
                let _ = ev.send(ChatEvent::Error(format!("reply failed: {e}")));
                let _ = ev.send(ChatEvent::TurnEnd { turn });
                return;
            }
        };
        let mut stream = Box::pin(stream);
        while let Some(item) = stream.next().await {
            match item {
                Ok(AgentEvent::Message(msg)) => translate_message(msg, &ev, turn),
                Ok(AgentEvent::Usage(_))
                | Ok(AgentEvent::MessageUsage { .. })
                | Ok(AgentEvent::McpNotification(_))
                | Ok(AgentEvent::HistoryReplaced(_)) => {}
                Err(e) => {
                    let _ = ev.send(ChatEvent::Error(format!("reply stream: {e}")));
                }
            }
        }
        let _ = ev.send(ChatEvent::TurnEnd { turn });
    }

    /// Map a goose message's content blocks onto our event vocabulary:
    /// text → [`ChatEvent::TextChunk`], tool request → [`ChatEvent::ToolCallStart`],
    /// tool response → [`ChatEvent::ToolCallResult`], and a pending tool
    /// confirmation → [`ChatEvent::PermissionRequest`] (released later by the
    /// command loop's Approve/Reject via the confirmation router).
    fn translate_message(msg: Message, ev: &StdSender<ChatEvent>, turn: u64) {
        for block in msg.content {
            match block {
                MessageContentBlock::Text(text) => {
                    let _ = ev.send(ChatEvent::TextChunk {
                        turn,
                        delta: text.text,
                    });
                }
                MessageContentBlock::ToolRequest(req) => {
                    if let Ok(params) = req.tool_call {
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
                    let _ = ev.send(ChatEvent::ToolCallResult {
                        id: resp.id.clone(),
                        is_error,
                        output,
                    });
                }
                MessageContentBlock::ActionRequired(action) => {
                    if let ActionRequiredData::ToolConfirmation {
                        id,
                        tool_name,
                        arguments,
                        prompt,
                    } = action.data
                    {
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
    fn orchestrator_delegates_through_the_summon_platform_extension() {
        use goose::agents::ExtensionConfig;
        let ext = super::live::build_summon_extension();
        // It must be a Platform-name-able extension named "summon" — goose's
        // in-process subagent hook (the only public route to run_subagent_task).
        let name = match &ext {
            ExtensionConfig::Platform { name, .. } => name.as_str(),
            other => panic!("orchestrator should load the Platform summon extension, got {other:?}"),
        };
        assert_eq!(name, "summon");
    }

    #[test]
    fn specialist_discovery_files_are_written_and_self_describing() {
        // Write into a throwaway dir (no store/artifact pollution).
        let base = std::env::temp_dir().join(format!(
            "corpus-chat-probe-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        super::live::write_specialist_agents(&base);
        for role in crate::chat::team::SPECIALIST_ROLES {
            if *role == TeamRole::Orchestrator {
                continue;
            }
            let path = base.join(".agents/agents").join(format!("{}.md", role.label()));
            let contents = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("specialist {role} discovery file must exist at {path:?}"));
            assert!(
                contents.contains(&format!("name: {}", role.label())),
                "{role} discovery file must carry its frontmatter name"
            );
            // Self-describing: it must credit the destructive set as forbidden.
            for t in crate::chat::team::DESTRUCTIVE_TOOLS {
                assert!(
                    contents.contains(t),
                    "{role} discovery file must self-describe {t} as out of scope"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn orchestrator_extension_possesses_no_admin_tool() {
        // The orchestrator has no corpus-admin extension at all — prove it has
        // no admin capability by the role manifest (and never register one).
        assert!(TeamRole::Orchestrator.admin_tools().is_empty());
        assert!(!TeamRole::Orchestrator.has_scoped_admin());
    }
}