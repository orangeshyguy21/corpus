//! Mission view (mission-view-plan, "the defluff"): the embedded opencode
//! TUI — and NOTHING else. No header, no hairline, no buttons, no status
//! banner, no explainer text, no bottom pane row. The pane is rendered
//! raw against the available rect (the central panel carries zero margin;
//! frame the pane here for nothing, terminal only). Mission actions live
//! in the sidebar mission-row menu; a mission is created AND launched by
//! the Missions `+` in one click, landing at an empty opencode prompt.
//!
//! Show() = resolve selection -> consume `pending_launch` once -> auto-
//! restore a dead-but-resumable mission -> poll the run -> aim the pane
//! (see the attach precedence) -> show the pane filling the rect, the
//! fallback transcript tail (piped no-tmux), a brief restoring line, or
//! the idle state.
//!
//! Auto-restore: selecting a mission whose tmux session has died silently
//! re-opens its opencode conversation (`opencode --session <id>`) — no
//! button, no prompt. It fires once per selection, only when nothing else
//! is running (a live run is never torn down to restore a dead one) and
//! only when the mission actually recorded a session to return to.

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
    /// The mission auto-restore has already been attempted for, so a
    /// failed restore (or one the operator then stopped) doesn't respawn
    /// every frame. Cleared by selecting a different mission.
    restored: Option<String>,
}

impl Default for MissionsView {
    fn default() -> Self {
        Self {
            pane: TerminalPane::default(),
            follow: true,
            restored: None,
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

        // A just-created mission launches automatically (once): a BARE TUI
        // at an empty prompt. pending_launch is consumed even on failure.
        // launch_mission BACKGROUNDS a live run rather than replacing it,
        // so creating a mission never disturbs the one already working.
        if state.pending_launch.as_deref() == Some(slug.as_str()) {
            state.pending_launch = None;
            if let Err(error) = state.launch_mission(&project, &mission.agent, &slug) {
                toast(toasts, ToastKind::Error, error.to_string());
            }
        }

        // Auto-restore: landing on a mission whose session has died just
        // brings it back — no button. Fire ONCE per selection (the guard),
        // and only when nothing else is running (a live run is never torn
        // down for this) and the mission actually recorded a conversation
        // to return to. A mission that is already live falls through to
        // the attach below untouched.
        let selection_changed = self.restored.as_deref() != Some(slug.as_str());
        if selection_changed {
            self.restored = Some(slug.clone());
            let session_live = mission.session.as_deref().is_some_and(|name| {
                state.live_sessions.iter().any(|l| l == name)
                    || state.live_run_session().as_deref() == Some(name)
            });
            if !session_live
                && !state.run_active()
                && mission.opencode_session.is_some()
            {
                if let Err(error) = state.resume_mission(&project, &slug) {
                    toast(toasts, ToastKind::Error, error.to_string());
                }
            }
        }

        // Drain whatever the session produced since the last frame.
        state.poll_run();

        // Attach ONLY to the selected mission's own session: the app-owned
        // live run when it is this mission's (so a just-restored run
        // attaches immediately, before the live_sessions poll catches up),
        // else the recorded tmux session if it is live on the server.
        let own_run =
            state.run_active() && state.run_mission.as_deref() == Some(slug.as_str());
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

        ui.ctx().request_repaint_after(Duration::from_millis(2500));
    }

    /// The idle state for a mission with no session to attach and none to
    /// restore: its name + a faint reason. No actions — a mission is
    /// launched from the sidebar, and a resumable one restores itself.
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
    /// default on, ANSI stripped.
    fn tail(&mut self, ui: &mut Ui, state: &mut AppState) {
        ui.checkbox(&mut self.follow, "follow tail");
        egui::ScrollArea::vertical()
            .id_salt("mission_transcript")
            .auto_shrink([false, false])
            .stick_to_bottom(self.follow)
            .show(ui, |ui| {
                for line in &state.run_lines {
                    let text = strip_ansi(&line.text);
                    if line.stderr {
                        ui.colored_label(theme::DANGER, text);
                    } else {
                        ui.monospace(text);
                    }
                }
            });
    }
}

/// Strip ANSI escape sequences (opencode streams colorized output).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    let b = c as u8;
                    if (0x40..=0x7E).contains(&b) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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