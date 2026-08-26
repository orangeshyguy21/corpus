//! Corpus-owned management-chat boundary.
#![allow(dead_code)]
//!
//! This is the ONE place in corpus-app that knows the GDK runtime. The
//! rest of the app sees nothing but our own types: the [`ChatEvent`] stream,
//! the [`ChatCommand`]s it sends, and the narrow [`Chat`] trait. **No goose/GDK
//! type crosses this module boundary** — the runtime lives behind `embedded`
//! (a private submodule) and its types are quarantined here. If the backend
//! is ever swapped again, it is a transport refactor behind this same seam,
//! not a redesign.
//!
//! The runtime is an embedded Goose agent: `chat/embedded.rs` compiles
//! goose's `Agent` in-process and drives it on a background thread, spinning up
//! `corpus-admin-mcp` as its (our) tool extension. `scripts/goose-chat` is
//! NOT used by the app — it stays the headless debug fallback.
//!
//! Event semantics mirror goose's `AgentEvent` stream faithfully (message
//! text, tool-call start/result, and the confirmation gate for sensitive
//! tools) so a future backend speaks the same model. Session identity is OUR
//! term (project-scoped id + transcript path) — never goose's own session name.

pub mod panel;
// GFM table splitting + rendering for assistant bubbles (2026-08-16:
// egui_commonmark 0.20's own tables are borderless striped Grids whose ids
// clash across messages, painting "🔥 ID clash" errors into the log).
pub mod tables;

mod embedded;
pub mod team;

pub use embedded::init_goose_env;

use std::sync::{Arc, Mutex};

/// An event emitted by the chat runtime, in OUR vocabulary.
///
/// Field/variant semantics intentionally mirror ACP's agent→client stream
/// (text content chunks, tool-call updates, and `session/request_permission`
/// requests) but are declared here so nothing outside this module touches ACP.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    /// A live backend session was established (our project-scoped id).
    Ready { session_id: String, project: String },
    /// A user message was accepted for a new turn.
    TurnStart { turn: u64 },
    /// Streaming assistant text fragment (a `ContentChunk`-equivalent).
    TextChunk { turn: u64, delta: String },
    /// A reasoning/thinking fragment (the model's thought process — rendered
    /// as a collapsible thought card, chronological with text and tools).
    ThinkingChunk { turn: u64, delta: String },
    /// The agent emitted a tool call (name + args).
    ToolCallStart {
        id: String,
        name: String,
        args_json: String,
    },
    /// The agent tool call completed with this output.
    ToolCallResult {
        id: String,
        is_error: bool,
        output: String,
    },
    /// The agent requested permission before executing a tool (a
    /// `RequestPermissionRequest`-equivalent). `summary` is the human-facing
    /// dry-run text; this is what the panel renders as inline Approve/Reject.
    PermissionRequest {
        id: String,
        tool: String,
        args_json: String,
        summary: String,
    },
    /// The agent ended its turn.
    TurnEnd { turn: u64 },
    /// The operator hard-stopped the turn (distinct from a natural TurnEnd
    /// so the log can mark it and the queue is cleared). Always followed by
    /// a `TurnEnd` for the same turn.
    Stopped { turn: u64 },
    /// Token accounting for a completed inference (per provider call).
    Usage {
        input_tokens: Option<i32>,
        output_tokens: Option<i32>,
        total_tokens: Option<i32>,
    },
    /// A write tool succeeded and mutated a store area ("projects" /
    /// "agents" / "missions" / "corpus" — chat::team::mutated_area). The
    /// app refreshes its nav from the store on this event.
    StoreMutated { area: &'static str },
    /// The backend shut down / errored.
    Error(String),
}

impl ChatEvent {
    /// A short kind tag for tests/logging.
    pub fn kind(&self) -> &'static str {
        match self {
            ChatEvent::Ready { .. } => "ready",
            ChatEvent::TurnStart { .. } => "turn_start",
            ChatEvent::TextChunk { .. } => "text_chunk",
            ChatEvent::ThinkingChunk { .. } => "thinking_chunk",
            ChatEvent::ToolCallStart { .. } => "tool_call_start",
            ChatEvent::ToolCallResult { .. } => "tool_call_result",
            ChatEvent::PermissionRequest { .. } => "permission_request",
            ChatEvent::TurnEnd { .. } => "turn_end",
            ChatEvent::Stopped { .. } => "stopped",
            ChatEvent::Usage { .. } => "usage",
            ChatEvent::StoreMutated { .. } => "store_mutated",
            ChatEvent::Error(_) => "error",
        }
    }
}

/// A command sent FROM the app TO the runtime (client→agent).
#[derive(Debug, Clone, PartialEq)]
pub enum ChatCommand {
    /// Send a user prompt (start/continue a turn).
    Send(String),
    /// Approve a pending [`ChatEvent::PermissionRequest`] by id.
    Approve { id: String },
    /// Reject a pending [`ChatEvent::PermissionRequest`] by id.
    Reject { id: String },
    /// Ask the backend to stop the current turn.
    Stop,
    /// Juice the session with the operator's current position in the app
    /// (project, screen, selected entities). The backend prepends it to the
    /// next user turn so deictic references ("this agent", "this project")
    /// resolve against where the operator actually is.
    SetContext(String),
    /// Tear down the backend session.
    Close,
}

/// The app-visible lifecycle state of a chat session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatPhase {
    /// No backend yet / not configured.
    Idle,
    /// A model was chosen and a backend is starting (`goose acp` spawn).
    Connecting,
    /// A session is live and can accept prompts.
    Ready,
    /// The backend failed or was closed.
    Finished,
}

/// The narrow trait the rest of the app sees. All GDK/ACP types stay behind
/// [`ChatHandle`]; the app only sends commands and drains events.
pub trait Chat {
    /// Send a user message (starts/continues a turn).
    fn send(&mut self, message: &str);
    /// Approve a pending permission request, releasing the tool call.
    fn approve(&mut self, id: &str);
    /// Reject a pending permission request (the tool is not executed).
    fn reject(&mut self, id: &str);
    /// Request that the current turn stop.
    fn stop(&mut self);
    /// Update the operator-position context the backend juices into turns.
    fn set_context(&mut self, context: &str);
    /// The lifecycle phase (derived, cheap).
    fn phase(&self) -> ChatPhase;
    /// Drain events emitted since the last call.
    fn poll_events(&self) -> Vec<ChatEvent>;
}

/// A handle to a live chat backend: owns the command sink into the runtime
/// and a drain of events out. Owned by the app; cheap to clone (Arc).
#[derive(Clone)]
pub struct ChatHandle {
    inner: Arc<Mutex<ChatInner>>,
}

struct ChatInner {
    tx: std::sync::mpsc::Sender<ChatCommand>,
    events: std::sync::mpsc::Receiver<ChatEvent>,
    phase: ChatPhase,
    project: String,
    session_id: Option<String>,
    /// Accumulated transcript (our session memory), line-joined.
    transcript: Vec<String>,
}

impl ChatHandle {
    /// Start a backend for `project`, scoped via `GOOSE_PATH_ROOT`, as the
    /// unfiltered **Operator** (all admin tools, still approval-gated). `model`
    /// must be Some — there is never an ambient default. The transcript is
    /// written to `<project scope>/var/chat/<session>.md`. For an explicit
    /// team role, use [`ChatHandle::start_scoped`].
    pub fn start(project: &str, model: &str) -> ChatHandle {
        Self::start_scoped(project, model, team::TeamRole::Operator)
    }

    /// Start a backend as a specific team role. A specialist registers only
    /// its scoped admin domain; an `Orchestrator` registers no admin tools.
    pub fn start_scoped(project: &str, model: &str, role: team::TeamRole) -> ChatHandle {
        Self::start_scoped_inner(project, model, role, None)
    }

    /// Production entry point: every backend event is placed in the UI
    /// inbox before egui is woken. Without this bridge streamed replies can
    /// sit invisible until unrelated pointer input or an ambient repaint.
    pub fn start_scoped_with_wake(
        project: &str,
        model: &str,
        role: team::TeamRole,
        wake: Arc<dyn crate::jobs::RepaintWake>,
    ) -> ChatHandle {
        Self::start_scoped_inner(project, model, role, Some(wake))
    }

    fn start_scoped_inner(
        project: &str,
        model: &str,
        role: team::TeamRole,
        wake: Option<Arc<dyn crate::jobs::RepaintWake>>,
    ) -> ChatHandle {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<ChatCommand>();
        let (visible_tx, ev_rx) = std::sync::mpsc::channel::<ChatEvent>();
        let backend_tx = if let Some(wake) = wake {
            let (backend_tx, backend_rx) = std::sync::mpsc::channel::<ChatEvent>();
            spawn_event_bridge(backend_rx, visible_tx, wake);
            backend_tx
        } else {
            visible_tx
        };
        let session_id = embedded::spawn_backend(project, model, role, backend_tx, cmd_rx);
        let handle = ChatInner {
            tx: cmd_tx,
            events: ev_rx,
            // Behind the ready event: the backend connects asynchronously; the
            // phase flips to Ready only when the Ready event is polled (and to
            // Finished on an Error). Until then the UI should show "connecting…".
            phase: ChatPhase::Connecting,
            project: project.to_string(),
            session_id: Some(session_id),
            transcript: Vec::new(),
        };
        ChatHandle {
            inner: Arc::new(Mutex::new(handle)),
        }
    }

    /// A handle in the Idle phase (no backend) — used by the panel before a
    /// model is selected.
    pub fn idle(project: &str) -> ChatHandle {
        ChatHandle {
            inner: Arc::new(Mutex::new(ChatInner {
                tx: std::sync::mpsc::channel().0,
                events: std::sync::mpsc::channel().1,
                phase: ChatPhase::Idle,
                project: project.to_string(),
                session_id: None,
                transcript: Vec::new(),
            })),
        }
    }

    /// The project this session is scoped to.
    pub fn project(&self) -> String {
        self.inner.lock().unwrap().project.clone()
    }
}

fn spawn_event_bridge(
    source: std::sync::mpsc::Receiver<ChatEvent>,
    destination: std::sync::mpsc::Sender<ChatEvent>,
    wake: Arc<dyn crate::jobs::RepaintWake>,
) {
    let _ = std::thread::Builder::new()
        .name("chat-event-wake".into())
        .spawn(move || {
            while let Ok(event) = source.recv() {
                if destination.send(event).is_err() {
                    break;
                }
                // Send first: the frame this requests must always be able to
                // drain the event that caused it.
                wake.request_repaint();
            }
        });
}

impl Chat for ChatHandle {
    fn send(&mut self, message: &str) {
        let _ = self
            .inner
            .lock()
            .unwrap()
            .tx
            .send(ChatCommand::Send(message.to_string()));
        self.inner
            .lock()
            .unwrap()
            .transcript
            .push(format!("## you\n\n{message}"));
    }

    fn approve(&mut self, id: &str) {
        let _ = self
            .inner
            .lock()
            .unwrap()
            .tx
            .send(ChatCommand::Approve { id: id.to_string() });
    }

    fn reject(&mut self, id: &str) {
        let _ = self
            .inner
            .lock()
            .unwrap()
            .tx
            .send(ChatCommand::Reject { id: id.to_string() });
    }

    fn stop(&mut self) {
        let _ = self.inner.lock().unwrap().tx.send(ChatCommand::Stop);
    }

    fn set_context(&mut self, context: &str) {
        let _ = self
            .inner
            .lock()
            .unwrap()
            .tx
            .send(ChatCommand::SetContext(context.to_string()));
    }

    fn phase(&self) -> ChatPhase {
        self.inner.lock().unwrap().phase
    }

    fn poll_events(&self) -> Vec<ChatEvent> {
        let mut inner = self.inner.lock().unwrap();
        let mut out = Vec::new();
        while let Ok(ev) = inner.events.try_recv() {
            match &ev {
                ChatEvent::Ready { session_id, .. } => {
                    inner.session_id = Some(session_id.clone());
                    inner.phase = ChatPhase::Ready;
                }
                ChatEvent::Error(e) => {
                    inner.phase = ChatPhase::Finished;
                    inner.transcript.push(format!("**error**: {e}"));
                }
                // The transcript is a STRUCTURED log (role headers, thought
                // cards, tool calls) — the old format concatenated user and
                // assistant text (`> helloHello! 👋…`) and dropped tools and
                // thinking, making audits impossible (2026-08-14).
                ChatEvent::TextChunk { delta, .. } => match inner.transcript.last_mut() {
                    Some(last) if last.starts_with("## corpus") => last.push_str(delta),
                    _ => inner.transcript.push(format!("## corpus\n\n{delta}")),
                },
                ChatEvent::ThinkingChunk { delta, .. } => match inner.transcript.last_mut() {
                    Some(last) if last.starts_with("### thought") => last.push_str(delta),
                    _ => inner.transcript.push(format!("### thought\n\n{delta}")),
                },
                ChatEvent::ToolCallStart {
                    name, args_json, ..
                } => {
                    inner
                        .transcript
                        .push(format!("### tool › {name}\n\n```json\n{args_json}\n```"));
                }
                ChatEvent::ToolCallResult {
                    is_error, output, ..
                } => {
                    let marker = if *is_error { "→ (error) " } else { "→ " };
                    match inner.transcript.last_mut() {
                        Some(last) if last.starts_with("### tool ›") => {
                            last.push_str(&format!("\n\n{marker}{output}"));
                        }
                        _ => inner.transcript.push(format!("{marker}{output}")),
                    }
                }
                _ => {}
            }
            // Make the transcript durable as each turn completes.
            if matches!(&ev, ChatEvent::TurnEnd { .. } | ChatEvent::Error(_)) {
                flush_transcript(
                    &inner.project,
                    inner.session_id.as_deref(),
                    &inner.transcript,
                );
            }
            out.push(ev);
        }
        out
    }
}

/// Write the accumulated in-memory transcript to the project scope
/// `<store parent>/var/chat/<project>/<session>.md`, outside the project
/// subtree so a launched agent cannot read the operator's management notes.
/// Non-fatal: the durable memory
/// degrades to in-memory only if the path is unwritable.
fn flush_transcript(project: &str, session_id: Option<&str>, transcript: &[String]) {
    let Some(session_id) = session_id else { return };
    let dir = corpus_core::Store::from_env().project_chat_dir(project);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let body = transcript.join("\n\n");
    let _ = std::fs::write(dir.join(format!("{session_id}.md")), body);
}

/// Truncate a string for one-line diagnostics (char-safe, ellipsised).
pub fn truncate(s: &str, max: usize) -> String {
    let one_line = s.replace('\n', " ");
    if one_line.chars().count() <= max {
        return one_line;
    }
    let cut: String = one_line.chars().take(max).collect();
    format!("{cut}…")
}

/// The corpus-admin destructive tool set. The embedded backend gates/// execution in-process: a mutating tool call is surfaced to the operator as
/// an inline [`ChatEvent::PermissionRequest`] and only runs when the operator
/// Approves (goose's `tool_confirmation_router` releases it before dispatch).
/// This list keeps the gate explicit for tests and UI emphasis; enforcement
/// rides Goose's confirmation router.
pub fn is_destructive(tool: &str) -> bool {
    matches!(
        tool,
        "corpus_wipe" | "project_delete" | "agent_delete" | "mission_delete"
    )
}

/// The confirm-token gate, as a pure unit testable in isolation: a tool call
/// (by permission-request id) runs only after the operator Approves; Reject
/// (or approving an unknown/expired id) is a no-op. The model never holds an
/// unapproved grant — the operator sees the dry-run summary first.
///
/// The embedded backend enforces the gate in-process via Goose's
/// `tool_confirmation_router` before dispatch; this
/// struct keeps the corpus-mcp server-side token-gate contract as the
/// defense-in-depth backstop and a unit-testable spec of the semantics.
#[derive(Debug, Default)]
pub struct ConfirmGate {
    pending: std::collections::HashMap<String, String>,
}

impl ConfirmGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a permission request awaiting a decision. Returns false if the
    /// id was already present (a replay is rejected).
    pub fn request(&mut self, id: &str, dry_run_summary: &str) -> bool {
        use std::collections::hash_map::Entry;
        match self.pending.entry(id.to_string()) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(dry_run_summary.to_string());
                true
            }
        }
    }

    /// The operator approved: consume id -> access granted iff it was pending.
    pub fn approve(&mut self, id: &str) -> bool {
        self.pending.remove(id).is_some()
    }

    /// The operator rejected: the attempted tool never runs.
    pub fn reject(&mut self, id: &str) -> bool {
        self.pending.remove(id).is_some()
    }

    /// Whether `id` is still awaiting a decision (an Approve without a request
    /// is a no-op — the gate never releases a grant that was not requested).
    pub fn is_pending(&self, id: &str) -> bool {
        self.pending.contains_key(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct InspectWake {
        events: std::sync::Mutex<std::sync::mpsc::Receiver<ChatEvent>>,
        observed: std::sync::Mutex<Vec<&'static str>>,
    }

    impl crate::jobs::RepaintWake for InspectWake {
        fn request_repaint(&self) {
            if let Ok(event) = self.events.lock().unwrap().try_recv() {
                self.observed.lock().unwrap().push(event.kind());
            }
        }
    }

    #[test]
    fn event_kinds_are_stable() {
        assert_eq!(
            ChatEvent::Ready {
                session_id: "s".into(),
                project: "p".into()
            }
            .kind(),
            "ready"
        );
        assert_eq!(ChatEvent::TurnStart { turn: 1 }.kind(), "turn_start");
        assert_eq!(
            ChatEvent::TextChunk {
                turn: 1,
                delta: "x".into()
            }
            .kind(),
            "text_chunk"
        );
        assert_eq!(
            ChatEvent::ThinkingChunk {
                turn: 1,
                delta: "x".into()
            }
            .kind(),
            "thinking_chunk"
        );
        assert_eq!(
            ChatEvent::ToolCallStart {
                id: "i".into(),
                name: "n".into(),
                args_json: "{}".into()
            }
            .kind(),
            "tool_call_start"
        );
        assert_eq!(
            ChatEvent::ToolCallResult {
                id: "i".into(),
                is_error: false,
                output: "o".into()
            }
            .kind(),
            "tool_call_result"
        );
        assert_eq!(
            ChatEvent::PermissionRequest {
                id: "i".into(),
                tool: "t".into(),
                args_json: "{}".into(),
                summary: "s".into()
            }
            .kind(),
            "permission_request"
        );
        assert_eq!(ChatEvent::TurnEnd { turn: 1 }.kind(), "turn_end");
        assert_eq!(
            ChatEvent::Usage {
                input_tokens: Some(1),
                output_tokens: Some(2),
                total_tokens: Some(3)
            }
            .kind(),
            "usage"
        );
        assert_eq!(ChatEvent::Error("e".into()).kind(), "error");
    }

    #[test]
    fn idle_handle_has_no_session_and_polls_nothing() {
        let h = ChatHandle::idle("default");
        assert_eq!(h.phase(), ChatPhase::Idle);
        assert!(h.poll_events().is_empty());
    }

    #[test]
    fn chat_event_is_visible_before_its_repaint_wake() {
        let (backend_tx, backend_rx) = std::sync::mpsc::channel();
        let (visible_tx, visible_rx) = std::sync::mpsc::channel();
        let wake = Arc::new(InspectWake {
            events: std::sync::Mutex::new(visible_rx),
            observed: std::sync::Mutex::new(Vec::new()),
        });
        spawn_event_bridge(backend_rx, visible_tx, wake.clone());

        backend_tx.send(ChatEvent::TurnStart { turn: 7 }).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while wake.observed.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(*wake.observed.lock().unwrap(), vec!["turn_start"]);
    }

    #[test]
    fn destructive_tool_set_is_operator_gated() {
        for tool in [
            "corpus_wipe",
            "project_delete",
            "agent_delete",
            "mission_delete",
        ] {
            assert!(is_destructive(tool), "{tool} must be gated");
        }
        for tool in ["project_list", "corpus_stats", "mission_set_budget"] {
            assert!(!is_destructive(tool), "{tool} must not be gated");
        }
    }

    #[test]
    fn confirm_gate_requires_approval_before_release() {
        let mut gate = ConfirmGate::new();
        // The model requests a corpus_wipe; the operator sees the dry-run first.
        assert!(gate.request("wipe-1", "DRY RUN — would wipe corpus of default"));
        assert!(gate.is_pending("wipe-1"));
        // Approving an id that was never requested (or already used) is a no-op.
        assert!(!gate.approve("never-requested"));
        // The dry-run summary is surfaced, not a token.
        // Operator Reject: nothing runs.
        assert!(gate.reject("wipe-1"));
        assert!(!gate.is_pending("wipe-1"));
        // A fresh request, operator Approve: granted exactly once.
        assert!(gate.request("wipe-2", "DRY RUN"));
        assert!(gate.approve("wipe-2"));
        // Single-use: a second approve is refused.
        assert!(!gate.approve("wipe-2"));
    }
}
