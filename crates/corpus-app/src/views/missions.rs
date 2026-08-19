//! Mission view (mission-view-plan, "the defluff"): the embedded opencode
//! TUI — and NOTHING else. No header, no hairline, no buttons, no status
//! banner, no explainer text, no bottom pane row. The pane is rendered
//! raw against the available rect (the central panel carries zero margin;
//! frame the pane here for nothing, terminal only). Mission actions live
//! in the sidebar mission-row menu; a mission is created AND launched by
//! the Missions `+` in one click, landing at an empty opencode prompt.
//!
//! Show() = resolve selection -> aim the pane
//! (see the attach precedence) -> show the pane filling the rect, the
//! fallback transcript tail (piped no-tmux), a brief starting line, or
//! the idle state.

use std::time::Duration;

use egui::{RichText, Ui};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::AppState;
use crate::terminal::TerminalPane;
use crate::theme;

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

        if matches!(
            state.latest_run_phase(&project, &slug),
            crate::state::RunPhase::Preparing | crate::state::RunPhase::Starting
        ) {
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
        if let Err(error) = self.pane.sync_target(ui.ctx(), target) {
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
            // Nothing to attach and nothing to restore: a mission that
            // never ran (or ran before ids were kept). One quiet line —
            // creating a run is the sidebar `+`'s job.
            self.idle(ui, &slug, &mission);
        }
    }

    /// The idle state for a mission with no session to attach and none to
    /// restore: its name + a faint reason. Actions live in the sidebar
    /// mission menu; browsing this view never launches a process.
    fn idle(&mut self, ui: &mut Ui, slug: &str, mission: &corpus_core::Mission) {
        let label = crate::state::mission_label(mission.name.as_deref(), slug);
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
                ui.add_space(10.0);
                ui.label(
                    RichText::new("no session yet — launch this mission from the sidebar")
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
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

/// Add a timed toast to the overlay.
fn toast(toasts: &mut Toasts, kind: ToastKind, text: impl Into<String>) {
    toasts.add(
        Toast::new()
            .kind(kind)
            .text(text.into())
            .options(ToastOptions::default().duration(Duration::from_secs(4))),
    );
}
