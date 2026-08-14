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
            state,
        }
    }
}

impl eframe::App for App {
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
                    let total_w = ui.available_width();
                    // LEFT: the wordmark, height 20px, aspect maintained
                    // (source 1072×325 -> ~66px wide).
                    ui.add(
                        egui::Image::new(egui::include_image!("../assets/logo.png"))
                            .fit_to_exact_size(egui::vec2(66.0, 20.0)),
                    );
                    // Roughly centre the source dropdowns between the logo
                    // and the right zone.
                    let dropdown_count = self.state.source_revs.len().max(1);
                    let dropdown_w = (dropdown_count as f32) * 150.0;
                    let right_zone = 200.0;
                    let logo_w = 66.0 + theme::SPACING;
                    let spacer = ((total_w - logo_w - dropdown_w - right_zone) / 2.0).max(0.0);
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

    /// The per-source `repo: rev` dropdowns (spec §3): flat PANEL fields,
    /// the `repo: rev` text in MONOSPACE 13px + a caret_down arrow. Options
    /// come from the selected project's plugin (`source_revs`), the
    /// selection lives in `source_pins` (stamped into missions at
    /// creation). Declaration order is preserved (`source_revs` is a Vec).
    fn source_dropdowns(&mut self, ui: &mut egui::Ui) {
        let revs = self.state.source_revs.clone();
        for source in &revs {
            let selected = self
                .state
                .source_pins
                .get(&source.name)
                .cloned()
                .unwrap_or_else(|| source.pinned.clone());
            theme::combo_field(ui, |ui| {
                egui::ComboBox::from_id_salt(format!("top_source_{}", source.name))
                    .icon(theme::combo_caret)
                    .selected_text(
                        egui::RichText::new(format!("{}: {}", source.name, selected))
                            .monospace()
                            .size(13.0)
                            .color(theme::TEXT),
                    )
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        for rev in &source.revs {
                            if ui
                                .selectable_label(
                                    rev == &selected,
                                    egui::RichText::new(rev.clone()).monospace(),
                                )
                                .clicked()
                            {
                                self.state
                                    .source_pins
                                    .insert(source.name.clone(), rev.clone());
                            }
                        }
                    });
            });
        }
        if revs.is_empty() {
            ui.weak("no source pins");
        }
    }

    /// The live env status for the selected project's plugin (spec §3): the
    /// plugin name in 13px TEXT beside an 8px filled dot — OK-green when the
    /// probe is ready, DANGER-red when not. A click forces a re-probe; hover
    /// shows the probe notes. Rendered as flat text, NOT a pill button.
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
            Some((name, false, notes)) => (
                theme::DANGER,
                name,
                if notes.is_empty() {
                    "environment not ready — click to re-probe".to_string()
                } else {
                    format!("not ready — {notes}")
                },
            ),
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

    /// The reserved (chunk 0, empty) chat panel: a header + stretchable
    /// filler + a disabled, rounded input box. Content is UNDEFINED until
    /// the chunk-6 definition conversation.
    fn chat_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("chat_panel")
            .resizable(true)
            .default_width(260.0)
            .min_width(200.0)
            .frame(
                egui::Frame::default()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(12, 12)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Chat");
                    ui.add_space(4.0);
                    ui.colored_label(theme::TEXT_FAINT, "content pending definition");
                });
                ui.separator();
                ui.add_space(10.0);
                // Stretch filler; the input box sits at the bottom.
                ui.add_space((ui.available_height() - 44.0).max(0.0));
                ui.add_enabled(
                    false,
                    egui::TextEdit::singleline(&mut self.state.chat_draft)
                        .hint_text("message…")
                        .desired_width(f32::INFINITY),
                );
            });
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
