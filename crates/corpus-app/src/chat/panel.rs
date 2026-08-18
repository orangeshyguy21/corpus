//! The native egui management-chat panel (dev/decisions.md chunk 3, per the
//! app-parity spec): streaming markdown messages, collapsible tool-call cards,
//! and the confirm-token ritual as an INLINE Approve/Reject affordance.
#![allow(dead_code)]
//!
//! GUI-agnostic: it consumes only [`crate::chat`]'s public [`ChatEvent`]s and
//! sends [`ChatCommand`]s through the [`Chat`] trait — no ACP type is named
//! here.

use eframe::egui;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::chat::{Chat, ChatEvent, ChatHandle, ChatPhase};

/// Explicit retention bound for the on-screen conversation. The complete
/// transcript is flushed separately; this vector exists only to paint a tail.
const MAX_VISIBLE_MESSAGES: usize = 256;

/// Panel-local render state for one conversation.
pub struct ChatPanelView {
    /// Events still on screen (roll the tail). Kept small; the transcript is
    /// the durable memory.
    messages: Vec<Rendered>,
    input: String,
    model: String,
    ollama_models: crate::state::ModelDiscovery,
    ollama_jobs: Option<crate::jobs::JobSet<corpus_core::ModelList>>,
    md: CommonMarkCache,
    /// Map from permission request id -> the args/tool, for the inline card.
    pending: Vec<PendingPermission>,
    /// Live backend activity for the status line.
    activity: Activity,
    /// When the current activity began (for the animated elapsed timer).
    activity_since: std::time::Instant,
    /// When the CURRENT turn started (reset on TurnStart) — drives the
    /// live throughput read, which must span the whole turn, not just the
    /// latest activity phase.
    turn_since: std::time::Instant,
    /// Streamed characters (thinking + text) this turn — an approximate
    /// live output size so a long turn shows visible progress, not a frozen
    /// spinner. Reset on TurnStart.
    turn_stream_chars: usize,
    /// Whether any content chunk has arrived this turn yet: before the
    /// first, the model is prefilling/loading, not thinking.
    turn_saw_output: bool,
    /// The turn we last saw start — a `TurnEnd` for an OLDER turn must not
    /// idle the status (stale-task guard; turns are serialized backend-side,
    /// this is belt-and-braces).
    live_turn: u64,
    /// The last backend failure, surfaced as a visible status (never silently).
    last_error: Option<String>,
    /// Cumulative token usage this session (input, output), from Usage events.
    usage: (i64, i64),
    /// The team role this session runs as (default: Operator — the full,
    /// approval-gated catalog; the Orchestrator's summon delegation is
    /// experimental until in-process specialist delegation lands).
    role: crate::chat::team::TeamRole,
    /// The display name of the chat's project (slug fallback) — the header
    /// names things, never bare UUIDs.
    project_label: String,
    /// LAST FRAME's measured composer height. The footer used to be a fixed
    /// 88px reservation, so the moment the input grew past one row the box
    /// (and the status line under it) fell off the bottom of the panel — the
    /// "text cut off" bug. Measuring it instead means the message log always
    /// yields exactly the space the composer actually takes.
    footer_h: f32,
    /// Whether the input had keyboard focus last frame — drives the
    /// composer's focus ring.
    input_focused: bool,
}

struct Rendered {
    text: String,
    /// Our tool-call cards, in arrival order within this bubble.
    tools: Vec<ToolCard>,
    kind: BubbleKind,
    /// A user message sent while a turn was live — queued backend-side
    /// (turns are serialized). Cleared when its turn starts.
    queued: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum BubbleKind {
    User,
    Assistant,
    /// The model's reasoning (collapsible "thought" card).
    Thought,
    Tool,
    /// A backend failure, rendered as red text in the log (never silent).
    Error,
    /// A neutral, operator-facing marker (e.g. "— stopped —") — faint, not
    /// an error.
    Notice,
}

/// What the backend is doing right now, for the status line (the "no
/// feedback — model loading.. thinking…." gap): derived from turn events.
#[derive(Clone, PartialEq)]
enum Activity {
    Idle,
    Thinking,
    Streaming,
    Tool(String),
}

struct ToolCard {
    /// The backend's call id — results are matched back to THEIR call, not
    /// to "the most recent card" (a mis-match attributed a result to the
    /// wrong tool when calls interleaved).
    id: String,
    name: String,
    args: String,
    result: Option<String>,
    is_error: bool,
}

struct PendingPermission {
    id: String,
    tool: String,
    args: String,
    summary: String,
    open: bool,
}

impl Default for ChatPanelView {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            model: String::new(),
            ollama_models: crate::state::ModelDiscovery::Loading,
            ollama_jobs: None,
            md: CommonMarkCache::default(),
            pending: Vec::new(),
            activity: Activity::Idle,
            activity_since: std::time::Instant::now(),
            turn_since: std::time::Instant::now(),
            turn_stream_chars: 0,
            turn_saw_output: false,
            live_turn: 0,
            last_error: None,
            usage: (0, 0),
            role: crate::chat::team::TeamRole::Operator,
            project_label: String::new(),
            footer_h: 108.0,
            input_focused: false,
        }
    }
}

/// A project-scoped chat session id (`<epoch>-chat-<project>`), entirely in
/// our terms (never goose's own naming).
pub fn session_id(project: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{ts}-chat-{project}")
}

/// Human-readable tool names for the log (pure data): the model speaks
/// `corpus-admin__mission_set_budget`, the operator reads "set mission
/// budget". Delegate calls and specialist sub-calls get role phrasing.
pub fn human_tool_name(raw: &str) -> String {
    fn bare_name(raw: &str) -> String {
        match raw {
            "project_list" => "list projects".into(),
            "project_new" => "create project".into(),
            "project_clone" => "clone project".into(),
            "project_delete" => "delete project".into(),
            "project_rebind" => "rebind project".into(),
            "agent_list" => "list agents".into(),
            "agent_get" => "read agent".into(),
            "agent_new" => "create agent".into(),
            "agent_save" => "save agent".into(),
            "agent_clone" => "clone agent".into(),
            "agent_copy" => "copy agent to project".into(),
            "agent_set" => "edit agent field".into(),
            "agent_set_role" => "set agent role".into(),
            "agent_set_permission" => "edit agent permissions".into(),
            "agent_subagent_add" => "add subagent".into(),
            "agent_subagent_remove" => "remove subagent".into(),
            "agent_delete" => "delete agent".into(),
            "mission_list" => "list missions".into(),
            "mission_get" => "read mission".into(),
            "mission_new" => "create mission".into(),
            "mission_delete" => "delete mission".into(),
            "mission_set_budget" => "set mission budget".into(),
            "mission_set_pins" => "set mission pins".into(),
            "corpus_stats" => "corpus stats".into(),
            "corpus_list" => "list corpus entries".into(),
            "corpus_read" => "read corpus entry".into(),
            "corpus_wipe" => "WIPE corpus".into(),
            "model_list" => "list available models".into(),
            other => other.replace('_', " "),
        }
    }
    // "delegate › agent-builder" → "delegate to agent-builder".
    if let Some(role) = raw.strip_prefix("delegate › ") {
        return format!("delegate to {role}");
    }
    // "agent-builder›corpus-admin__agent_save" → "agent-builder › save agent".
    if let Some((role, tool)) = raw.split_once('›') {
        let tool = crate::chat::team::bare_tool_name(tool.trim());
        return format!("{} › {}", role.trim(), bare_name(tool));
    }
    bare_name(crate::chat::team::bare_tool_name(raw))
}

impl ChatPanelView {
    /// The chosen model (empty = none selected). The panel must NOT start until
    /// set; `Chat::phase` therefore gates the input.
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The team role this session runs as (dev/decisions.md chunk 3). Default
    /// `Orchestrator`; the operator can switch to `Operator` (full catalog,
    /// destructive gated by Approve/Reject) or a specialist.
    pub fn set_role(&mut self, role: crate::chat::team::TeamRole) {
        self.role = role;
    }

    pub fn role(&self) -> crate::chat::team::TeamRole {
        self.role
    }

    /// Set the project display name shown in the header (empty = show the
    /// backend's slug).
    pub fn set_project_label(&mut self, label: &str) {
        self.project_label = label.to_string();
    }

    /// Transition the live activity, restarting the elapsed timer when the
    /// KIND of activity changes (thinking → streaming → tool…).
    fn set_activity(&mut self, activity: Activity) {
        if self.activity != activity {
            self.activity = activity;
            self.activity_since = std::time::Instant::now();
        }
    }

    /// Drain events from `chat` into the view, returning them so the app
    /// can react to app-level signals (StoreMutated → nav refresh).
    pub fn absorb(&mut self, chat: &dyn Chat) -> Vec<ChatEvent> {
        let events = chat.poll_events();
        for ev in events.iter().cloned() {
            match ev {
                ChatEvent::TextChunk { delta, .. } => {
                    self.set_activity(Activity::Streaming);
                    self.turn_saw_output = true;
                    self.turn_stream_chars += delta.chars().count();
                    // Coalesce streamed chunks into the open assistant bubble.
                    match self.messages.last_mut() {
                        Some(m) if m.kind == BubbleKind::Assistant => m.text.push_str(&delta),
                        _ => self.messages.push(Rendered {
                            text: delta,
                            tools: Vec::new(),
                            kind: BubbleKind::Assistant,
                            queued: false,
                        }),
                    }
                }
                ChatEvent::ThinkingChunk { delta, .. } => {
                    self.set_activity(Activity::Thinking);
                    self.turn_saw_output = true;
                    self.turn_stream_chars += delta.chars().count();
                    // Coalesce streamed reasoning into the open thought card.
                    match self.messages.last_mut() {
                        Some(m) if m.kind == BubbleKind::Thought => m.text.push_str(&delta),
                        _ => self.messages.push(Rendered {
                            text: delta,
                            tools: Vec::new(),
                            kind: BubbleKind::Thought,
                            queued: false,
                        }),
                    }
                }
                ChatEvent::ToolCallStart { id, name, args_json, .. } => {
                    self.set_activity(Activity::Tool(name.clone()));
                    self.messages.push(Rendered {
                        text: String::new(),
                        tools: vec![ToolCard {
                            id,
                            name,
                            args: args_json,
                            result: None,
                            is_error: false,
                        }],
                        kind: BubbleKind::Tool,
                        queued: false,
                    });
                }
                ChatEvent::ToolCallResult { id, is_error, output, .. } => {
                    self.set_activity(Activity::Thinking);
                    // A resolved permission request's tool result arriving
                    // clears its card (backstop to the click path below).
                    self.pending.retain(|p| p.id != id);
                    // Match the result to ITS call by id (chronological
                    // attribution); fall back to the last unresolved card.
                    // Located by (bubble, card) index — a `&mut` can't move
                    // out of a loop over `iter_mut` cleanly.
                    let mut located: Option<(usize, usize)> = None;
                    'by_id: for (mi, m) in self.messages.iter().enumerate().rev() {
                        if m.kind != BubbleKind::Tool {
                            continue;
                        }
                        for (ci, c) in m.tools.iter().enumerate() {
                            if c.id == id {
                                located = Some((mi, ci));
                                break 'by_id;
                            }
                        }
                    }
                    if located.is_none() {
                        'open: for (mi, m) in self.messages.iter().enumerate().rev() {
                            if m.kind != BubbleKind::Tool {
                                continue;
                            }
                            for (ci, c) in m.tools.iter().enumerate() {
                                if c.result.is_none() {
                                    located = Some((mi, ci));
                                    break 'open;
                                }
                            }
                        }
                    }
                    if let Some((mi, ci)) = located {
                        let card = &mut self.messages[mi].tools[ci];
                        card.result = Some(output.clone());
                        card.is_error = is_error;
                    }
                }
                ChatEvent::PermissionRequest { id, tool, args_json, summary, .. } => {
                    self.pending.push(PendingPermission {
                        id,
                        tool,
                        args: args_json,
                        summary,
                        open: true,
                    });
                }
                ChatEvent::Ready { .. } => {}
                ChatEvent::TurnStart { turn } => {
                    self.set_activity(Activity::Thinking);
                    self.live_turn = turn;
                    self.turn_since = std::time::Instant::now();
                    self.turn_stream_chars = 0;
                    self.turn_saw_output = false;
                    // The oldest queued message's turn just started.
                    if let Some(m) = self.messages.iter_mut().find(|m| m.queued) {
                        m.queued = false;
                    }
                }
                ChatEvent::TurnEnd { turn } => {
                    if turn >= self.live_turn {
                        self.set_activity(Activity::Idle);
                    }
                }
                ChatEvent::Stopped { .. } => {
                    // Any queued sends were dropped backend-side; drop their
                    // bubbles too so the log matches what will actually run.
                    self.messages.retain(|m| !m.queued);
                    self.messages.push(Rendered {
                        text: "— stopped —".into(),
                        tools: Vec::new(),
                        kind: BubbleKind::Notice,
                        queued: false,
                    });
                }
                ChatEvent::Usage { input_tokens, output_tokens, .. } => {
                    self.usage.0 += input_tokens.unwrap_or(0) as i64;
                    self.usage.1 += output_tokens.unwrap_or(0) as i64;
                }
                ChatEvent::StoreMutated { .. } => {} // the app reacts (nav refresh)
                 ChatEvent::Error(e) => {
                    self.set_activity(Activity::Idle);
                    self.last_error = Some(e.clone());
                    self.messages.push(Rendered {
                        text: format!("error: {e}"),
                        tools: Vec::new(),
                        kind: BubbleKind::Error,
                        queued: false,
                    });
                }
            }
        }
        if self.messages.len() > MAX_VISIBLE_MESSAGES {
            self.messages.drain(..self.messages.len() - MAX_VISIBLE_MESSAGES);
        }
        events
    }

    fn can_send(&self, chat: &dyn Chat) -> bool {
        !self.model.is_empty() && chat.phase() == ChatPhase::Ready && !self.input.trim().is_empty()
    }

    /// The agent's live activity, rendered IN the log (chronologically after
    /// its last message/tool card) — "the thought process and actions read in
    /// order" — instead of the old detached footer line. None when idle: the
    /// log shows history only. Animated: the ellipsis cycles and the elapsed
    /// timer owns a panel-local 400 ms repaint deadline while a turn is live.
    fn live_activity(&self, chat: &dyn Chat) -> Option<(String, egui::Color32)> {
        let busy = crate::theme::rgb(200, 150, 80);
        let dots = match (self.activity_since.elapsed().as_millis() / 400) % 4 {
            0 => "",
            1 => ".",
            2 => "..",
            _ => "...",
        };
        // The timer spans the whole TURN (not the latest phase), so a long
        // think doesn't reset to 0:00 the moment it starts writing.
        let secs = self.turn_since.elapsed().as_secs();
        let timer = format!("{}:{:02}", secs / 60, secs % 60);
        // Approx output so far: ~4 chars/token, shown once enough has
        // streamed to be meaningful, with a rate so a slow local model
        // reads as "working", not "stuck".
        let throughput = |color: egui::Color32| {
            let toks = self.turn_stream_chars / 4;
            if toks < 5 {
                return (String::new(), color);
            }
            let rate = toks as f32 / self.turn_since.elapsed().as_secs_f32().max(0.1);
            (format!(" · ~{toks} tok · {rate:.0}/s"), color)
        };
        match chat.phase() {
            ChatPhase::Connecting => Some((format!("connecting / model loading{dots}"), busy)),
            ChatPhase::Ready => match &self.activity {
                Activity::Idle => None,
                // Before the first token the model is loading/prefilling the
                // (often large) context — say so instead of "thinking", which
                // read as a hang when nothing moved for minutes.
                Activity::Thinking if !self.turn_saw_output => {
                    Some((format!("preparing · prefilling context{dots} {timer}"), busy))
                }
                Activity::Thinking => {
                    let (rate, _) = throughput(busy);
                    Some((format!("thinking{dots} {timer}{rate}"), busy))
                }
                Activity::Streaming => {
                    let (rate, _) = throughput(busy);
                    Some((format!("writing{dots} {timer}{rate}"), busy))
                }
                Activity::Tool(name) => {
                    Some((format!("running {}… {timer}", human_tool_name(name)), busy))
                }
            },
            _ => None,
        }
    }

    /// The backend PHASE (connecting / ready / failed) as the model picker's
    /// dot colour and tooltip wording. Turn activity lives in the log
    /// (`live_activity`), and a failure's detail is already an error bubble
    /// there — this line names the phase, it doesn't explain it.
    fn status(&self, chat: &dyn Chat) -> (String, egui::Color32) {
        match chat.phase() {
            ChatPhase::Idle => ("no backend — pick a model".into(), crate::theme::TEXT_FAINT),
            ChatPhase::Connecting => ("connecting…".into(), crate::theme::rgb(200, 150, 80)),
            ChatPhase::Ready => ("ready".into(), crate::theme::HEALTHY),
            ChatPhase::Finished => (
                match &self.last_error {
                    Some(_) => "failed — see log".into(),
                    None => "session ended".into(),
                },
                crate::theme::SIGNAL_RED,
            ),
        }
    }

    /// Render the panel; returns nothing, mutates chat/self.
    pub fn show(&mut self, ui: &mut egui::Ui, chat: &mut ChatHandle) {
        let project = if self.project_label.is_empty() {
            chat.project()
        } else {
            self.project_label.clone()
        };
        // The role picker is allocated FIRST (right-to-left), then the title
        // group fills what's left. Laid out title-first, the truncating
        // project label claims the whole row and squeezes the picker off the
        // panel's edge.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The role selector (default Operator: full catalog,
                // destructive ops gated by inline Approve/Reject). A change
                // restarts the session (main.rs ensure_chat_started compares
                // the role).
                crate::theme::combo_field(ui, |ui| {
                    egui::ComboBox::from_id_salt("chat_role")
                        .icon(crate::theme::combo_caret)
                        .selected_text(
                            egui::RichText::new(self.role.label().to_string()).small(),
                        )
                        .show_ui(ui, |ui| {
                            for r in crate::chat::team::ALL_ROLES {
                                let label = if *r == crate::chat::team::TeamRole::Orchestrator {
                                    format!("{} (experimental)", r.label())
                                } else {
                                    r.label().to_string()
                                };
                                if ui.selectable_label(self.role == *r, label).clicked() {
                                    self.role = *r;
                                }
                            }
                        });
                });
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.label(crate::theme::section_heading("Chat"));
                    // Truncated, never wrapped.
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("· {project}"))
                                .small()
                                .color(crate::theme::TEXT_FAINT),
                        )
                        .truncate(),
                    );
                });
            });
        });
        ui.add_space(2.0);
        crate::theme::hairline(ui);
        ui.add_space(6.0);

        // Message scroll with explicit height reservation, then the composer
        // below it. (NOT bottom_up layout — it inverts the ScrollArea's
        // content stacking: the "new message stacks on top" bug.) The
        // reservation is LAST FRAME's measured composer height, so a grown
        // input steals space from the log instead of falling off the panel.
        let scroll_h = (ui.available_height() - self.footer_h - 8.0).max(60.0);
        // Scrollable message area. Horizontal shrink is ON so a wide
        // markdown line can never force the panel wider than its clamp.
        egui::ScrollArea::vertical()
            .max_height(scroll_h)
            .auto_shrink([true, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.messages.is_empty() {
                    self.empty_state(ui, scroll_h);
                }
                let messages = &self.messages;
                let md = &mut self.md;
                let last_i = messages.len().saturating_sub(1);
                let thinking_live = matches!(self.activity, Activity::Thinking);
                for (mi, m) in messages.iter().enumerate() {
                    // Every bubble is namespaced by its index: id-bearing
                    // widgets inside (egui_commonmark's table Grid among
                    // them) can then never collide ACROSS bubbles — the
                    // cross-message clash painted egui's red "🔥 ID clash"
                    // error text into the log.
                    ui.push_id(mi, |ui| match m.kind {
                        BubbleKind::User => user_bubble(ui, &m.text, m.queued),
                        BubbleKind::Assistant => {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("corpus")
                                    .small()
                                    .color(crate::theme::TEXT_FAINT),
                            );
                            // Segment markdown: tables are rendered by our
                            // own renderer (chat/tables.rs); everything else
                            // goes to egui_commonmark.
                            for seg in crate::chat::tables::split(&m.text) {
                                match seg {
                                    crate::chat::tables::Segment::Markdown(text) => {
                                        CommonMarkViewer::new().show(ui, md, &text);
                                    }
                                    crate::chat::tables::Segment::Table(t) => {
                                        ui.add_space(4.0);
                                        crate::chat::tables::show_table(ui, &t);
                                        ui.add_space(4.0);
                                    }
                                }
                            }
                        }
                        BubbleKind::Thought => {
                            ui.add_space(4.0);
                            // The model's reasoning, collapsed by default
                            // (traces run to thousands of tokens) but always
                            // present in the log, in order. The ACTIVE thought
                            // (last bubble while thinking) opens itself so a
                            // long reasoning phase visibly streams instead of
                            // hiding behind a collapsed header.
                            let live = mi == last_i && thinking_live;
                            egui::CollapsingHeader::new(
                                egui::RichText::new(if live { "thinking…" } else { "thought" })
                                    .small()
                                    .italics()
                                    .color(crate::theme::TEXT_FAINT),
                            )
                            .id_salt(format!("thought_{mi}"))
                            .open(live.then_some(true))
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&m.text)
                                        .small()
                                        .color(crate::theme::TEXT_MUTED),
                                );
                            });
                        }
                        BubbleKind::Tool => {
                            ui.add_space(6.0);
                            egui::Frame::default()
                                .fill(crate::theme::EDITOR_BG)
                                .stroke(egui::Stroke::new(1.0_f32, crate::theme::HAIRLINE))
                                .corner_radius(egui::CornerRadius::same(6))
                                .inner_margin(egui::Margin::symmetric(8, 6))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    tool_cards(ui, &m.tools);
                                });
                        }
                        BubbleKind::Error => {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(&m.text)
                                    .color(crate::theme::SIGNAL_RED)
                                    .small(),
                            );
                        }
                        BubbleKind::Notice => {
                            ui.add_space(6.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new(&m.text)
                                        .color(crate::theme::TEXT_FAINT)
                                        .italics()
                                        .small(),
                                );
                            });
                        }
                    });
                }
                // Inline approve/reject cards.
                self.permission_cards(ui, chat);
                // Live activity tail: the agent's current action reads
                // chronologically after its last log entry.
                if let Some((text, color)) = self.live_activity(chat) {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(text).small().italics().color(color));
                }
            });

        // The composer card: model picker, input, send/stop and the status
        // line, all inside ONE bordered surface pinned to the bottom of the
        // panel. Measured every frame so the log above reserves exactly the
        // height it needs (see `footer_h`).
        ui.add_space(6.0);
        let measured = ui
            .scope(|ui| {
                self.composer(ui, chat);
            })
            .response
            .rect
            .height();
        if (measured - self.footer_h).abs() > 0.5 {
            self.footer_h = measured;
            // Re-lay out immediately: the log's reservation is now stale by
            // exactly this delta, and a static panel wouldn't repaint on its
            // own.
            ui.ctx().request_repaint();
        }
        // Backend events wake the UI at delivery. This timer belongs only
        // to the elapsed-time/ellipsis animation during a live turn; an idle
        // or closed chat schedules no frames.
        if !matches!(self.activity, Activity::Idle) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(400));
        }
    }

    /// The empty log: a quiet, vertically-centred invitation rather than a
    /// lone grey line stranded at the top of a tall black rectangle.
    fn empty_state(&self, ui: &mut egui::Ui, h: f32) {
        ui.add_space((h * 0.34).max(12.0));
        ui.vertical_centered(|ui| {
            let (title, sub) = if self.model.is_empty() {
                ("choose a model to begin", "the picker sits below, in the composer")
            } else {
                ("no messages yet", "ask for a project, an agent, a mission…")
            };
            ui.label(
                crate::theme::icon_text(
                    egui_phosphor::regular::CHATS_CIRCLE,
                    26.0,
                    crate::theme::HAIRLINE,
                ),
            );
            ui.add_space(6.0);
            ui.label(egui::RichText::new(title).color(crate::theme::TEXT_MUTED));
            ui.add_space(2.0);
            ui.label(egui::RichText::new(sub).small().color(crate::theme::TEXT_FAINT));
        });
    }

    /// The composer: ONE bordered card — the auto-growing input on top, then
    /// a rail carrying the model picker, the session's token usage and the
    /// transforming send/stop button — plus the keyboard contract (Enter
    /// send, Shift+Enter newline, Esc stop).
    ///
    /// It is a card, not three loose rows, because the old stack (picker row
    /// / input+button row / status row) had no shared edge: the input's own
    /// frame, the button and the status text all floated at different insets
    /// and the whole thing overflowed the panel once the box grew.
    ///
    /// The backend phase has no widget of its own: it is the dot INSIDE the
    /// picker (grey = no model, amber = connecting, green = ready, red =
    /// failed), which is the control the operator acts on when it isn't
    /// green. The wording lives in its tooltip and, for a live turn, in the
    /// log's activity line.
    fn composer(&mut self, ui: &mut egui::Ui, chat: &mut dyn Chat) {
        let is_live = chat.phase() == ChatPhase::Ready && !matches!(self.activity, Activity::Idle);
        // Esc is a global stop while a turn runs, wherever focus is.
        if is_live && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            chat.stop();
        }

        let mut card = egui::Frame::default()
            .fill(crate::theme::EDITOR_BG)
            .stroke(egui::Stroke::new(1.0_f32, crate::theme::HAIRLINE))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::symmetric(8, 8))
            .begin(ui);

        {
            let ui = &mut card.content_ui;
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;

            // The box grows with its content: 1 row minimum, capped so a long
            // paste scrolls instead of eating the log. Frameless — the card
            // IS its frame — and full width, so a wrapped line is never
            // clipped by a button sitting beside it.
            let text_w = ui.available_width();
            let editor = egui::TextEdit::multiline(&mut self.input)
                .hint_text(
                    egui::RichText::new("message the corpus…").color(crate::theme::TEXT_FAINT),
                )
                .desired_width(text_w)
                .desired_rows(1)
                .frame(false)
                .margin(egui::Margin::symmetric(2, 2))
                .id_salt("chat_input");
            let response = egui::ScrollArea::vertical()
                .max_height(150.0)
                .auto_shrink([false, true])
                .id_salt("chat_input_scroll")
                .show(ui, |ui| ui.add_enabled(!self.model.is_empty(), editor))
                .inner;
            self.input_focused = response.has_focus();

            // Enter submits; Shift+Enter is a newline (left for the editor).
            // The editor has already inserted the '\n' for a plain Enter, so
            // strip it back off before sending.
            let plain_enter = response.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            if plain_enter && self.input.ends_with('\n') {
                self.input.pop();
            }

            crate::theme::hairline(ui);

            // Bottom rail: the status-bearing model picker fills the left,
            // usage and the one transforming button sit hard right. Same
            // allocation order as the header — the button and usage take
            // their slots first, and the picker sizes itself to what's left
            // (a picker laid out first would claim the row and push the
            // button off the card).
            let can_send = self.can_send(chat);
            let mut clicked = false;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clicked = if is_live {
                        crate::theme::destructive_button(ui, "stop").clicked()
                    } else {
                        ui.add_enabled_ui(can_send, |ui| crate::theme::house_button(ui, "send"))
                            .inner
                            .clicked()
                    };
                    if self.usage != (0, 0) {
                        ui.label(
                            egui::RichText::new(format!(
                                "↑{} ↓{}",
                                compact_tokens(self.usage.0),
                                compact_tokens(self.usage.1)
                            ))
                            .small()
                            .color(crate::theme::TEXT_FAINT),
                        );
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        self.model_picker(ui, chat);
                    });
                });
            });

            if is_live {
                if clicked {
                    chat.stop();
                }
            } else if clicked || (plain_enter && can_send) {
                self.submit(chat);
            }
        }

        // A focus ring on the card, painted after its content so the ring
        // reflects THIS frame's focus.
        if self.input_focused {
            card.frame.stroke = egui::Stroke::new(1.0_f32, crate::theme::TEXT_FAINT);
        }
        card.end(ui);
    }

    /// Send the current input as a new turn and clear the box.
    fn submit(&mut self, chat: &mut dyn Chat) {
        let msg = self.input.trim().to_string();
        if msg.is_empty() {
            return;
        }
        // A send while a turn is live QUEUES backend-side (turns are
        // serialized) — mark the bubble until its turn starts so the wait
        // reads as queued, not lost.
        let queued = !matches!(self.activity, Activity::Idle);
        self.messages.push(Rendered {
            text: msg.clone(),
            tools: Vec::new(),
            kind: BubbleKind::User,
            queued,
        });
        chat.send(&msg);
        self.input.clear();
    }

    /// A user message: right-aligned boxed bubble with a "you" tag, so every
    /// log entry is unambiguous about who said it (the "no indication who
    /// sent what" bug). A queued send (mid-turn) is dimmed and tagged.
    fn user_bubble(&self, ui: &mut egui::Ui, text: &str, queued: bool) {
        user_bubble(ui, text, queued)
    }

    /// The chat model picker (the GDK chat's OWN source: `ollama list` via
    /// corpus-core, never opencode's catalog), in the composer's bottom rail.
    ///
    /// It carries the backend status as a coloured dot in its own field: the
    /// phase is a property OF the chosen model's session, and the picker is
    /// what the operator reaches for when the dot isn't green, so a separate
    /// status line was both a duplicate and a widget nobody could act on. The
    /// phase wording moves to the field's tooltip.
    fn model_picker(&mut self, ui: &mut egui::Ui, chat: &dyn Chat) {
        if self.ollama_jobs.is_none() {
            self.ollama_jobs = Some(crate::jobs::JobSet::new(std::sync::Arc::new(
                ui.ctx().clone(),
            )));
        }
        let results = self
            .ollama_jobs
            .as_mut()
            .map(|jobs| jobs.drain_applicable(|_| true))
            .unwrap_or_default();
        for result in results {
            self.ollama_models = match result.terminal {
                crate::jobs::JobTerminal::Success(models) => {
                    crate::state::ModelDiscovery::Ready(models)
                }
                crate::jobs::JobTerminal::Failure(error) => {
                    crate::state::ModelDiscovery::Failed(error)
                }
                crate::jobs::JobTerminal::Cancelled => {
                    crate::state::ModelDiscovery::Failed("Ollama discovery cancelled".into())
                }
                crate::jobs::JobTerminal::TimedOut => {
                    crate::state::ModelDiscovery::Failed("Ollama discovery timed out".into())
                }
            };
        }
        if matches!(self.ollama_models, crate::state::ModelDiscovery::Loading) {
            self.start_ollama_discovery(false);
        }
        let current = self.model.clone();
        let (phase, dot) = self.status(chat);
        // The field takes what the rail's right-hand controls left it. The
        // label is elided to fit rather than allowed to widen the button:
        // ComboBox sizes to its text, so a long `hf.co/…` id would shove the
        // send button off the card.
        let field_w = (ui.available_width() - 2.0).max(90.0);
        let text_color = if current.is_empty() {
            crate::theme::TEXT_FAINT
        } else {
            crate::theme::TEXT
        };
        let label = if current.is_empty() {
            "choose a model…"
        } else {
            &current
        };
        // ComboBox sizes to its text (`width` is only a minimum), so the
        // label is fitted to the slot BEFORE it can widen the button and
        // shove the send button off the card. Budget = the field minus its
        // own furniture: two 8px margins, the caret icon and its spacing.
        let selected = fit_picker_label(ui, label, text_color, field_w - 36.0);
        crate::theme::combo_field(ui, |ui| {
            let combo = egui::ComboBox::from_id_salt("chat_model")
                .icon(crate::theme::combo_caret)
                .width(field_w)
                .selected_text(selected)
                .show_ui(ui, |ui| {
                        match &self.ollama_models {
                            crate::state::ModelDiscovery::Ready(list) => for g in &list.groups {
                                if !g.label.is_empty() {
                                    ui.label(egui::RichText::new(&g.label).weak().small());
                                }
                                for m in &g.models {
                                    let label =
                                        if m.name.is_empty() { m.model.clone() } else { m.name.clone() };
                                    if ui.selectable_label(current == m.model, &label).clicked() {
                                        self.model = m.model.clone();
                                    }
                                }
                            },
                            crate::state::ModelDiscovery::Loading => {
                                ui.label("loading Ollama models…");
                            }
                            crate::state::ModelDiscovery::Failed(error) => {
                                ui.label("ollama not available — a model is required")
                                    .on_hover_text(error);
                            }
                        }
                        ui.separator();
                        if ui.button("Refresh models").clicked() {
                            self.ollama_models = crate::state::ModelDiscovery::Loading;
                            self.start_ollama_discovery(true);
                        }
                    });
            // The status light, painted into the gap the label reserved for
            // it. A painted dot (the app's idiom — sidebar rows and the env
            // dot are the same shape) rather than a glyph: phosphor's DOT
            // renders at punctuation size and read as a stray period.
            let rect = combo.response.rect;
            ui.painter().circle_filled(
                egui::pos2(rect.left() + DOT_INSET, rect.center().y),
                3.5,
                dot,
            );
            // The full id is never truncated away: it's the first line of the
            // tooltip, with the phase under it.
            if !current.is_empty() {
                combo.response.on_hover_text(format!("{current}\n{phase}"))
            } else {
                combo.response.on_hover_text(phase)
            }
        });
    }

    fn start_ollama_discovery(&mut self, refresh: bool) {
        let Some(jobs) = self.ollama_jobs.as_mut() else { return };
        jobs.start(
            crate::jobs::JobKind::ModelDiscovery,
            crate::jobs::JobScope {
                project: String::new(),
                project_generation: 0,
                run_id: None,
            },
            std::time::Duration::from_secs(15),
            move |_| {
                corpus_core::ollama_models_refresh(refresh).map_err(|error| error.to_string())
            },
        );
    }

    fn tool_cards(&self, ui: &mut egui::Ui, cards: &[ToolCard]) {
        tool_cards(ui, cards)
    }

    fn permission_cards(&mut self, ui: &mut egui::Ui, chat: &mut ChatHandle) {
        // Take the pending list by value so clicks can mutate chat; a card is
        // re-added ONLY if the operator didn't resolve it this frame (the old
        // unconditional re-add made every card immortal — the "stale
        // approve/reject" bug).
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let mut resolved = false;
            ui.add_space(8.0);
            egui::Frame::default()
                .fill(crate::theme::PANEL)
                .stroke(egui::Stroke::new(1.0_f32, crate::theme::INTERACTION))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(crate::theme::icon_text(
                        egui_phosphor::regular::SHIELD_WARNING,
                        14.0,
                        crate::theme::INTERACTION,
                    ));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!(
                                "approval needed · {}",
                                human_tool_name(&p.tool)
                            ))
                            .color(crate::theme::TEXT),
                        )
                        .truncate(),
                    );
                });
                if !p.summary.is_empty() {
                    ui.label(egui::RichText::new(&p.summary).monospace().small());
                }
                // One-line args summary (the raw JSON blob is in the tool
                // card above if the operator wants it).
                let args: String = p.args.chars().take(120).collect();
                ui.label(
                    egui::RichText::new(args)
                        .monospace()
                        .small()
                        .color(crate::theme::TEXT_MUTED),
                );
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    if crate::theme::house_button(ui, "approve").clicked() {
                        chat.approve(&p.id);
                        resolved = true;
                    }
                    if crate::theme::destructive_button(ui, "reject").clicked() {
                        chat.reject(&p.id);
                        resolved = true;
                    }
                });
            });
            if !resolved {
                self.pending.push(p);
            }
        }
    }
}

/// Where the picker's status dot is painted, measured from the field's left
/// edge — the button's 8px inner margin plus half the gap the label leaves
/// in front of itself.
const DOT_INSET: f32 = 15.0;

/// The picker's selected line: the model id, indented past the status dot's
/// gap and elided to the widest form that fits `budget` px. The line is
/// MEASURED (real galley width) rather than estimated from an average
/// character width, which is what let a long `hf.co/…` id creep over the
/// token counts beside it. Binary search: ~6 layouts, not one per character.
fn fit_picker_label(
    ui: &egui::Ui,
    model: &str,
    text_color: egui::Color32,
    budget: f32,
) -> egui::text::LayoutJob {
    let job = |label: &str| {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            label,
            DOT_INSET,
            egui::TextFormat {
                font_id: crate::theme::font(12.0),
                color: text_color,
                valign: egui::Align::Center,
                ..Default::default()
            },
        );
        job
    };
    let fits = |j: &egui::text::LayoutJob| {
        ui.fonts(|f| f.layout_job(j.clone())).size().x <= budget
    };
    let full = job(model);
    if fits(&full) {
        return full;
    }
    // Largest character count that still fits (never below 4 — an id elided
    // past that says nothing, and the tooltip carries the full one anyway).
    let (mut lo, mut hi) = (4_usize, model.chars().count());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&job(&elide_middle(model, mid))) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    job(&elide_middle(model, lo))
}

/// Elide a model id in the MIDDLE — `hf.co/unsloth/Qwen3-30B:Q4_K_M` keeps
/// both the family and the quant tag, which is what tells two pulled models
/// apart. Head/tail truncation drops exactly the half that disambiguates.
fn elide_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max || max < 4 {
        return s.to_string();
    }
    let keep = max - 1; // the ellipsis costs one
    let head = keep.div_ceil(2);
    let tail = keep - head;
    format!(
        "{}…{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

/// Token counts for the composer's status rail: `1234` → `1.2k`, so the
/// cumulative usage can never grow wide enough to shove the send button.
fn compact_tokens(n: i64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

/// A user message: right-aligned boxed bubble with a "you" tag. A queued send
/// (mid-turn) is dimmed and tagged. Free function so the message loop can
/// call it without borrowing `self` (which conflicts with `&mut self.md`).
fn user_bubble(ui: &mut egui::Ui, text: &str, queued: bool) {
    ui.add_space(6.0);
    let max_w = (ui.available_width() * 0.85).max(120.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
        egui::Frame::default()
            .fill(crate::theme::PANEL)
            .stroke(egui::Stroke::new(1.0_f32, crate::theme::HAIRLINE))
            .corner_radius(egui::CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_max_width(max_w);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(if queued { "you · queued" } else { "you" })
                            .small()
                            .color(crate::theme::TEXT_FAINT),
                    );
                    let color = if queued {
                        crate::theme::TEXT_MUTED
                    } else {
                        crate::theme::TEXT
                    };
                    ui.label(egui::RichText::new(text).color(color));
                });
            });
    });
}

/// Collapsible tool-call cards. Free function (same reason as `user_bubble`).
fn tool_cards(ui: &mut egui::Ui, cards: &[ToolCard]) {
    for card in cards {
        // Collapsed by default: the header is the status glyph + the
        // human tool name; args and result expand on click. (Wall-of-
        // JSON cards buried the conversation — operator 2026-08-14.)
        // The glyph comes from the phosphor family via a LayoutJob: the old
        // "✓ / ✗ / …" literals aren't in Inter-Light and rendered as tofu
        // boxes.
        let (glyph, color) = match &card.result {
            None => (
                egui_phosphor::regular::CIRCLE_DASHED,
                crate::theme::rgb(200, 150, 80),
            ),
            Some(_) if card.is_error => (
                egui_phosphor::regular::X_CIRCLE,
                crate::theme::SIGNAL_RED,
            ),
            Some(_) => (
                egui_phosphor::regular::CHECK_CIRCLE,
                crate::theme::HEALTHY,
            ),
        };
        egui::CollapsingHeader::new(crate::theme::icon_label(
            glyph,
            13.0,
            color,
            &human_tool_name(&card.name),
            crate::theme::mono(12.5),
            color,
        ))
        .id_salt(format!("tool_{}_{}", card.id, card.name))
        .default_open(false)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(&card.name).monospace().small().weak());
            ui.label(
                egui::RichText::new(&card.args)
                    .monospace()
                    .small()
                    .color(crate::theme::TEXT_MUTED),
            );
            if let Some(result) = &card.result {
                ui.add_space(4.0);
                crate::theme::hairline(ui);
                ui.add_space(4.0);
                ui.label(egui::RichText::new(result).monospace().small());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compact_tokens, elide_middle, human_tool_name, BubbleKind, ChatPanelView, Rendered,
        MAX_VISIBLE_MESSAGES,
    };

    #[test]
    fn visible_history_has_an_explicit_bound() {
        let mut panel = ChatPanelView::default();
        panel.messages.extend((0..MAX_VISIBLE_MESSAGES + 5).map(|i| Rendered {
            text: i.to_string(),
            tools: Vec::new(),
            kind: BubbleKind::Notice,
            queued: false,
        }));
        panel.messages.drain(..panel.messages.len() - MAX_VISIBLE_MESSAGES);
        assert_eq!(panel.messages.len(), MAX_VISIBLE_MESSAGES);
        assert_eq!(panel.messages.first().unwrap().text, "5");
    }

    #[test]
    fn model_ids_elide_in_the_middle_keeping_family_and_tag() {
        // Short enough to fit: untouched.
        assert_eq!(elide_middle("qwen3:8b", 20), "qwen3:8b");
        // Both ends survive — the quant tag is what tells two pulls apart.
        let long = "hf.co/unsloth/Qwen3-30B-A3B-GGUF:Q4_K_M";
        let short = elide_middle(long, 20);
        assert_eq!(short.chars().count(), 20);
        assert!(short.starts_with("hf.co/"), "family head kept: {short}");
        assert!(short.ends_with("Q4_K_M"), "quant tail kept: {short}");
        // Multi-byte ids are cut on char boundaries, never mid-codepoint.
        assert_eq!(elide_middle("ααααααααββββββββ", 5).chars().count(), 5);
    }

    #[test]
    fn usage_counts_compact_so_they_cannot_widen_the_rail() {
        assert_eq!(compact_tokens(0), "0");
        assert_eq!(compact_tokens(999), "999");
        assert_eq!(compact_tokens(18_432), "18.4k");
        assert_eq!(compact_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn human_tool_names_cover_the_catalog_and_roles() {
        assert_eq!(human_tool_name("corpus-admin__agent_list"), "list agents");
        assert_eq!(human_tool_name("agent_new"), "create agent");
        assert_eq!(human_tool_name("corpus-admin__mission_set_budget"), "set mission budget");
        assert_eq!(human_tool_name("delegate › agent-builder"), "delegate to agent-builder");
        assert_eq!(
            human_tool_name("project-manager›corpus-admin__project_new"),
            "project-manager › create project"
        );
        assert_eq!(human_tool_name("corpus_wipe"), "WIPE corpus");
        // Fallback: unknown names strip the extension prefix and underscores.
        assert_eq!(human_tool_name("corpus-admin__future_thing"), "future thing");
        // Every catalog tool has a non-empty human name.
        for t in crate::chat::team::ALL_ADMIN_TOOLS {
            assert!(!human_tool_name(t).is_empty(), "{t} must render");
        }
    }
}
