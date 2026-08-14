//! Mission view (mission-view-plan, "the defluff"): the embedded opencode
//! TUI — and NOTHING else. No header, no hairline, no buttons, no status
//! banner, no explainer text, no bottom pane row. The pane is rendered
//! raw against the available rect (the central panel carries zero margin;
//! frame the pane here for nothing, terminal only). Mission actions live
//! in the sidebar mission-row menu; a mission is created AND launched by
//! the Missions `+` in one click, landing at an empty opencode prompt.
//!
//! Show() = resolve selection -> consume `pending_launch` once -> poll the
//! run -> aim the pane (see the attach precedence) -> show the pane
//! filling the rect, or the fallback transcript tail (piped no-tmux), or a
//! single faint centered line when idle.

use std::time::Duration;

use egui::{Align2, Ui};
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

        // A just-created mission launches automatically (once): a BARE TUI
        // at an empty prompt. pending_launch is consumed even on failure.
        if state.pending_launch.as_deref() == Some(slug.as_str()) {
            state.pending_launch = None;
            if !state.run_active() {
                if let Err(error) = state.launch_mission(&project, &mission.agent, &slug) {
                    toast(toasts, ToastKind::Error, error.to_string());
                }
            }
        }

        // Drain whatever the session produced since the last frame.
        state.poll_run();

        // Attach precedence (state.rs): (1) the mission's recorded session
        // when it's live on the tmux server, (2) the app-owned live run,
        // (3) idle.
        let target = if mission
            .session
            .as_ref()
            .is_some_and(|s| state.live_sessions.contains(s))
        {
            let name = mission.session.clone().expect("checked above");
            AppState::session_attach_command(&name).map(|argv| (name, argv))
        } else if let Some(argv) = state.live_pty_attach() {
            let name = AppState::pty_attach_session(&argv).unwrap_or_default();
            Some((name, argv))
        } else {
            None
        };
        if let Err(error) = self.pane.sync_target(ui.ctx(), target) {
            toast(toasts, ToastKind::Error, error);
        }

        if self.pane.attached().is_some() {
            // The embedded opencode TUI, edge-to-edge.
            self.pane.show(ui);
        } else if state.run_active() {
            // Piped fallback (no tmux): the bare transcript tail.
            self.tail(ui, state);
        } else if let Some(path) = &state.export_path {
            // Idle with a known transcript: one faint centered line.
            let rect = ui.max_rect();
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                format!("no live session — transcript: {path}"),
                egui::FontId::monospace(12.0),
                theme::TEXT_FAINT,
            );
        }
        // else: nothing — an empty central column.

        ui.ctx().request_repaint_after(Duration::from_millis(2500));
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