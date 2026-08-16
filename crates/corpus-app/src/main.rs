//! corpus-app: the operator's window into the research team (egui).
//!
//! Mock-driven redesign per dev/app-flow-plan.md: chunk 0 landed the
//! shell — a design-token module (`theme`), a three-column scaffold
//! (left sidebar, flex main, collapsible right chat panel) replacing the
//! old `nav` button list, and a top-bar skeleton (wordmark, placeholder
//! per-source dropdowns, env dot, chat toggle). `Screen::Launch` is
//! deleted: `LaunchView` merges into the mission view at chunk 5 — its
//! machinery is intact, and the run view takes over the main column
//! while a run is live (no standalone screen). Chunk 1 fills the sidebar
//! with the scoped lists + `+` flows; chunk 2 wires the top-bar sources.
//!
//! House rules: corpus-core calls live behind `AppState` (state.rs); one
//! module per screen; no business logic in widgets.

mod chat;
mod fmt;
mod nav;
mod sidebar;
mod state;
mod terminal;
mod theme;
mod views;

use std::time::Duration;

use eframe::egui;
use egui_phosphor::regular as ph;

use crate::chat::Chat as _;
use crate::nav::Screen;
use crate::sidebar::Sidebar;
use crate::state::AppState;
use crate::views::{agents, missions, projects};

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
    /// Last operator-position context pushed to the chat backend (re-pushed
    /// only on change).
    last_chat_context: String,
    /// The chat panel's current width (drag-settable, clamped 280..=half). A
    /// panel width in app state, so the divider is sticky and never persisted
    /// as pathologically wide by egui's own memory.
    chat_width: f32,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let state = AppState::from_env();
        Self {
            last_screen: state.current_screen,
            sidebar: Sidebar::default(),
            projects: projects::ProjectsView::default(),
            agents: agents::AgentsView::default(),
            missions: missions::MissionsView::default(),
            toasts: egui_toast::Toasts::new(),
            chat: chat::ChatHandle::idle(""),
            chat_panel: chat::panel::ChatPanelView::default(),
            chat_role: chat::team::TeamRole::Operator,
            chat_model: String::new(),
            last_chat_context: String::new(),
            chat_width: 360.0,
            state,
        }
    }
}

impl eframe::App for App {
    /// Don't persist egui's GUI memory (window positions AND **panel widths**)
    /// to disk. Persisted `PanelState` was restoring the chat panel's once-dragged
    /// full width on every launch, so it "slid across the whole window" and
    /// resisted being dragged back. We reset to a sane default each start; any
    /// signed layout resets first frame.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Keep the selected project's scoped caches loaded (only hits disk
        // when the selection changed).
        self.state.ensure_selection();
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
        self.sidebar_panel(ctx);
        if self.state.chat_open {
            self.chat_panel(ctx);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(theme::BG))
            .show(ctx, |ui| {
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

        // Toast overlay (top-right of the whole window).
        self.toasts.show(ctx);

        // Keep the toast overlay and its timers animating between clicks.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

impl App {
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
        let segments: [(Option<String>, Screen); 3] = [
            (project, Screen::Projects),
            (self.state.selected_agent.clone(), Screen::Agents),
            (self.state.selected_mission.clone(), Screen::Missions),
        ];
        let mut first = true;
        for (label, screen) in segments {
            let Some(label) = label else { continue };
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
        for source in &revs {
            let selected = self
                .state
                .source_pins
                .get(&source.name)
                .cloned()
                .unwrap_or_else(|| source.default_rev().to_string());
            if let Some(rev) = crate::views::source_dropdown::source_dropdown(
                ui,
                &format!("top_source_{}", source.name),
                source,
                &selected,
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
            ui.weak("no source pins");
        }
    }

    /// The live env status for the selected project's plugin (spec §3): the
    /// plugin name in 13px TEXT beside an 8px filled dot — OK-green when the
    /// probe is ready, DANGER-red when not. The STATUS is inline, not hidden
    /// in a tooltip: a not-ready env appends a short reason (truncated probe
    /// notes) so a dead gateway is visible at a glance. A click forces a
    /// re-probe; hover shows the full probe notes. Rendered as flat text,
    /// NOT a pill button.
    fn env_dot(&mut self, ui: &mut egui::Ui) {
        let Some(project) = self.state.effective_project() else {
            return;
        };
        let (dot, label, notes) = match self.state.env_status(&project) {
            Some((name, true, _)) => (
                theme::OK,
                name,
                "environment ready — click to re-probe".to_string(),
            ),
            Some((name, false, notes)) => {
                // Short inline reason: the first probe-note clause, capped —
                // the full notes stay on hover.
                let short: String = notes.chars().take(48).collect();
                let short = short.trim_end_matches([' ', ',', ';', '—', '(']).to_string();
                (
                    theme::DANGER,
                    if short.is_empty() {
                        format!("{name} — not ready")
                    } else {
                        format!("{name} — {short}")
                    },
                    if notes.is_empty() {
                        "environment not ready — click to re-probe".to_string()
                    } else {
                        format!("not ready — {notes}")
                    },
                )
            }
            None => (
                theme::TEXT_MUTED,
                "probe…".to_string(),
                "no probe yet — click to probe".to_string(),
            ),
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
    /// chrome.
    fn sidebar_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar_nav")
            .resizable(true)
            .default_width(200.0)
            .min_width(160.0)
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(theme::PANEL_MARGIN as i8, 12)),
            )
            .show(ctx, |ui| {
                self.sidebar.show(ui, &mut self.state, &mut self.toasts);
            });
    }

    /// The management chat panel (dev/decisions.md chunk 3, native egui):
    /// attributed message bubbles (you / corpus / tool cards), a
    /// chronological activity tail in the log, and the footer row — model
    /// picker by the input, then the input + phase status. The picker is
    /// driven by corpus-core's `ollama_models()` (the GDK chat talks to
    /// Ollama DIRECTLY, never opencode's catalog); it lives in
    /// `chat::panel::ChatPanelView`.
    fn chat_panel(&mut self, ctx: &egui::Context) {
        // Width policy: default ~360px, min 280px. The max clamp keeps the panel
        // from ever being forced wider than half the window. `show` returns the
        // panel's actual width, which we clamp and feed back as the default so
        // dragging STICKS (and stays clamped) across frames. Disk-persistence of
        // panel width is off (persist_egui_memory=false) so a once-dragged-wide
        // panel cannot be restored on the next launch.
        let half = ctx.screen_rect().width() * 0.5;
        let max = half.max(360.0);
        let inner = egui::SidePanel::right("chat_panel_v2")
            .resizable(true)
            .default_width(self.chat_width)
            .min_width(280.0)
            .max_width(max)
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
                self.ensure_chat_started();
                // Juice the session with the operator's current position
                // (re-pushed only when it changes).
                let ctx = self.chat_context();
                if ctx != self.last_chat_context {
                    self.chat.set_context(&ctx);
                    self.last_chat_context = ctx;
                }
                self.chat_panel.show(ui, &mut self.chat);
            });
        // Persist the (clamped) dragged width so the divider sticks.
        self.chat_width = inner.response.rect.width().clamp(280.0, max);
    }

    /// The role + model the current chat backend was launched with; a change
    /// in either (or a Finished backend) restarts the scoped session.
    fn ensure_chat_started(&mut self) {
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
        self.chat = chat::ChatHandle::start_scoped(&project, &model, role);
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
