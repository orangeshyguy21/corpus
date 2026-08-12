//! corpus-deck: the operator's window into the research team (egui).
//!
//! Rebuilt ground-up per dev/deck-flow-plan.md: chunk 0 struck the M0
//! views to a shell (window, left nav with one entry per planned screen
//! — greyed until its chunk lands, central panel, toast overlay);
//! chunks 1–4 landed Projects, Teams, and the Agent template editors;
//! chunk 5 landed the launch seam (materialize a team's agents, spawn a
//! run on the team scope); chunk 7 embedded the terminal (egui_term —
//! the run pane is a tmux client, the external-terminal popup is gone).
//! House rules: corpus-core calls live behind `DeckState` (state.rs);
//! one module per screen; no business logic in widgets.

mod nav;
mod state;
mod terminal;
mod views;

use std::time::Duration;

use eframe::egui;
use egui_toast::Toasts;

use crate::nav::Screen;
use crate::state::DeckState;
use crate::views::{agents, launch, projects, teams};

/// corpus-deck application state: the deck's state layer, the active
/// screen, per-screen widget state, and the toast overlay.
struct App {
    state: DeckState,
    screen: Screen,
    /// The screen shown last frame — screen-change hooks (fresh project
    /// list before Teams/Agents render) fire on transitions only.
    last_screen: Screen,
    /// A screen change a view requested (Teams → Launch); applied after
    /// the central panel.
    pending_nav: Option<Screen>,
    projects: projects::ProjectsView,
    teams: teams::TeamsView,
    agents: agents::AgentsView,
    launch: launch::LaunchView,
    toasts: Toasts,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_style(&cc.egui_ctx);
        let screen = Screen::Projects;
        Self {
            state: DeckState::from_env(),
            screen,
            last_screen: screen,
            pending_nav: None,
            projects: projects::ProjectsView::default(),
            teams: teams::TeamsView::default(),
            agents: agents::AgentsView::default(),
            launch: launch::LaunchView::default(),
            toasts: Toasts::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Background fetch results land here (never block the UI thread).
        self.state.poll_models();
        // Screen-change hooks.
        if self.screen != self.last_screen {
            self.last_screen = self.screen;
            if self.screen == Screen::Teams || self.screen == Screen::Agents {
                self.state.refresh();
            }
            if self.screen == Screen::Launch {
                self.state.refresh_live_sessions();
            }
        }
        egui::TopBottomPanel::top("title_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new("corpus-deck").strong());
                ui.separator();
                ui.weak("the operator's window into the research team");
            });
            ui.add_space(2.0);
        });

        egui::SidePanel::left("nav")
            .resizable(false)
            .default_width(150.0)
            .show(ctx, |ui| {
                ui.add_space(12.0);
                for screen in Screen::ALL {
                    let selected = self.screen == screen;
                    let button = egui::Button::new(egui::RichText::new(screen.label()).size(16.0))
                        .selected(selected)
                        .min_size(egui::vec2(ui.available_width() - 8.0, 30.0));
                    let response = ui.add(button);
                    if response.clicked() {
                        self.screen = screen;
                    }
                    response.on_hover_text(screen.note());
                    ui.add_space(4.0);
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.screen {
            Screen::Projects => {
                self.projects.show(ui, &mut self.state, &mut self.toasts);
            }
            Screen::Teams => {
                self.teams.show(
                    ui,
                    &mut self.state,
                    &mut self.toasts,
                    &mut self.pending_nav,
                );
            }
            Screen::Agents => {
                self.agents.show(ui, &mut self.state, &mut self.toasts);
            }
            Screen::Launch => {
                self.launch.show(ui, &mut self.state, &mut self.toasts);
            }
        });

        // Apply a screen change a view requested (Teams -> Launch).
        if let Some(screen) = self.pending_nav.take() {
            self.screen = screen;
            self.last_screen = screen; // the transition hook already ran
        }

        // Toast overlay (top-right of the whole window).
        self.toasts.show(ctx);

        // Keep the toast overlay and its timers animating between clicks.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

/// Dark theme + readable type.
fn configure_style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(24, 25, 30);
    visuals.window_fill = egui::Color32::from_rgb(24, 25, 30);
    ctx.set_visuals(visuals);
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.text_styles.insert(
        egui::TextStyle::Body,
        egui::FontId::proportional(16.0),
    );
    style.text_styles.insert(
        egui::TextStyle::Heading,
        egui::FontId::proportional(24.0),
    );
    ctx.set_style(style);
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_title("corpus-deck"),
        ..Default::default()
    };
    eframe::run_native(
        "corpus-deck",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}