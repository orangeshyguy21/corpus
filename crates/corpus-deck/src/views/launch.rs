//! Launch screen (deck-flow chunks 5 + 7): the run view — a run header,
//! the embedded terminal pane (`tmux attach -t <session>` in-pane; the
//! external-terminal popup is GONE), and abort/dismiss chrome. tmux
//! stays the supervisor: the pane is just a client, so deck close/crash
//! never kills a run, and a relaunched deck offers live corpus sessions
//! for re-attach. A run on the piped fallback (no tmux) has no pane —
//! it keeps the chunk-5 transcript tail. No replay, oracle panes, or
//! docking: those are the M1 run-dashboard follow-up. Launches are
//! initiated from the Teams screen (Launch… on a team); this screen
//! only watches and tears down.

use egui::{RichText, Ui};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use std::time::Duration;

use crate::state::{DeckState, RunStatus};
use crate::terminal::TerminalPane;

/// Widget state for the run view.
#[derive(Default)]
pub struct LaunchView {
    /// The embedded terminal (chunk 7): attached to the live run's tmux
    /// session, or to an orphan session the operator re-attached.
    pane: TerminalPane,
    /// The orphan session chosen from the re-attach list (a run that
    /// outlived a previous deck process).
    reattached: Option<String>,
    /// Auto-follow the tail (piped-fallback runs only).
    follow: bool,
}

impl LaunchView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts) {
        // Drain whatever the session produced since the last frame (the
        // pipe-pane capture keeps feeding the transcript machinery even
        // while the pane is the live view).
        state.poll_run();

        ui.horizontal(|ui| {
            ui.heading("Launch");
            if let Some(meta) = &state.run_meta {
                ui.separator();
                ui.weak(format!("{} · {} · agent {}", meta.project, meta.team, meta.agent));
                ui.separator();
                ui.weak(&meta.transcript);
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let running = state.run_active();
            let abort = ui
                .add_enabled(running, egui::Button::new("Abort run"))
                .on_disabled_hover_text("no run is active");
            if abort.clicked() {
                state.abort_run();
            }
            let dismiss = ui
                .add_enabled(running, egui::Button::new("Dismiss run"))
                .on_disabled_hover_text("a run must be live to dismiss");
            if dismiss.clicked() {
                if let Err(error) = state.dismiss_run() {
                    toast(toasts, ToastKind::Error, error.to_string());
                }
            }
            let cleared = ui
                .add_enabled(!running && state.run_meta.is_some(), egui::Button::new("Clear"))
                .on_disabled_hover_text("a launch is still in flight");
            if cleared.clicked() {
                state.clear_run();
                self.reattached = None;
            }
        });
        ui.add_space(8.0);

        let (running, status) = (state.run_active(), state.run_status);
        if let Some(status) = status {
            let (color, text) = match status {
                RunStatus::Aborted => (
                    egui::Color32::from_rgb(255, 180, 90),
                    "aborted by operator".to_string(),
                ),
                RunStatus::Dismissed => (
                    egui::Color32::from_rgb(120, 200, 120),
                    "dismissed — transcript exported".to_string(),
                ),
                RunStatus::Exited(0) => (
                    egui::Color32::from_rgb(120, 200, 120),
                    "exited 0".to_string(),
                ),
                RunStatus::Exited(code) => (
                    egui::Color32::from_rgb(255, 120, 90),
                    format!("exited {code}"),
                ),
            };
            ui.colored_label(color, text);
            if let Some(path) = &state.export_path {
                ui.weak(format!("transcript: {path}"));
            }
            ui.add_space(4.0);
        } else if running {
            ui.weak("run in flight — the pane is a tmux client on the run; deck close never kills it");
            ui.add_space(4.0);
        }

        // Aim the pane: the live run's session wins; otherwise the
        // operator's re-attach pick (a run that outlived the deck).
        let target = if let Some(argv) = state.live_pty_attach() {
            self.reattached = None;
            let session = DeckState::pty_attach_session(&argv).unwrap_or_default();
            Some((session, argv))
        } else if let Some(session) = self.reattached.clone() {
            DeckState::session_attach_command(&session).map(|argv| (session, argv))
        } else {
            None
        };
        if let Err(error) = self.pane.sync_target(ui.ctx(), target) {
            toast(toasts, ToastKind::Error, error);
        }
        // A re-attached orphan that ended (TUI quit, session killed)
        // detaches the pane — drop the pick so we don't re-attach in a
        // loop, and refresh the survivors.
        if self.reattached.is_some() && self.pane.attached().is_none() {
            self.reattached = None;
            state.refresh_live_sessions();
        }

        if self.pane.attached().is_some() {
            ui.weak(
                "terminal: click the pane to type (ctrl chords and tab go to the run) · \
                 click anywhere outside the pane to release focus · keys reach the run \
                 while the pointer is over the pane · drag to select, ⌘C copies",
            );
            ui.add_space(4.0);
            self.pane.show(ui);
            return;
        }

        if running {
            // The piped fallback (no tmux): the chunk-5 transcript tail.
            ui.checkbox(&mut self.follow, "follow tail");
            egui::ScrollArea::vertical()
                .id_salt("run_transcript")
                .auto_shrink([false, false])
                .stick_to_bottom(self.follow)
                .show(ui, |ui| {
                    for line in &state.run_lines {
                        let text = strip_ansi(&line.text);
                        if line.stderr {
                            ui.colored_label(egui::Color32::from_rgb(255, 200, 160), text);
                        } else {
                            ui.monospace(text);
                        }
                    }
                });
            return;
        }

        if state.run_meta.is_none() {
            self.orphans(ui, state, toasts);
        }
    }

    /// No deck-owned run: offer the live corpus tmux sessions for
    /// in-pane re-attach (a run survives the deck by design).
    fn orphans(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts) {
        ui.add_space(24.0);
        ui.label(
            RichText::new("No run active — open Teams and hit Launch… on a team.")
                .weak()
                .size(17.0),
        );
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Live runs from before this deck session:").weak());
            if ui.button("Refresh").clicked() {
                state.refresh_live_sessions();
            }
        });
        if state.live_sessions.is_empty() {
            ui.weak("none found on the tmux server");
            return;
        }
        ui.add_space(4.0);
        let sessions = state.live_sessions.clone();
        for session in sessions {
            ui.horizontal(|ui| {
                ui.monospace(&session);
                if ui.button("Attach in pane").clicked() {
                    if DeckState::session_attach_command(&session).is_some() {
                        self.reattached = Some(session.clone());
                    } else {
                        toast(toasts, ToastKind::Error, "tmux is not available");
                    }
                }
            });
        }
        ui.add_space(4.0);
        ui.weak(
            "a re-attached run is steered and ended inside its TUI (quitting opencode \
             ends the session); abort/dismiss chrome only covers runs this deck launched",
        );
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
