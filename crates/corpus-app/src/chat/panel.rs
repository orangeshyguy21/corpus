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
    /// The last backend failure, surfaced as a visible status (never silently).
    last_error: Option<String>,
    /// The team role this session runs as (default: the Orchestrator).
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
    Tool,
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
            last_error: None,
            role: crate::chat::team::TeamRole::Orchestrator,
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
                ChatEvent::ToolCallStart { id, name, args_json, .. } => {
                    self.activity = Activity::Tool(name.clone());
                    self.messages.push(Rendered {
                        text: String::new(),
                        tools: vec![ToolCard {
                            name,
                            args: args_json,
                            result: None,
                            is_error: false,
                            open: true,
                        }],
                        kind: BubbleKind::Tool,
                    });
                    let _ = id;
                }
                ChatEvent::ToolCallResult { id, is_error, output, .. } => {
                    self.activity = Activity::Thinking;
                    // A resolved permission request's tool result arriving
                    // clears its card (backstop to the click path below).
                    self.pending.retain(|p| p.id != id);
                    for m in self.messages.iter_mut().rev() {
                        if m.kind == BubbleKind::Tool {
                            if let Some(card) = m.tools.last_mut() {
                                card.result = Some(output.clone());
                                card.is_error = is_error;
                                break;
                            }
                        }
                    }
                    let _ = id;
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
                ChatEvent::TurnStart { .. } => self.activity = Activity::Thinking,
                ChatEvent::TurnEnd { .. } => self.activity = Activity::Idle,
                ChatEvent::Error(e) => {
                    self.activity = Activity::Idle;
                    self.last_error = Some(e.clone());
                    self.messages.push(Rendered {
                        text: format!("error: {e}"),
                        tools: Vec::new(),
                        kind: BubbleKind::Assistant,
                    });
                }
            }
        }
    }

    fn can_send(&self, chat: &dyn Chat) -> bool {
        !self.model.is_empty() && chat.phase() == ChatPhase::Ready && !self.input.trim().is_empty()
    }

    /// The human-readable backend status line shown just above the input.
    /// While a turn is live this is ACTIVITY (thinking / streaming / running
    /// tool) — the silence between send and first chunk was the "no
    /// feedback" complaint.
    fn status(&self, chat: &dyn Chat) -> (String, egui::Color32) {
        let busy = egui::Color32::from_rgb(200, 150, 80);
        match chat.phase() {
            ChatPhase::Idle => ("no backend".into(), egui::Color32::GRAY),
            ChatPhase::Connecting => ("connecting / model loading…".into(), busy),
            ChatPhase::Ready => match &self.activity {
                Activity::Idle => ("ready".into(), egui::Color32::from_rgb(110, 180, 110)),
                Activity::Thinking => ("thinking…".into(), busy),
                Activity::Streaming => ("streaming…".into(), busy),
                Activity::Tool(name) => (format!("running tool › {name}…"), busy),
            },
            ChatPhase::Finished => (
                match &self.last_error {
                    Some(e) => format!("failed: {e}"),
                    None => "session ended".into(),
                },
                egui::Color32::from_rgb(210, 90, 90),
            ),
        }
    }

    /// Render the panel; returns nothing, mutates chat/self.
    pub fn show(&mut self, ui: &mut egui::Ui, chat: &mut ChatHandle) {
        ui.horizontal(|ui| {
            ui.heading("Chat");
            ui.weak(&format!("— {}", chat.project()));
            // The role is ALWAYS the orchestrator (operator decision: the
            // user never picks); it is named, not selectable.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(egui::RichText::new(format!("role: {}", self.role.label())).small());
            });
        });
        ui.separator();

        // Model gate: no model -> refuse to start (and say so honestly).
        if self.model.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(200, 120, 60),
                "no model selected — choose a chat model to start",
            );
            return;
        }

        // Message scroll with explicit height reservation, then the input row
        // + status line below it. (NOT bottom_up layout — it inverts the
        // ScrollArea's content stacking: the "new message stacks on top" bug.)
        let footer_h = 58.0; // input row + status line + separator
        let scroll_h = (ui.available_height() - footer_h).max(0.0);
        // Scrollable message area. Horizontal shrink is ON so a wide
        // markdown line can never force the panel wider than its clamp.
        egui::ScrollArea::vertical()
            .max_height(scroll_h)
            .auto_shrink([true, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for m in &self.messages {
                    match m.kind {
                        BubbleKind::User => {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(&m.text).strong());
                        }
                        BubbleKind::Assistant => {
                            ui.add_space(6.0);
                            CommonMarkViewer::new().show(ui, &mut self.md, &m.text);
                        }
                        BubbleKind::Tool => {
                            self.tool_cards(ui, &m.tools);
                        }
                    }
                }
                // Inline approve/reject cards.
                self.permission_cards(ui, chat);
            });
        ui.separator();

        // Input row: the text edit is ALWAYS editable once a model is
        // chosen (typing must not be silently gated); only SEND is gated
        // on readiness + non-empty. The send button's space is reserved
        // first so both stay fully visible at the 280px min width.
        ui.horizontal(|ui| {
            let can_send = self.can_send(chat);
            let text_w =
                (ui.available_width() - 64.0).max(20.0); // 64 = button + spacing
            let response = ui.add(
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

        // Visible backend status line (Connecting / Ready / failed:<err>).
        let (text, color) = self.status(chat);
        ui.horizontal(|ui| {
            ui.colored_label(color, egui::RichText::new(text).small());
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
