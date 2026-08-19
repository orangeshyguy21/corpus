//! Mission view: a native-feeling, edge-to-edge opencode TUI while a session
//! is attached, and a compact control surface while the mission is idle.
//! Live terminal space is never reduced by persistent app chrome.
//!
//! Show() = resolve selection -> aim the pane
//! (see the attach precedence) -> show the pane filling the rect, the
//! fallback transcript tail (piped no-tmux), a brief starting line, or
//! the idle state.

use std::time::Duration;

use egui::{RichText, Ui};
use egui_phosphor::regular as ph;
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::{AppState, RunPhase};
use crate::terminal::TerminalPane;
use crate::theme;
use crate::views::mission_actions::{self, Availability};

/// Widget state for the Mission view: the embedded terminal and the
/// tail-follow flag for the piped fallback. Run bookkeeping lives on
/// `AppState` and the mission records — the view holds no launch state.
pub struct MissionsView {
    pane: TerminalPane,
    /// Auto-follow the tail (piped-fallback runs only).
    follow: bool,
    rename_open: bool,
    rename_name: String,
}
impl Default for MissionsView {
    fn default() -> Self {
        Self {
            pane: TerminalPane::default(),
            follow: true,
            rename_open: false,
            rename_name: String::new(),
        }
    }
}

impl MissionsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(project) = state.effective_project() else {
            return;
        };
        // Ensure a concrete mission selection (sidebar picks; else first).
        let selection_stale = state
            .selected_mission
            .as_ref()
            .map(|m| !state.missions.iter().any(|(s, _)| s == m))
            .unwrap_or(true);
        if selection_stale {
            state.selected_mission = state.missions.first().map(|(s, _)| s.clone());
        }
        let Some(slug) = state.selected_mission.clone() else {
            return;
        };
        let Some((_, mission)) = state.missions.iter().find(|(s, _)| s == &slug).cloned() else {
            return;
        };

        let phase = state.latest_run_phase(&project, &slug);
        if matches!(phase, RunPhase::Preparing | RunPhase::Starting) {
            let rect = ui.max_rect();
            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(rect)
                    .layout(egui::Layout::top_down(egui::Align::Center)),
                |ui| {
                    ui.add_space((rect.height() * 0.4).max(24.0));
                    ui.label(
                        RichText::new("preparing mission…")
                            .size(13.0)
                            .color(theme::TEXT_MUTED),
                    );
                    ui.add_space(8.0);
                    if crate::theme::house_button(ui, "Cancel").clicked() {
                        state.cancel_preparation(&project, &slug);
                    }
                },
            );
            return;
        }

        // Attach ONLY to the selected mission's own session: the app-owned
        // live run when it is this mission's (so a just-restored run
        // attaches immediately, before the live_sessions poll catches up),
        // else the recorded tmux session if it is live on the server.
        let own_run = state.run_active() && state.run_belongs_to(&project, &slug);
        let attach_name = if own_run {
            state.live_run_session()
        } else {
            mission
                .session
                .clone()
                .filter(|name| state.live_sessions.iter().any(|l| l == name))
        };
        let target = attach_name
            .and_then(|name| AppState::session_attach_command(&name).map(|argv| (name, argv)));
        if let Err(error) = self.pane.sync_target(ui.ctx(), target, own_run) {
            toast(toasts, ToastKind::Error, error);
        }

        if self.pane.attached().is_some() {
            // The embedded opencode TUI, edge-to-edge.
            self.pane.show(ui);
        } else if own_run && state.live_run_session().is_some() {
            // A tmux run booting (or between frames): the pane attaches
            // the instant the session is up. Say so rather than flashing
            // the piped tail's checkbox.
            self.centered(ui, "restoring session…", theme::TEXT_FAINT);
        } else if own_run {
            // Piped fallback (no tmux): this mission's transcript tail.
            self.tail(ui, state);
        } else {
            self.idle(ui, state, toasts, &project, &slug, &mission, &phase);
        }
    }

    /// The idle control surface. It deliberately disappears completely once
    /// a TUI attaches, preserving the terminal-first mission view.
    fn idle(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        mission: &corpus_core::Mission,
        phase: &RunPhase,
    ) {
        let label = crate::state::mission_label(mission.name.as_deref(), slug);
        let resumable = mission.opencode_session.is_some();
        let actions = Availability::resolve(
            false,
            resumable,
            state.mission_run_inflight(project, slug),
            state.mission_environment_needs_cleanup(project, slug),
        );
        let rect = ui.max_rect();
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                ui.add_space((rect.height() * 0.36).max(24.0));
                ui.label(RichText::new(&label).size(15.0).color(theme::TEXT));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("agent={}", mission.agent))
                        .size(12.0)
                        .color(theme::TEXT_FAINT),
                );
                ui.add_space(8.0);
                match phase {
                    RunPhase::Failed { message, .. } => {
                        ui.label(RichText::new(message).size(11.0).color(theme::SIGNAL_RED));
                    }
                    _ if resumable => {
                        ui.label(
                            RichText::new("session stopped — resume it or start fresh")
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                    }
                    _ => {
                        ui.label(
                            RichText::new("ready to launch")
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                    }
                }
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if actions.retry_cleanup {
                        if theme::primary_button(ui, "Retry cleanup").clicked() {
                            mission_actions::stop(state, toasts, project, slug);
                        }
                    } else if actions.resume {
                        if theme::primary_button(ui, "Resume").clicked() {
                            mission_actions::resume(state, toasts, project, slug);
                        }
                        if theme::house_button(ui, "Launch fresh").clicked() {
                            mission_actions::launch(state, toasts, project, slug);
                        }
                    } else if ui
                        .add_enabled_ui(actions.launch, |ui| theme::primary_button(ui, "Launch"))
                        .inner
                        .clicked()
                    {
                        mission_actions::launch(state, toasts, project, slug);
                    }

                    egui::menu::menu_custom_button(
                        ui,
                        egui::Button::new(theme::icon_text(
                            ph::DOTS_THREE_VERTICAL,
                            17.0,
                            theme::TEXT_MUTED,
                        ))
                        .frame(false),
                        |ui| {
                            if ui.button("Rename…").clicked() {
                                self.rename_name = label.clone();
                                self.rename_open = true;
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(actions.delete, egui::Button::new("Delete"))
                                .clicked()
                            {
                                mission_actions::delete(state, toasts, project, slug);
                                ui.close_menu();
                            }
                        },
                    );
                });
            },
        );
        self.rename_window(ui, state, toasts, project, slug);
    }

    fn rename_window(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
    ) {
        if !self.rename_open {
            return;
        }
        let mut open = true;
        let mut renamed = false;
        egui::Window::new("Rename mission")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -120.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (the slug stays as the id)");
                let entry = ui.text_edit_singleline(&mut self.rename_name);
                ui.add_space(8.0);
                let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if theme::house_button(ui, "Rename").clicked() || submit {
                    match state.rename_mission(project, slug, &self.rename_name) {
                        Ok(()) => {
                            state.refresh_missions(project);
                            renamed = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        if !open || renamed {
            self.rename_open = false;
        }
    }

    /// One faint centered line in the pane rect — the transient states
    /// (restoring…) that aren't worth a full idle block.
    fn centered(&self, ui: &mut Ui, text: &str, color: egui::Color32) {
        let rect = ui.max_rect();
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::monospace(12.0),
            color,
        );
    }

    /// The piped no-tmux fallback transcript tail: full width, follow-tail
    /// default on. Lines were ANSI-stripped once at ingest.
    fn tail(&mut self, ui: &mut Ui, state: &mut AppState) {
        ui.checkbox(&mut self.follow, "follow tail");
        egui::ScrollArea::vertical()
            .id_salt("mission_transcript")
            .auto_shrink([false, false])
            .stick_to_bottom(self.follow)
            .show(ui, |ui| {
                for line in &state.run_lines {
                    if line.stderr {
                        ui.colored_label(theme::SIGNAL_RED, &line.text);
                    } else {
                        ui.monospace(&line.text);
                    }
                }
            });
    }
}

/// Add a timed toast to the overlay.
fn toast(toasts: &mut Toasts, kind: ToastKind, text: impl Into<String>) {
    toasts.add(
        Toast::new()
            .kind(kind)
            .text(text.into())
            .options(ToastOptions::default().duration(Duration::from_secs(4))),
    );
}
