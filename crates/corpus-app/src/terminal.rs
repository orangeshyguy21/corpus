//! TerminalPane (app-flow chunk 7): the embedded terminal wrapping
//! `egui_term` (alacritty_terminal PTY backend). The pane NEVER spawns
//! opencode — it runs `tmux attach -t <run session>` inside the
//! embedded PTY, so tmux stays the supervisor: closing or crashing the
//! app closes the PTY, the tmux CLIENT detaches, and the run lives on.
//!
//! Focus discipline: click the pane to focus it (keys — including the
//! ctrl chords the opencode TUI uses, and tab — route to the PTY); the
//! release-focus gesture is a click ANYWHERE OUTSIDE the pane, which is
//! documented in the run header. egui_term 0.1.0 additionally gates
//! keyboard input on pointer hover, so keys reach the run while the
//! pointer is over the pane. Scrollback (mouse wheel), selection
//! (drag; double/triple click for word/line), and copy/paste
//! (Cmd+C / Cmd+V) are egui_term's own.

use std::sync::mpsc::{channel, Receiver, Sender};

use egui_term::{
    BackendSettings, ColorPalette, FontSettings, PtyEvent, TerminalBackend, TerminalFont,
    TerminalTheme, TerminalView,
};

/// An embedded terminal pane attached to one tmux session at a time.
/// Dropping or replacing the backend closes the PTY — the tmux client
/// inside detaches, leaving the session (the run) untouched.
pub struct TerminalPane {
    backend: Option<TerminalBackend>,
    pty_tx: Sender<(u64, PtyEvent)>,
    pty_rx: Receiver<(u64, PtyEvent)>,
    /// The tmux session the pane is attached to (None = detached).
    attached: Option<String>,
    /// Click-to-focus state; released by a click outside the pane.
    focused: bool,
    /// Font matched to the app theme, resolved on first show.
    font: Option<TerminalFont>,
    /// Colors matched to the app theme (panel fill + readable fg).
    theme: TerminalTheme,
    /// Backend ids must be unique per attach (widget state is keyed).
    next_id: u64,
}

impl Default for TerminalPane {
    fn default() -> Self {
        let (pty_tx, pty_rx) = channel();
        Self {
            backend: None,
            pty_tx,
            pty_rx,
            attached: None,
            focused: false,
            font: None,
            theme: pane_theme(),
            next_id: 1,
        }
    }
}

impl TerminalPane {
    /// The session the pane is attached to, if any.
    pub fn attached(&self) -> Option<&str> {
        self.attached.as_deref()
    }

    /// Keep the pane aimed at `target`: attach when the target session
    /// changed (run switching — the old tmux client detaches with its
    /// PTY, no zombie clients), detach when there is no target.
    pub fn sync_target(
        &mut self,
        ctx: &egui::Context,
        target: Option<(String, Vec<String>)>,
    ) -> Result<(), String> {
        match target {
            Some((session, _)) if self.attached.as_deref() == Some(session.as_str()) => Ok(()),
            Some((session, argv)) => self.attach(ctx, &session, &argv),
            None => {
                self.detach();
                Ok(())
            }
        }
    }

    /// Detach the current tmux client (PTY close) without touching the
    /// session — the run survives by design.
    pub fn detach(&mut self) {
        self.backend = None; // Drop shuts the event loop; the PTY close detaches the client
        self.attached = None;
        self.focused = false;
    }

    /// Spawn the embedded PTY running the attach argv. The child is
    /// wrapped in `env TERM=… COLORTERM=…` because a GUI-launched app
    /// carries no TERM (tmux refuses to attach without one) and tmux
    /// 3.2+ passes RGB through for clients advertising truecolor.
    fn attach(&mut self, ctx: &egui::Context, session: &str, argv: &[String]) -> Result<(), String> {
        self.detach();
        let Some((program, args)) = argv.split_first() else {
            return Err("empty attach command".to_string());
        };
        let settings = BackendSettings {
            shell: "/bin/sh".to_string(),
            args: std::iter::once("-c".to_string())
                .chain(std::iter::once(
                    "exec env TERM=xterm-256color COLORTERM=truecolor \"$@\"".to_string(),
                ))
                .chain(std::iter::once("corpus-term".to_string()))
                .chain(std::iter::once(program.clone()))
                .chain(args.iter().cloned())
                .collect(),
            working_directory: None,
        };
        let id = self.next_id;
        match TerminalBackend::new(id, ctx.clone(), self.pty_tx.clone(), settings) {
            Ok(backend) => {
                self.backend = Some(backend);
                self.attached = Some(session.to_string());
                self.next_id += 1;
                Ok(())
            }
            Err(error) => Err(format!("embedded terminal failed to spawn: {error}")),
        }
    }

    /// Render the pane filling the available space. PTY resize is wired
    /// by the widget itself (it re-sends Resize from the layout rect
    /// every frame); PTY output drives repaints via the backend.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        self.drain_events();
        let font = self
            .font
            .get_or_insert_with(|| pane_font(ui.ctx()))
            .clone();
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        let view = TerminalView::new(ui, backend)
            .set_theme(self.theme.clone())
            .set_font(font)
            .set_focus(self.focused);
        let response = ui.add(view);
        // Focus discipline: click-to-focus; the release gesture is a
        // click anywhere outside the pane (documented in the header).
        if response.clicked() {
            self.focused = true;
        }
        let clicked_outside = ui.ctx().input(|i| {
            i.pointer.any_click()
                && i.pointer
                    .interact_pos()
                    .map(|pos| !response.rect.contains(pos))
                    .unwrap_or(false)
        });
        if clicked_outside {
            self.focused = false;
        }
    }

    /// Drain PTY events (keeps the backend's subscription channel
    /// empty); a client exit (session killed, TUI quit) detaches the
    /// pane — it must never be re-attached implicitly.
    fn drain_events(&mut self) {
        let mut exited = false;
        while let Ok((_, event)) = self.pty_rx.try_recv() {
            if matches!(event, PtyEvent::Exit) {
                exited = true;
            }
        }
        if exited {
            self.detach();
        }
    }
}

/// Colors matched to the app theme: the run pane sits on the app's
/// panel fill with the stock readable palette otherwise.
fn pane_theme() -> TerminalTheme {
    let palette = ColorPalette {
        background: "#18191e".to_string(), // the app's panel_fill (24,25,30)
        black: "#18191e".to_string(),
        foreground: "#d8d8d8".to_string(),
        ..Default::default()
    };
    TerminalTheme::new(Box::new(palette))
}

/// The app's own monospace text style as the terminal font.
fn pane_font(ctx: &egui::Context) -> TerminalFont {
    let font_type = ctx
        .style()
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .unwrap_or_else(|| egui::FontId::monospace(14.0));
    TerminalFont::new(FontSettings { font_type })
}
