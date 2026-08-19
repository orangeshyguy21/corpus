//! corpus-app: the operator's window into the research team (egui).
//!
//! The shell is a three-column command centre: scoped project navigation,
//! a flexible Project/Agent/Mission workspace, and collapsible management
//! chat. The top bar owns source pins and environment health. Missions render
//! their own terminal directly in the workspace; there is no separate launch
//! screen or run takeover.
//!
//! House rules: corpus-core calls live behind `AppState` (state.rs); one
//! module per screen; no business logic in widgets.

mod chat;
mod fmt;
mod file_watch;
mod jobs;
mod nav;
mod session_service;
mod sidebar;
mod state;
mod terminal;
mod theme;
mod views;

use std::sync::Arc;

use eframe::egui;
use egui_phosphor::regular as ph;

use crate::chat::Chat as _;
use crate::nav::Screen;
use crate::sidebar::Sidebar;
use crate::state::AppState;
use crate::views::{agents, components, missions, projects};

/// corpus-app application state: the app's state layer, per-screen widget
/// state, and the toast overlay. The active screen and chat toggle live on
/// `AppState` (they reflect the sidebar/top-bar chrome).
struct App {
    state: AppState,
    /// The screen shown last frame — screen-change hooks (fresh project
    /// list before Agents/Missions render) fire on transitions only.
    last_screen: Screen,
    sidebar: Sidebar,
    projects: projects::ProjectsView,
    agents: agents::AgentsView,
    missions: missions::MissionsView,
    toasts: egui_toast::Toasts,
    /// The management chat (dev/decisions.md + dev/decisions.md): a native egui
    /// panel backed by the EMBEDDED goose runtime (`chat/embedded.rs`). All
    /// GDK lives in `chat`.
    chat: chat::ChatHandle,
    chat_panel: chat::panel::ChatPanelView,
    /// The team role the current chat backend was launched as
    /// (dev/decisions.md chunk 3); a change restarts the scoped session.
    chat_role: chat::team::TeamRole,
    /// The model the current chat backend was launched with; a picker change
    /// restarts the session (the old code kept the old model silently).
    chat_model: String,
    /// The model last written to `store/app.yaml` — the guard that keeps the
    /// persistence check an in-memory comparison instead of a per-frame read.
    chat_model_saved: String,
    /// Last operator-position context pushed to the chat backend (re-pushed
    /// only on change).
    last_chat_context: String,
    /// The chat panel's width in app state (280..=half window). The app
    /// owns panel widths outright — panels render at `exact_width` and a
    /// custom drag divider moves them; egui's native resize is OFF (its
    /// PanelState/content feedback loop jittered and fought the drag).
    chat_width: f32,
    /// The sidebar's width (160..=~45% window), same app-owned mechanics.
    sidebar_width: f32,
    /// A live divider drag's press anchor (panel, starting width, starting
    /// pointer x). Anchored math means the width is RECOMPUTED from the
    /// anchor each frame — it can't accumulate error, fight content width,
    /// or drift at the clamps.
    divider_drag: Option<DividerDrag>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let state = AppState::from_env_deferred(cc.egui_ctx.clone());
        // Restore the remembered chat model (store/app.yaml). Only the
        // PICKER is restored, not a session: the backend starts on the first
        // frame the chat panel actually renders, so a launch with the panel
        // closed still spawns nothing. A model that ollama no longer has
        // fails visibly on that first start rather than being silently
        // dropped here — checking would mean probing ollama during boot.
        let remembered = state.prefs().chat_model;
        let mut chat_panel = chat::panel::ChatPanelView::default();
        chat_panel.set_model(&remembered);
        Self {
            chat_model_saved: remembered,
            last_screen: state.current_screen,
            sidebar: Sidebar::default(),
            projects: projects::ProjectsView::default(),
            agents: agents::AgentsView::default(),
            missions: missions::MissionsView::default(),
            toasts: egui_toast::Toasts::new(),
            chat: chat::ChatHandle::idle(""),
            chat_panel,
            chat_role: chat::team::TeamRole::Operator,
            chat_model: String::new(),
            last_chat_context: String::new(),
            chat_width: 360.0,
            sidebar_width: 200.0,
            divider_drag: None,
            state,
        }
    }
}

/// Which side panel a divider drag is moving.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
enum Divider {
    Sidebar,
    Chat,
}

/// The live drag anchor: where the press started and what width the panel
/// had then. Recomputed-from per frame — accumulation can never jitter.
#[derive(Clone, Copy)]
struct DividerDrag {
    target: Divider,
    start_width: f32,
    start_x: f32,
}

/// The width an anchored divider drag yields: `start_width` plus the
/// pointer displacement in the panel's widening direction, clamped. Pure
/// so the drag contract is unit-tested (drags must track the pointer
/// exactly and stop dead at the clamps — never fight, never drift).
fn dragged_width(target: Divider, drag: DividerDrag, pointer_x: f32, min: f32, max: f32) -> f32 {
    let dx = pointer_x - drag.start_x;
    let delta = match target {
        Divider::Sidebar => dx,   // a left panel widens as you pull right
        Divider::Chat => -dx,     // a right panel widens as you pull left
    };
    (drag.start_width + delta).clamp(min, max)
}

impl eframe::App for App {
    /// Don't persist egui's GUI memory (window positions) to disk. Panel
    /// widths are app-owned now (see `chat_width`), so egui memory has
    /// nothing pathological left to restore.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for (is_error, message) in self.state.poll_background_jobs() {
            self.toasts.add(
                egui_toast::Toast::new()
                    .kind(if is_error {
                        egui_toast::ToastKind::Error
                    } else {
                        egui_toast::ToastKind::Info
                    })
                    .text(message),
            );
        }
        // Keep the selected project's scoped caches loaded (only hits disk
        // when the selection changed).
        self.state.ensure_selection();
        // Native filesystem events only invalidate coarse cache domains;
        // the normal readers below perform reconciliation and retain their
        // timed backstops for startup/missed events.
        if let Some(warning) = self.state.poll_file_invalidations() {
            self.toasts.add(
                egui_toast::Toast::new()
                    .kind(egui_toast::ToastKind::Info)
                    .text(warning),
            );
        }
        // Keep the sidebar's agent status dots honest: poll tmux on a
        // throttle when a live session can exist (never per frame).
        self.state.poll_live_sessions();
        // Keep the selected project's agent list, mission list and corpus
        // summary current as the curator mutates them from the MCP process
        // (throttled re-list — replaces the old manual refresh button).
        self.state.poll_project_scope();
        // Notice a run ending wherever the operator happens to be: polled
        // here rather than only in the mission view, which sees nothing
        // while another screen is up. A run that dies on its own used to
        // pass in total silence — the pane simply went idle.
        self.state.poll_run();
        self.report_run_exit();
        // Honor any launch the curator requested (from the MCP process) and
        // announce it — an autonomous launch the operator did not initiate
        // should still surface, wherever they are.
        self.state.poll_launch_requests();
        self.report_launch_notices();
        self.clamp_panel_widths(ctx);
        // Screen-change hooks.
        if self.state.current_screen != self.last_screen {
            self.last_screen = self.state.current_screen;
            match self.state.current_screen {
                Screen::Agents | Screen::Missions => self.state.refresh(),
                _ => {}
            }
            // The mission view's attach precedence consults the live tmux
            // session list; refresh it on screen entry (never per-frame).
            if matches!(self.state.current_screen, Screen::Missions) {
                self.state.refresh_live_sessions();
            }
        }
        self.top_bar(ctx);
        let sidebar_rect = self.sidebar_panel(ctx);
        let chat_rect = if self.state.chat_open {
            Some(self.chat_panel(ctx))
        } else {
            None
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(theme::BG))
            .show(ctx, |ui| {
                components::paint_command_canvas(ui);
            // Chunk 5: the run lives in the Missions view — no `Launch`
            // takeover of the main column. The mission view renders the
            // terminal edge-to-edge (zero margin); Projects/Agents pad
            // themselves with the old 24/18 inset.
            match self.state.current_screen {
                Screen::Projects => {
                    padded(ui, |ui| {
                        self.projects.show(ui, &mut self.state, &mut self.toasts);
                    });
                }
                Screen::Agents => {
                    padded(ui, |ui| {
                        self.agents.show(ui, &mut self.state, &mut self.toasts);
                    });
                }
                Screen::Missions => {
                    self.missions.show(ui, &mut self.state, &mut self.toasts);
                }
            }
        });

        // Drag dividers LAST: the grab zone paints/interacts over the edge
        // band, so it must come after the panels whose borders it straddles.
        if let Some(w) = self.drag_divider(ctx, Divider::Sidebar, sidebar_rect) {
            self.sidebar_width = w;
        }
        if let Some(rect) = chat_rect {
            if let Some(w) = self.drag_divider(ctx, Divider::Chat, rect) {
                self.chat_width = w;
            }
        }

        // Toast overlay (top-right of the whole window).
        self.toasts.show(ctx);

        // Polling has an explicit owner. Jobs, terminal output and chat
        // events wake egui directly; only a live run/session needs a clock
        // for liveness and activity transitions. Toasts schedule their own
        // expiry in egui-toast. A truly idle app schedules no next frame.
        if let Some(after) = self.state.live_repaint_after() {
            ctx.request_repaint_after(after);
        }
    }
}

impl App {
    /// Report a run that ended on its own — once, wherever the operator is.
    ///
    /// A non-zero code is an ERROR toast: the agent died, and the only
    /// other sign is a pane that stopped moving. A clean exit is an INFO
    /// one — the operator quit opencode themselves, so it needs
    /// acknowledgement, not alarm. An operator STOP reports through its
    /// own action and never lands here.
    fn report_run_exit(&mut self) {
        let Some(exit) = self.state.take_run_exit() else {
            return;
        };
        let (kind, text) = exit_notice(&exit);
        self.toasts.add(
            egui_toast::Toast::new()
                .kind(kind)
                .text(text)
                .options(
                    egui_toast::ToastOptions::default()
                        .duration(std::time::Duration::from_secs(8)),
                ),
        );
    }

    /// Announce each launch the curator carried out this beat. A success is
    /// an INFO toast pointing the operator at the sidebar (the mission is
    /// now a live TUI they can select and watch); a failure is an ERROR one
    /// naming what went wrong, since nothing else surfaced it.
    fn report_launch_notices(&mut self) {
        for notice in self.state.take_launch_notices() {
            let (kind, text) = match &notice.result {
                Ok(()) => (
                    egui_toast::ToastKind::Info,
                    format!("curator launched {} — select it to watch", notice.mission),
                ),
                Err(error) => (
                    egui_toast::ToastKind::Error,
                    format!("curator launch of {} failed: {error}", notice.mission),
                ),
            };
            self.toasts.add(
                egui_toast::Toast::new()
                    .kind(kind)
                    .text(text)
                    .options(
                        egui_toast::ToastOptions::default()
                            .duration(std::time::Duration::from_secs(8)),
                    ),
            );
        }
    }

    /// The top bar (spec §3): wordmark left, per-source rev dropdowns +
    /// the live env dot center, chat toggle far right.
    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar")
            .show_separator_line(true)
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // LEFT: the wordmark, height 20px, aspect maintained
                    // (source 1072×325 -> ~66px wide).
                    ui.add(
                        egui::Image::new(egui::include_image!("../assets/logo.png"))
                            .fit_to_exact_size(egui::vec2(66.0, 20.0)),
                    );
                    ui.add_space(theme::SPACING);
                    self.breadcrumb(ui);
                    // Roughly centre the source dropdowns in the space
                    // left between the breadcrumb and the right zone.
                    let remaining = ui.available_width();
                    let dropdown_count = self.state.source_revs.len().max(1);
                    let dropdown_w = (dropdown_count as f32) * 150.0;
                    let right_zone = 200.0;
                    let spacer = ((remaining - dropdown_w - right_zone) / 2.0).max(0.0);
                    ui.add_space(spacer);
                    self.source_dropdowns(ui);
                    // Right zone (right_to_left places rightmost first): the
                    // chat toggle far right, the env status to its left.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let toggle =
                            theme::icon_button(ui, ph::CHATS_CIRCLE, 18.0)
                                .on_hover_text("toggle the chat panel (content lands at chunk 6)");
                        if toggle.clicked() {
                            self.state.chat_open = !self.state.chat_open;
                        }
                        ui.add_space(8.0);
                        self.env_dot(ui);
                    });
                });
            });
    }

    /// The nav breadcrumb: `project > agent > mission` from the current
    /// selections; each segment jumps to its screen. Faint, mono, compact.
    fn breadcrumb(&mut self, ui: &mut egui::Ui) {
        let project = self.state.effective_project().map(|slug| {
            self.state
                .projects
                .iter()
                .find(|(s, _)| s == &slug)
                .map(|(_, p)| if p.name.is_empty() { slug.clone() } else { p.name.clone() })
                .unwrap_or(slug)
        });
        // Match the nesting project > agent > mission, but only as deep as
        // the current screen: an agent screen stops at the agent, a project
        // screen at the project. Labels are display NAMES via the *_label
        // helpers — never the raw uuid slug the selections hold.
        let mut segments: Vec<(String, Screen)> = Vec::new();
        if let Some(label) = project {
            segments.push((label, Screen::Projects));
        }
        match self.state.current_screen {
            Screen::Projects => {}
            Screen::Agents => {
                if let Some(slug) = self.state.selected_agent.clone() {
                    segments.push((self.state.agent_label(&slug), Screen::Agents));
                }
            }
            Screen::Missions => {
                if let Some(slug) = self.state.selected_mission.clone() {
                    // A mission's driving agent is the middle of the trail.
                    if let Some(agent) = self
                        .state
                        .missions
                        .iter()
                        .find(|(s, _)| s == &slug)
                        .map(|(_, m)| m.agent.clone())
                        .filter(|a| !a.is_empty())
                    {
                        segments.push((self.state.agent_label(&agent), Screen::Agents));
                    }
                    segments.push((self.state.mission_label(&slug), Screen::Missions));
                }
            }
        }
        let mut first = true;
        for (label, screen) in segments {
            if !first {
                ui.label(egui::RichText::new("›").size(12.0).color(theme::TEXT_FAINT));
            }
            first = false;
            let active = self.state.current_screen == screen;
            let color = if active { theme::TEXT } else { theme::TEXT_FAINT };
            let response = ui.add(
                egui::Label::new(egui::RichText::new(label).size(12.0).monospace().color(color))
                    .sense(egui::Sense::click()),
            );
            if response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if response.clicked() {
                self.state.current_screen = screen;
            }
        }
    }

    /// The per-source `repo: rev` dropdowns (spec §3): flat PANEL fields in
    /// the plugin picker's style (see `views::source_dropdown`). Options
    /// come from the selected project's plugin (`source_revs`); the
    /// selection is the PROJECT's — persisted on `project.yaml` and
    /// stamped into missions at creation. Declaration order is preserved
    /// (`source_revs` is a Vec). A branch rev (`main`) drawn from an
    /// absent/expired rev cache is amber + tooltipped — it resolves to
    /// the recorded snapshot, not today's head.
    fn source_dropdowns(&mut self, ui: &mut egui::Ui) {
        let revs = self.state.source_revs.clone();
        let project = self.state.effective_project();
        // The rev the live environment is running, as a rev NAME — the mint
        // reports `0.18.0-rc.0`, the matching tag is `v0.18.0-rc.0`.
        let running_rev = project
            .as_deref()
            .and_then(|p| self.state.env_status(p))
            .and_then(|env| env.running_version)
            .map(|ver| format!("v{ver}"));
        for source in &revs {
            let selected = self
                .state
                .source_pins
                .get(&source.name)
                .cloned()
                .unwrap_or_else(|| source.default_rev().to_string());
            // The running version only pertains to the source it VERSIONS:
            // pass it only when it's a rev this source could hold (its tag
            // set, or already selected). Otherwise the spec repo would flash
            // a false mismatch against a mint version that isn't its own.
            let source_running = running_rev.as_deref().filter(|r| {
                source.revs.iter().any(|x| x == r) || selected == **r
            });
            if let Some(rev) = crate::views::source_dropdown::source_dropdown(
                ui,
                &format!("top_source_{}", source.name),
                source,
                &selected,
                source_running,
            ) {
                // Persist the pick onto the project.
                if let Some(project) = &project {
                    if let Err(error) = self.state.set_source_pin(project, &source.name, &rev) {
                        self.toasts.add(
                            egui_toast::Toast::new()
                                .kind(egui_toast::ToastKind::Error)
                                .text(error.to_string()),
                        );
                    }
                }
            }
        }
        if revs.is_empty() {
            if project
                .as_deref()
                .is_some_and(|project| self.state.source_revisions_loading(project))
            {
                ui.weak("loading source revisions…");
            } else {
                ui.weak("no source pins");
            }
        }
    }

    /// The live env status for the selected project's plugin (spec §3): the
    /// plugin name in 13px TEXT beside an 8px filled dot — health-green when
    /// the probe is ready, signal-red when not. The STATUS is inline, not hidden
    /// in a tooltip: a not-ready env appends a short reason (truncated probe
    /// notes) so a dead gateway is visible at a glance. A click forces a
    /// re-probe; hover shows the full probe notes. Rendered as flat text,
    /// NOT a pill button.
    fn env_dot(&mut self, ui: &mut egui::Ui) {
        let Some(project) = self.state.effective_project() else {
            return;
        };
        let (dot, label, notes) = if self.state.env_probe_loading(&project) {
            (
                theme::rgb(200, 150, 80),
                "probing environment…".to_string(),
                "environment probe is running in the background".to_string(),
            )
        } else if let Some(error) = self.state.env_probe_error(&project) {
            (
                theme::SIGNAL_RED,
                "probe failed".to_string(),
                format!("probe failed — {error}; click to retry"),
            )
        } else {
            match self.state.env_status(&project) {
            Some(env) if env.ready => {
                // Ready: name + the version the mint is actually running,
                // so "what's up" is legible at a glance, not buried.
                let label = match &env.running_version {
                    Some(ver) => format!("{}  {ver}", env.name),
                    None => env.name.clone(),
                };
                (
                    theme::HEALTHY,
                    label,
                    "environment ready — click to re-probe".to_string(),
                )
            }
            Some(env) => {
                // Short inline reason: the first probe-note clause, capped —
                // the full notes stay on hover.
                let short: String = env.notes.chars().take(48).collect();
                let short = short.trim_end_matches([' ', ',', ';', '—', '(']).to_string();
                (
                    theme::SIGNAL_RED,
                    if short.is_empty() {
                        format!("{} — not ready", env.name)
                    } else {
                        format!("{} — {short}", env.name)
                    },
                    if env.notes.is_empty() {
                        "environment not ready — click to re-probe".to_string()
                    } else {
                        format!("not ready — {}", env.notes)
                    },
                )
            }
            None => (
                theme::TEXT_MUTED,
                "probe…".to_string(),
                "no probe yet — click to probe".to_string(),
            ),
            }
        };
        // A compact clickable row: dot + 13px label. FIX 2d — the whole
        // region is click-sensitive with a pointing-hand cursor, and the
        // label brightens TEXT_MUTED → TEXT on hover.
                let galley = ui.painter().layout_no_wrap(label.clone(), egui::FontId::proportional(13.0), theme::TEXT);
        let size = egui::vec2(8.0 + 6.0 + galley.size().x, 24.0);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.clicked() {
            self.state.refresh_env(&project);
        }
        let color = if response.hovered() { theme::TEXT } else { theme::TEXT_MUTED };
        let painter = ui.painter_at(rect);
        painter.circle_filled(
            egui::pos2(rect.left() + 4.0, rect.center().y),
            4.0,
            dot,
        );
        painter.text(
            egui::pos2(rect.left() + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &label,
            egui::FontId::proportional(13.0),
            color,
        );
        response.on_hover_text(notes);
    }

    /// The left sidebar (app-flow chunk 1): the three scoped sections
    /// (Projects / Agents / Missions) with `+` create flows, the selected
    /// project's dots-three-vertical menu, and the bottom corpus summary.
    /// Rendered by the `sidebar` module; this wrapper only owns the panel
    /// chrome. The width is APP-OWNED (`exact_width`): egui's native
    /// resize is off — the drag divider is the only mover (native resize
    /// fed its content width back into PanelState — the jitter source).
    fn sidebar_panel(&mut self, ctx: &egui::Context) -> egui::Rect {
        egui::SidePanel::left("sidebar_nav")
            .resizable(false)
            .exact_width(self.sidebar_width)
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(theme::PANEL_MARGIN as i8, 12)),
            )
            .show(ctx, |ui| {
                self.sidebar.show(ui, &mut self.state, &mut self.toasts);
            })
            .response
            .rect
    }

    /// The management chat panel (dev/decisions.md chunk 3, native egui):
    /// attributed message bubbles (you / corpus / tool cards), a
    /// chronological activity tail in the log, and the footer row — model
    /// picker by the input, then the input + phase status. The picker is
    /// driven by corpus-core's `ollama_models()` (the GDK chat talks to
    /// Ollama DIRECTLY, never opencode's catalog); it lives in
    /// `chat::panel::ChatPanelView`.
    ///
    /// Width policy: default 360, min 280, max half the window — but
    /// APP-OWNED. The panel renders at `exact_width` and the custom drag
    /// divider is the only mover. egui's native resize (its persisted
    /// PanelState + content-width feedback) was the jitter / "slides
    /// across the window" / "doesn't hold" bug history; it's off, and the
    /// divider's anchored math can't accumulate error (see `drag_divider`).
    fn chat_panel(&mut self, ctx: &egui::Context) -> egui::Rect {
        egui::SidePanel::right("chat_panel_v2")
            .resizable(false)
            .exact_width(self.chat_width.max(280.0))
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                // Drain backend events into the view, then render. The model
                // picker is the panel's own footer widget (by the input).
                let events = self.chat_panel.absorb(&self.chat);
                // The chat mutated the store (a write tool succeeded):
                // re-read projects/agents/missions so the sidebar lists the
                // thing the chat just made.
                let mut corpus_touched = false;
                for ev in &events {
                    if let chat::ChatEvent::StoreMutated { area } = ev {
                        corpus_touched |= *area == "corpus";
                        self.state.refresh();
                    }
                }
                if corpus_touched {
                    if let Some(p) = self.state.effective_project() {
                        self.state.note_corpus_mutation(&p);
                        self.state.refresh_corpus_stats(&p);
                    }
                }
                // The header names the project, never a bare UUID.
                let label = self
                    .state
                    .effective_project()
                    .and_then(|slug| {
                        self.state
                            .projects
                            .iter()
                            .find(|(s, _)| s == &slug)
                            .map(|(_, p)| p.name.clone())
                    })
                    .unwrap_or_default();
                self.chat_panel.set_project_label(&label);
                self.ensure_chat_started(ui.ctx());
                // Juice the session with the operator's current position
                // (re-pushed only when it changes).
                let ctx = self.chat_context();
                if ctx != self.last_chat_context {
                    self.chat.set_context(&ctx);
                    self.last_chat_context = ctx;
                }
                self.chat_panel.show(ui, &mut self.chat);
                // Persist a picker change (store/app.yaml). Guarded by the
                // in-memory copy so the steady state is a comparison, not a
                // file read every frame. Saved on the PICK, not on session
                // start: a model chosen with no project selected (nothing to
                // scope a session to) must still come back next launch.
                let picked = self.chat_panel.model();
                if picked != self.chat_model_saved {
                    self.chat_model_saved = picked.to_string();
                    self.state.remember_chat_model(picked);
                }
            })
            .response
            .rect
    }

    /// The role + model the current chat backend was launched with; a change
    /// in either (or a Finished backend) restarts the scoped session.
    fn ensure_chat_started(&mut self, ctx: &egui::Context) {
        if self.chat_panel.model().is_empty() {
            return; // no model -> panel stays idle (refuses to start)
        }
        let Some(project) = self.state.effective_project() else {
            return;
        };
        let model = self.chat_panel.model().to_string();
        let role = self.chat_panel.role();
        // Live = same project + role + model AND a backend that isn't dead.
        // (The old check treated ChatPhase::Finished as live — a failed
        // backend was never restarted — and ignored the model entirely, so
        // a picker change silently kept the old model.)
        let live = self.chat.project() == project
            && self.chat_role == role
            && self.chat_model == model
            && matches!(
                self.chat.phase(),
                chat::ChatPhase::Connecting | chat::ChatPhase::Ready
            );
        if live {
            return;
        }
        self.chat_role = role;
        self.chat_model = model.clone();
        self.chat = chat::ChatHandle::start_scoped_with_wake(
            &project,
            &model,
            role,
            Arc::new(ctx.clone()),
        );
    }

    /// Keep the app-owned panel widths inside their live bounds (the
    /// window can have resized since a drag set them).
    fn clamp_panel_widths(&mut self, ctx: &egui::Context) {
        let half = ctx.screen_rect().width() * 0.5;
        self.sidebar_width = self.sidebar_width.clamp(160.0, (half * 0.9).max(220.0));
        self.chat_width = self.chat_width.clamp(280.0, half.max(360.0));
    }

    /// The drag handle for one panel edge: a 9px strip centered on the
    /// panel edge (the 1px hairline plus 4px of grab on each side). Its
    /// drag is ANCHORED: at press we remember the width + pointer x, and
    /// each frame's width derives from THAT anchor and the live pointer —
    /// never from the previous frame's width. That's what kills the
    /// jitter/drift of the native egui resize (which integrated from a
    /// content-union PanelState rect that fed back on itself). While the
    /// drag is live we take over the cursor so the strip reads as one
    /// held thing even when the pointer strays off the 9px band.
    fn drag_divider(
        &mut self,
        ctx: &egui::Context,
        panel: Divider,
        panel_rect: egui::Rect,
    ) -> Option<f32> {
        let (min, max) = match panel {
            Divider::Sidebar => (160.0, (ctx.screen_rect().width() * 0.45).max(220.0)),
            Divider::Chat => (280.0, (ctx.screen_rect().width() * 0.5).max(360.0)),
        };
        let current = match panel {
            Divider::Sidebar => self.sidebar_width,
            Divider::Chat => self.chat_width,
        };
        let edge_x = match panel {
            Divider::Sidebar => panel_rect.max.x,
            Divider::Chat => panel_rect.min.x,
        };
        let draw = egui::Rect::from_center_size(
            egui::pos2(edge_x, panel_rect.center().y),
            egui::vec2(1.0, panel_rect.height()),
        );
        let grab = draw.expand2(egui::vec2(4.0, 0.0));
        // A throwaway background-layer Ui just for the handle: painting a
        // REAL widget (not reusing a panel's ui) means its Sense hit-test
        // and its cursor are independent of any widget the panels laid
        // out near the edge.
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new(("divider", panel)),
            egui::UiBuilder::new()
                .max_rect(ctx.screen_rect())
                .layer_id(egui::LayerId::background()),
        );
        let r = ui.allocate_rect(grab, egui::Sense::drag());
        // The strip stays "held" look+grabbed while THIS panel's drag is
        // live, even if the pointer strays off the 9px band.
        let held = matches!(self.divider_drag, Some(d) if d.target == panel);
        if r.drag_started() {
            if let Some(p) = r.interact_pointer_pos() {
                self.divider_drag = Some(DividerDrag {
                    target: panel,
                    start_width: current,
                    start_x: p.x,
                });
            }
        }
        if !ctx.input(|i| i.pointer.primary_down()) {
            self.divider_drag = None;
        }
        if held || r.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let stroke = if r.dragged() {
            egui::Stroke::new(1.0_f32, theme::TEXT)
        } else if r.hovered() || held {
            egui::Stroke::new(1.0_f32, theme::TEXT_MUTED)
        } else {
            egui::Stroke::new(1.0_f32, theme::HAIRLINE)
        };
        ui.painter().vline(edge_x, panel_rect.y_range(), stroke);
        // Only the drag updates the width — the panel never snaps on
        // hover or release.
        if r.dragged() {
            if let (Some(d), Some(p)) = (self.divider_drag, r.interact_pointer_pos()) {
                if d.target == panel {
                    return Some(dragged_width(panel, d, p.x, min, max));
                }
            }
        }
        None
    }

    /// The operator-position context juiced into chat turns: where the
    /// operator is in the app (project, screen, selected entities) so
    /// "this agent" / "this project" resolve without a clarifying round-trip.
    fn chat_context(&self) -> String {
        let project = self
            .state
            .effective_project()
            .unwrap_or_else(|| "none".into());
        let agent = self.state.selected_agent.as_deref().unwrap_or("none");
        let mission = self.state.selected_mission.as_deref().unwrap_or("none");
        format!(
            "project={project}; screen={:?}; selected_agent={agent}; selected_mission={mission}",
            self.state.current_screen
        )
    }
}

/// How an unasked-for run exit reads to the operator. Split from the
/// toast plumbing so the rule itself is testable: a crash must not be
/// mistakable for a session the operator closed.
fn exit_notice(exit: &state::RunExit) -> (egui_toast::ToastKind, String) {
    let who = exit
        .mission
        .as_deref()
        .map(|label| format!("mission {label}"))
        .unwrap_or_else(|| "the run".to_string());
    if exit.code == 0 {
        (egui_toast::ToastKind::Info, format!("{who} ended — session closed"))
    } else {
        (
            egui_toast::ToastKind::Error,
            format!("{who} exited with code {} — see the transcript", exit.code),
        )
    }
}

/// A padded child: Projects/Agents apply the view's 24/18 inset themselves
/// now that the central panel renders the mission (terminal) edge-to-edge.
fn padded<R>(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .inner_margin(egui::Margin::symmetric(24, 18))
        .show(ui, content)
        .inner
}

fn main() -> eframe::Result {
    // Process-wide goose env (stream timeout, input limit, telemetry) —
    // ONCE, before any goose call can lock Config::global(). No-op values
    // when the operator already set them.
    chat::init_goose_env();
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1440.0, 900.0])
        .with_title("corpus-app");
    // Application icon for the OS dock/taskbar (logo-icon.png), decoded at
    // startup before the window exists.
    let viewport = match app_icon() {
        Some(icon) => viewport.with_icon(icon),
        None => viewport,
    };
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "corpus-app",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(width: f32, x: f32) -> DividerDrag {
        DividerDrag { target: Divider::Sidebar, start_width: width, start_x: x }
    }

    #[test]
    fn a_crash_is_reported_as_an_error_and_names_the_mission() {
        let crashed = state::RunExit { mission: Some("recon".into()), code: 1 };
        let (kind, text) = exit_notice(&crashed);
        assert_eq!(kind, egui_toast::ToastKind::Error);
        assert!(text.contains("recon"), "names the mission: {text}");
        assert!(text.contains("code 1"), "names the code: {text}");

        // A session the operator closed is not an alarm.
        let closed = state::RunExit { mission: Some("recon".into()), code: 0 };
        let (kind, _) = exit_notice(&closed);
        assert_eq!(kind, egui_toast::ToastKind::Info);

        // A run with no mission behind it still gets reported.
        let orphan = state::RunExit { mission: None, code: 137 };
        let (kind, text) = exit_notice(&orphan);
        assert_eq!(kind, egui_toast::ToastKind::Error);
        assert!(text.contains("the run"), "{text}");
    }

    #[test]
    fn anchored_drag_tracks_the_pointer_and_clamps_at_both_ends() {
        let d = drag(200.0, 100.0);
        let chat = drag(300.0, 100.0);
        // Sidebar (left panel): pull right = widen.
        assert_eq!(dragged_width(Divider::Sidebar, d, 130.0, 160.0, 480.0), 230.0);
        // Chat (right panel): pull LEFT = widen (opposite sign).
        assert_eq!(dragged_width(Divider::Chat, chat, 70.0, 280.0, 520.0), 330.0);
        // Clamps hold while the pointer keeps travelling past them —
        // anchored (not integrated), so a long overrun never "sticks".
        assert_eq!(dragged_width(Divider::Sidebar, d, 5000.0, 160.0, 480.0), 480.0);
        assert_eq!(dragged_width(Divider::Chat, chat, -5000.0, 280.0, 520.0), 520.0);
        assert_eq!(dragged_width(Divider::Sidebar, d, -5000.0, 160.0, 480.0), 160.0);
        // ...and releasing the clamp returns the width to the pointer with
        // no accumulated error (the jitter-killer).
        assert_eq!(dragged_width(Divider::Sidebar, d, 110.0, 160.0, 480.0), 210.0);
        assert_eq!(dragged_width(Divider::Chat, chat, 110.0, 280.0, 520.0), 290.0);
    }
}

/// Decode `assets/logo-icon.png` into the RGBA [`egui::IconData`] eframe
/// expects for the OS dock icon. `None` if the asset is missing or
/// undecodable (the app still runs, just with the default icon).
fn app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../assets/logo-icon.png");
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}
