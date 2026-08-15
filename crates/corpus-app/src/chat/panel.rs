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
}

struct Rendered {
    text: String,
    /// Our tool-call cards, in arrival order within this bubble.
    tools: Vec<ToolCard>,
    kind: BubbleKind,
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
    open: bool,
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
            live_turn: 0,
            last_error: None,
            usage: (0, 0),
            role: crate::chat::team::TeamRole::Operator,
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

    /// Drain events from `chat` into the view.
    pub fn absorb(&mut self, chat: &dyn Chat) {
        for ev in chat.poll_events() {
            match ev {
                ChatEvent::TextChunk { delta, .. } => {
                    self.activity = Activity::Streaming;
                    // Coalesce streamed chunks into the open assistant bubble.
                    match self.messages.last_mut() {
                        Some(m) if m.kind == BubbleKind::Assistant => m.text.push_str(&delta),
                        _ => self.messages.push(Rendered {
                            text: delta,
                            tools: Vec::new(),
                            kind: BubbleKind::Assistant,
                        }),
                    }
                }
                ChatEvent::ThinkingChunk { delta, .. } => {
                    self.activity = Activity::Thinking;
                    // Coalesce streamed reasoning into the open thought card.
                    match self.messages.last_mut() {
                        Some(m) if m.kind == BubbleKind::Thought => m.text.push_str(&delta),
                        _ => self.messages.push(Rendered {
                            text: delta,
                            tools: Vec::new(),
                            kind: BubbleKind::Thought,
                        }),
                    }
                }
                ChatEvent::ToolCallStart { id, name, args_json, .. } => {
                    self.activity = Activity::Tool(name.clone());
                    self.messages.push(Rendered {
                        text: String::new(),
                        tools: vec![ToolCard {
                            id,
                            name,
                            args: args_json,
                            result: None,
                            is_error: false,
                            open: true,
                        }],
                        kind: BubbleKind::Tool,
                    });
                }
                ChatEvent::ToolCallResult { id, is_error, output, .. } => {
                    self.activity = Activity::Thinking;
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
                    self.activity = Activity::Thinking;
                    self.live_turn = turn;
                }
                ChatEvent::TurnEnd { turn } => {
                    if turn >= self.live_turn {
                        self.activity = Activity::Idle;
                    }
                }
                ChatEvent::Usage { input_tokens, output_tokens, .. } => {
                    self.usage.0 += input_tokens.unwrap_or(0) as i64;
                    self.usage.1 += output_tokens.unwrap_or(0) as i64;
                }
                ChatEvent::Error(e) => {
                    self.activity = Activity::Idle;
                    self.last_error = Some(e.clone());
                    self.messages.push(Rendered {
                        text: format!("error: {e}"),
                        tools: Vec::new(),
                        kind: BubbleKind::Error,
                    });
                }
            }
        }
    }

    fn can_send(&self, chat: &dyn Chat) -> bool {
        !self.model.is_empty() && chat.phase() == ChatPhase::Ready && !self.input.trim().is_empty()
    }

    /// The agent's live activity, rendered IN the log (chronologically after
    /// its last message/tool card) — "the thought process and actions read in
    /// order" — instead of the old detached footer line. None when idle: the
    /// log shows history only.
    fn live_activity(&self, chat: &dyn Chat) -> Option<(String, egui::Color32)> {
        let busy = crate::theme::rgb(200, 150, 80);
        match chat.phase() {
            ChatPhase::Connecting => Some(("connecting / model loading…".into(), busy)),
            ChatPhase::Ready => match &self.activity {
                Activity::Idle => None,
                Activity::Thinking => Some(("corpus is thinking…".into(), busy)),
                Activity::Streaming => Some(("corpus is replying…".into(), busy)),
                Activity::Tool(name) => Some((format!("running tool › {name}…"), busy)),
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
            ui.weak(&format!("— {}", chat.project()));
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
                for m in &self.messages {
                    match m.kind {
                        BubbleKind::User => self.user_bubble(ui, &m.text),
                        BubbleKind::Assistant => {
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new("corpus")
                                    .small()
                                    .color(crate::theme::TEXT_FAINT),
                            );
                            CommonMarkViewer::new().show(ui, &mut self.md, &m.text);
                        }
                        BubbleKind::Thought => {
                            ui.add_space(4.0);
                            // The model's reasoning, collapsed by default
                            // (traces run to thousands of tokens) but always
                            // present in the log, in order.
                            egui::CollapsingHeader::new(
                                egui::RichText::new("thought")
                                    .small()
                                    .italics()
                                    .color(crate::theme::TEXT_FAINT),
                            )
                            .id_salt(format!("thought_{}", m.text.len()))
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
                                    self.tool_cards(ui, &m.tools);
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
                    }
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

        // Input row: the text edit is ALWAYS editable once a model is
        // chosen (typing must not be silently gated); only SEND is gated
        // on readiness + non-empty. The send button's space is reserved
        // first so both stay fully visible at the 280px min width.
        ui.horizontal(|ui| {
            let can_send = self.can_send(chat);
            let text_w =
                (ui.available_width() - 64.0).max(20.0); // 64 = button + spacing
            let response = ui.add_enabled(
                !self.model.is_empty(),
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("message…")
                    .desired_width(text_w),
            );
            let submit = ui
                .add_enabled(can_send, egui::Button::new("send"))
                .clicked()
                || (can_send
                    && response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if submit {
                let msg = self.input.trim().to_string();
                if !msg.is_empty() {
                    self.messages.push(Rendered {
                        text: msg.clone(),
                        tools: Vec::new(),
                        kind: BubbleKind::User,
                    });
                    chat.send(&msg);
                    self.input.clear();
                }
            }
        });

        // Visible backend status line (connecting / ready / failed:<err>),
        // with a stop affordance while a turn is live (a thinking model can
        // run for minutes — the operator must be able to cut it).
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
            if chat.phase() == ChatPhase::Ready && !matches!(self.activity, Activity::Idle) {
                if ui.small_button("stop").clicked() {
                    chat.stop();
                }
            }
        });
    }

    /// A user message: right-aligned boxed bubble with a "you" tag, so every
    /// log entry is unambiguous about who said it (the "no indication who
    /// sent what" bug).
    fn user_bubble(&self, ui: &mut egui::Ui, text: &str) {
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
                            egui::RichText::new("you")
                                .small()
                                .color(crate::theme::TEXT_FAINT),
                        );
                        ui.label(egui::RichText::new(text).color(crate::theme::TEXT));
                    });
                });
        });
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
        for card in cards {
            egui::CollapsingHeader::new(
                egui::RichText::new(format!("tool › {}", card.name)).monospace(),
            )
            .id_salt(format!("tool_{}_{}", card.name, card.args.len()))
            .default_open(card.open)
            .show(ui, |ui| {
                ui.label(egui::RichText::new(&card.args).monospace().small());
                if let Some(result) = &card.result {
                    ui.separator();
                    ui.label(egui::RichText::new(result).monospace().small());
                }
            });
        }
    }

    fn permission_cards(&mut self, ui: &mut egui::Ui, chat: &mut ChatHandle) {
        // Take the pending list by value so clicks can mutate chat; a card is
        // re-added ONLY if the operator didn't resolve it this frame (the old
        // unconditional re-add made every card immortal — the "stale
        // approve/reject" bug).
        let pending = std::mem::take(&mut self.pending);
        for p in pending {
            let mut resolved = false;
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!("⛨ permission requested › {}", p.tool)).strong(),
                );
                ui.label(egui::RichText::new(&p.summary).monospace().small());
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
