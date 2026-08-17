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

use alacritty_terminal::term::TermMode;
/// egui_term does not re-export its SelectionType (its `backend` module is
/// private), but it IS a public alias of alacritty's — and this crate pins
/// the same alacritty_terminal version as egui_term, so the types match.
use alacritty_terminal::selection::SelectionType;
use egui_term::{
    BackendCommand, BackendSettings, ColorPalette, FontSettings, PtyEvent, TerminalBackend,
    TerminalFont, TerminalTheme, TerminalView,
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
    /// A Shift-drag selection is in progress: while true, primary-button
    /// press/move/release events are stripped from the frame BEFORE the
    /// egui_term view clones them (its press handler would forward them to
    /// the PTY as mouse reports, starting tmux's own copy-mode selection
    /// instead of a copyable local one — the mouse-mode copy bug).
    shift_selecting: bool,
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
            shift_selecting: false,
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
        self.shift_selecting = false;
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
        // The pane is the panel's only child: the available rect IS the
        // view rect (TerminalView allocates `ui.available_size()` at the
        // current cursor).
        let rect = ui.available_rect_before_wrap();
        let mut selecting = self.shift_selecting;
        Self::shift_select_pass(&mut selecting, ui.ctx(), rect, backend);
        self.shift_selecting = selecting;
        let view = TerminalView::new(ui, backend)
            .set_theme(self.theme.clone())
            .set_font(font)
            .set_focus(self.focused);
        let response = ui.add(view);
        Self::copy_pass(ui.ctx(), rect, self.focused, backend);
        // Scrollback: forward the wheel as SGR mouse reports when the
        // attached program requested mouse mode (corpus tmux sessions get
        // `set-option mouse on` at launch). egui_term's own wheel handler
        // only scrolls its LOCAL grid — which is empty under the alternate
        // screen tmux paints into — so without this there is no way to
        // scroll a run's history (the "can't scroll the log" bug).
        if backend
            .last_content()
            .terminal_mode
            .intersects(TermMode::MOUSE_MODE)
        {
            if let Some(pos) = ui.ctx().pointer_latest_pos() {
                if response.rect.contains(pos) {
                    let lines = ui.ctx().input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::MouseWheel {
                                    unit: egui::MouseWheelUnit::Line,
                                    delta,
                                    ..
                                } => Some(delta.y),
                                _ => None,
                            })
                            .sum::<f32>()
                    });
                    let mut n = lines.signum() * lines.abs().ceil().min(12.0);
                    while n != 0.0 {
                        // SGR wheel report (button 64/65, press-only), the
                        // exact bytes tmux answers with copy-mode scroll.
                        // egui_term's MouseButton type is not exported, so
                        // we write the sequence ourselves.
                        let button: u8 = if n > 0.0 { 64 } else { 65 };
                        let size = &backend.last_content().terminal_size;
                        let col =
                            ((pos.x - response.rect.left()) / size.cell_width.max(1) as f32) as usize
                                + 1;
                        let line =
                            ((pos.y - response.rect.top()) / size.cell_height.max(1) as f32) as usize
                                + 1;
                        backend.process_command(BackendCommand::Write(
                            format!("\x1b[<{button};{col};{line}M").into_bytes(),
                        ));
                        n -= n.signum();
                    }
                }
            }
        }
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

    /// Shift-drag local selection (the mouse-mode bypass). egui_term's press
    /// handler forwards EVERY left click to the PTY once the attached
    /// program enabled mouse reporting — and our tmux sessions always have
    /// `mouse on` — so a drag could never start the local, copyable
    /// selection (its SelectStart lives in the branch the mouse-report
    /// branch shadows). Standard terminals bypass mouse reporting with
    /// Shift; we do the same at the wrapper level: a Shift+press starts
    /// the selection, the primary-button events are stripped from the
    /// frame's event list BEFORE the view clones them (tmux never sees
    /// the drag, so it does not enter copy mode), and the drag drives
    /// SelectUpdate on the backend directly. Ends on button release.
    fn shift_select_pass(
        selecting: &mut bool,
        ctx: &egui::Context,
        rect: egui::Rect,
        backend: &mut TerminalBackend,
    ) {
        let (shift_press, released, latest) = ctx.input(|i| {
            let press = i.events.iter().find_map(|e| match e {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers,
                    pos,
                } if modifiers.shift && rect.contains(*pos) => Some(*pos),
                _ => None,
            });
            (
                press,
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.latest_pos(),
            )
        });
        if let Some(pos) = shift_press {
            *selecting = true;
            backend.process_command(BackendCommand::SelectStart(
                SelectionType::Simple,
                pos.x - rect.left(),
                pos.y - rect.top(),
            ));
        }
        if !*selecting {
            return;
        }
        // Swallow the drag from the view: leave PointerMoved and the
        // primary press/release in the event list and the view forwards
        // them as SGR reports (its own selection is tied to state the
        // stripped press never set).
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::PointerButton {
                        button: egui::PointerButton::Primary,
                        ..
                    } | egui::Event::PointerMoved(_)
                )
            });
        });
        if let Some(pos) = latest {
            backend.process_command(BackendCommand::SelectUpdate(
                pos.x - rect.left(),
                pos.y - rect.top(),
            ));
        }
        if released {
            *selecting = false;
        }
    }

    /// Copy the local selection to the OS clipboard on Cmd+C (egui Copy
    /// event). Pane-level, not the view's: the view only handles Copy
    /// while focused, and a Shift-drag select never grabbed focus. Gated
    /// on focus-or-hover so a Cmd+C aimed at another pane is untouched;
    /// the string the view would copy is the same one, so a focused pane
    /// just copies twice.
    fn copy_pass(ctx: &egui::Context, rect: egui::Rect, focused: bool, backend: &TerminalBackend) {
        let copy_here = ctx.input(|i| {
            let copy = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            let over = i.pointer.latest_pos().is_some_and(|pos| rect.contains(pos));
            copy && (focused || over)
        });
        if !copy_here {
            return;
        }
        let content = backend.selectable_content();
        if !content.is_empty() {
            ctx.copy_text(content);
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
