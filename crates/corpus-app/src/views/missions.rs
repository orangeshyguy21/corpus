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
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::{AppState, RunPhase};
use crate::terminal::TerminalPane;
use crate::theme;
use crate::views::mission_actions;

/// Widget state for the Mission view: the embedded terminal and the
/// tail-follow flag for the piped fallback. Run bookkeeping lives on
/// `AppState` and the mission records — the view holds no launch state.
pub struct MissionsView {
    pane: TerminalPane,
    /// Auto-follow the tail (piped-fallback runs only).
    follow: bool,
}
impl Default for MissionsView {
    fn default() -> Self {
        Self {
            pane: TerminalPane::default(),
            follow: true,
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
            self.idle(ui, state, toasts, &project, &slug, &mission);
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
    ) {
        let label = crate::state::mission_label(mission.name.as_deref(), slug);
        let agent_line = state
            .agents
            .iter()
            .find(|(agent_slug, _)| agent_slug == &mission.agent)
            .map(|(agent_slug, agent)| {
                format_agent_line(
                    &crate::state::agent_label(&agent.meta.name, agent_slug),
                    agent.meta.role().as_str(),
                )
            })
            .unwrap_or_else(|| format_agent_line(&state.agent_label(&mission.agent), "unknown"));
        let delete_available = state.mission_delete_available(project, slug);
        let rect = ui.max_rect();
        let splash_rect = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width(), 88.0_f32.min(rect.height())),
        );
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(splash_rect)
                .layout(egui::Layout::top_down(egui::Align::Center)),
            |ui| {
                ui.label(RichText::new(&label).size(15.0).color(theme::TEXT));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(agent_line)
                        .size(12.0)
                        .color(theme::TEXT_MUTED),
                );
                ui.add_space(12.0);

                // A horizontal layout greedily takes the entire available
                // width in egui. Give the action row an explicit compact
                // rectangle so the parent can center it as one unit.
                ui.allocate_ui_with_layout(
                    egui::vec2(184.0, 38.0),
                    egui::Layout::left_to_right(egui::Align::Center)
                        .with_main_align(egui::Align::Center),
                    |ui| {
                        let resumable = mission.opencode_session.is_some();
                        let primary_available = !state.mission_run_inflight(project, slug)
                            && !state.mission_environment_needs_cleanup(project, slug);
                        if ui
                            .add_enabled_ui(primary_available, |ui| {
                                theme::primary_button(
                                    ui,
                                    if resumable { "Resume" } else { "Launch" },
                                )
                            })
                            .inner
                            .clicked()
                        {
                            if resumable {
                                state.select_mission(project, slug);
                                match state.resume_mission(project, slug) {
                                    Ok(()) => toast(toasts, ToastKind::Info, "mission resumed"),
                                    Err(error) => {
                                        toast(toasts, ToastKind::Error, error.to_string())
                                    }
                                }
                            } else {
                                mission_actions::launch(state, toasts, project, slug);
                            }
                        }
                        if ui
                            .add_enabled_ui(delete_available, |ui| {
                                theme::destructive_button(ui, "Delete")
                            })
                            .inner
                            .clicked()
                        {
                            mission_actions::delete(state, toasts, project, slug);
                        }
                    },
                );
            },
        );
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

fn format_agent_line(name: &str, role: &str) -> String {
    format!("{name} <{role}>")
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

#[cfg(test)]
mod tests {
    use super::format_agent_line;

    #[test]
    fn splash_identifies_agent_by_name_and_role() {
        assert_eq!(format_agent_line("operator", "tester"), "operator <tester>");
    }
}
