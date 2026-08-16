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

/// Panel-local render state for one conversation.
pub struct ChatPanelView {
    /// Events still on screen (roll the tail). Kept small; the transcript is
    /// the durable memory.
    messages: Vec<Rendered>,
    input: String,
    model: String,
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
        events
    }

    fn can_send(&self, chat: &dyn Chat) -> bool {
        !self.model.is_empty() && chat.phase() == ChatPhase::Ready && !self.input.trim().is_empty()
    }

    /// The agent's live activity, rendered IN the log (chronologically after
    /// its last message/tool card) — "the thought process and actions read in
    /// order" — instead of the old detached footer line. None when idle: the
    /// log shows history only. Animated: the ellipsis cycles and the elapsed
    /// timer counts (main.rs repaints at 250 ms for the toast loop anyway).
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

    /// The footer status line: backend PHASE only (connecting / ready /
    /// failed). Turn activity lives in the log (`live_activity`).
    fn status(&self, chat: &dyn Chat) -> (String, egui::Color32) {
        match chat.phase() {
            ChatPhase::Idle => ("no backend — pick a model below".into(), crate::theme::TEXT_FAINT),
            ChatPhase::Connecting => ("connecting…".into(), crate::theme::rgb(200, 150, 80)),
            ChatPhase::Ready => ("ready".into(), crate::theme::OK),
            ChatPhase::Finished => (
                match &self.last_error {
                    Some(e) => format!("failed: {e}"),
                    None => "session ended".into(),
                },
                crate::theme::DANGER,
            ),
        }
    }

    /// Render the panel; returns nothing, mutates chat/self.
    pub fn show(&mut self, ui: &mut egui::Ui, chat: &mut ChatHandle) {
        ui.horizontal(|ui| {
            ui.heading("Chat");
            let label = if self.project_label.is_empty() {
                chat.project()
            } else {
                self.project_label.clone()
            };
            ui.weak(&format!("— {label}"));
            // The role selector (default Operator: full catalog, destructive
            // ops gated by inline Approve/Reject). A change restarts the
            // session (main.rs ensure_chat_started compares the role).
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                crate::theme::combo_field(ui, |ui| {
                    egui::ComboBox::from_id_salt("chat_role")
                        .icon(crate::theme::combo_caret)
                        .selected_text(
                            egui::RichText::new(format!("role: {}", self.role.label())).small(),
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
            });
        });
        ui.separator();

        // Message scroll with explicit height reservation, then the footer
        // (model picker + input row + status line) below it. (NOT bottom_up
        // layout — it inverts the ScrollArea's content stacking: the "new
        // message stacks on top" bug.)
        let footer_h = 88.0; // picker row + input row + status line + separator
        let scroll_h = (ui.available_height() - footer_h).max(0.0);
        // Scrollable message area. Horizontal shrink is ON so a wide
        // markdown line can never force the panel wider than its clamp.
        egui::ScrollArea::vertical()
            .max_height(scroll_h)
            .auto_shrink([true, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if self.model.is_empty() {
                    ui.label(
                        egui::RichText::new("no model selected — choose a chat model below to start")
                            .color(crate::theme::rgb(200, 120, 60))
                            .small(),
                    );
                } else if self.messages.is_empty() {
                    ui.label(
                        egui::RichText::new("no messages yet — say hello below")
                            .color(crate::theme::TEXT_FAINT)
                            .small(),
                    );
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
                            ui.add_space(4.0);
                            egui::Frame::default()
                                .fill(crate::theme::EDITOR_BG)
                                .stroke(egui::Stroke::new(1.0_f32, crate::theme::HAIRLINE))
                                .corner_radius(egui::CornerRadius::same(2))
                                .inner_margin(egui::Margin::symmetric(8, 4))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    tool_cards(ui, &m.tools);
                                });
                        }
                        BubbleKind::Error => {
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(&m.text)
                                    .color(crate::theme::DANGER)
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
        ui.separator();

        // The model picker lives with the input (operator decision), not up
        // in the header. Driven by corpus-core's `ollama_models()` — the
        // GDK chat talks to the Ollama server DIRECTLY, so it lists what
        // Ollama has pulled, never opencode's catalog. Never an ambient
        // default: with no model selected, SEND stays gated.
        self.model_picker(ui);

        // Input row: an auto-growing multiline box (grows with content up to
        // a cap, then scrolls) beside ONE button that transforms — `send`
        // while idle, a red `stop` while a turn is live. Enter sends,
        // Shift+Enter inserts a newline; Esc stops a live turn.
        self.input_row(ui, chat);

        // Status line: backend phase + cumulative usage. The stop affordance
        // now lives in the transforming button above, not here.
        let (text, color) = self.status(chat);
        ui.horizontal(|ui| {
            ui.colored_label(color, egui::RichText::new(text).small());
            if self.usage != (0, 0) {
                ui.label(
                    egui::RichText::new(format!("· ↑{} ↓{} tokens", self.usage.0, self.usage.1))
                        .small()
                        .color(crate::theme::TEXT_FAINT),
                );
            }
        });
    }

    /// The composer: the auto-growing input + the transforming send/stop
    /// button, plus the keyboard contract (Enter send, Shift+Enter newline,
    /// Esc stop).
    fn input_row(&mut self, ui: &mut egui::Ui, chat: &mut dyn Chat) {
        let is_live = chat.phase() == ChatPhase::Ready && !matches!(self.activity, Activity::Idle);
        // Esc is a global stop while a turn runs, wherever focus is.
        if is_live && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            chat.stop();
        }
        ui.horizontal_top(|ui| {
            let btn_w = 58.0;
            let text_w = (ui.available_width() - btn_w - 8.0).max(20.0);

            // The box grows with its content: 1 row minimum, capped so a
            // long paste scrolls instead of eating the log. Its width is
            // reserved explicitly so the button always keeps its slot.
            let editor = egui::TextEdit::multiline(&mut self.input)
                .hint_text("message…")
                .desired_width(text_w)
                .desired_rows(1)
                .id_salt("chat_input");
            let response = ui
                .allocate_ui(egui::vec2(text_w, 0.0), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .id_salt("chat_input_scroll")
                        .show(ui, |ui| ui.add_enabled(!self.model.is_empty(), editor))
                        .inner
                })
                .inner;

            // Enter submits; Shift+Enter is a newline (left for the editor).
            // The editor has already inserted the '\n' for a plain Enter, so
            // strip it back off before sending.
            let plain_enter = response.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
            if plain_enter && self.input.ends_with('\n') {
                self.input.pop();
            }

            let can_send = self.can_send(chat);
            let clicked = if is_live {
                ui.add(egui::Button::new(
                    egui::RichText::new("stop").color(crate::theme::DANGER),
                ))
                .clicked()
            } else {
                ui.add_enabled(can_send, egui::Button::new("send")).clicked()
            };

            if is_live {
                if clicked {
                    chat.stop();
                }
            } else if clicked || (plain_enter && can_send) {
                self.submit(chat);
            }
        });
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
    /// corpus-core, never opencode's catalog). Lives in the footer beside
    /// the input row.
    fn model_picker(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("model").small().color(crate::theme::TEXT_MUTED));
            let current = self.model.clone();
            crate::theme::combo_field(ui, |ui| {
                egui::ComboBox::from_id_salt("chat_model")
                    .icon(crate::theme::combo_caret)
                    .selected_text(
                        egui::RichText::new(if current.is_empty() {
                            "choose…".to_string()
                        } else {
                            current.clone()
                        })
                        .small(),
                    )
                    .show_ui(ui, |ui| {
                        if let Ok(list) = corpus_core::ollama_models() {
                            for g in &list.groups {
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
                            }
                        } else {
                            ui.label("ollama not available — a model is required");
                        }
                    });
            });
        });
    }

    fn tool_cards(&self, ui: &mut egui::Ui, cards: &[ToolCard]) {
        tool_cards(ui, cards)
    }

    fn permission_cards(&mut self, ui: &mut egui::Ui, chat: &mut ChatHandle) {        // Take the pending list by value so clicks can mutate chat; a card is
        // re-added ONLY if the operator didn't resolve it this frame (the old
        // unconditional re-add made every card immortal — the "stale
        // approve/reject" bug).
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let mut resolved = false;
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "⛨ approval needed › {}",
                        human_tool_name(&p.tool)
                    ))
                    .strong(),
                );
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
                ui.horizontal(|ui| {
                    if ui.button("Approve").clicked() {
                        chat.approve(&p.id);
                        resolved = true;
                    }
                    if ui.button("Reject").clicked() {
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
        let (glyph, color) = match &card.result {
            None => ("…", crate::theme::rgb(200, 150, 80)),
            Some(_) if card.is_error => ("✗", crate::theme::DANGER),
            Some(_) => ("✓", crate::theme::OK),
        };
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("{glyph} {}", human_tool_name(&card.name)))
                .monospace()
                .color(color),
        )
        .id_salt(format!("tool_{}_{}", card.id, card.name))
        .default_open(false)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(&card.name).monospace().small().weak());
            ui.label(egui::RichText::new(&card.args).monospace().small());
            if let Some(result) = &card.result {
                ui.separator();
                ui.label(egui::RichText::new(result).monospace().small());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::human_tool_name;

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
