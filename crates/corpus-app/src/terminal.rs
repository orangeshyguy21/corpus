//! The embedded terminal wraps
//! `egui_term` (alacritty_terminal PTY backend). The pane NEVER spawns
//! opencode — it runs `tmux attach -t <run session>` inside the
//! embedded PTY, so tmux stays the supervisor: closing or crashing the
//! app closes the PTY, the tmux CLIENT detaches, and the run lives on.
//!
//! Focus discipline: an operator-launched pane focuses on first attach;
//! otherwise click the pane to focus it (keys — including the ctrl chords
//! the opencode TUI uses, and tab — route to the PTY). A click ANYWHERE
//! OUTSIDE the pane releases focus. The locally pinned egui_term separates
//! focused keyboard input from hover-gated pointer input. Scrollback,
//! selection
//! (drag; double/triple click for word/line), and copy/paste
//! (Cmd+C / Cmd+V) are egui_term's own.

use std::sync::mpsc::{channel, Receiver, Sender};

/// egui_term does not re-export its SelectionType (its `backend` module is
/// private), but it IS a public alias of alacritty's — and this crate pins
/// the same alacritty_terminal version as egui_term, so the types match.
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
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
    /// Fractional high-resolution wheel movement retained until it becomes
    /// a whole terminal row. This makes trackpads behave like native terminal
    /// scrollback without emitting one PTY write per pixel event.
    scroll_points: f32,
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
            scroll_points: 0.0,
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
        focus_new: bool,
    ) -> Result<(), String> {
        // Reconcile the current client before comparing targets. An exit that
        // was already queued must retire the backend that produced it, not a
        // replacement installed later in this method.
        self.drain_events();
        match target {
            Some((session, _)) if self.attached.as_deref() == Some(session.as_str()) => Ok(()),
            Some((session, argv)) => self.attach(ctx, &session, &argv, focus_new),
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
        self.scroll_points = 0.0;
    }

    /// Spawn the embedded PTY running the attach argv. The child is
    /// wrapped in `env TERM=… COLORTERM=…` because a GUI-launched app
    /// carries no TERM (tmux refuses to attach without one) and tmux
    /// 3.2+ passes RGB through for clients advertising truecolor.
    fn attach(
        &mut self,
        ctx: &egui::Context,
        session: &str,
        argv: &[String],
        focus_new: bool,
    ) -> Result<(), String> {
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
                self.focused = focus_new;
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
        let font = self.font.get_or_insert_with(|| pane_font(ui.ctx())).clone();
        let Some(backend) = self.backend.as_mut() else {
            return;
        };
        // The pane is the panel's only child: the available rect IS the
        // view rect (TerminalView allocates `ui.available_size()` at the
        // current cursor).
        let rect = ui.available_rect_before_wrap();
        Self::tmux_scroll_pass(&mut self.scroll_points, ui.ctx(), rect, backend);
        let mut selecting = self.shift_selecting;
        Self::shift_select_pass(&mut selecting, ui.ctx(), rect, backend);
        self.shift_selecting = selecting;
        let view = TerminalView::new(ui, backend)
            .set_theme(self.theme.clone())
            .set_font(font)
            .set_focus(self.focused);
        let response = ui.add(view);
        Self::copy_pass(ui.ctx(), rect, self.focused, backend);
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

    /// Route wheel/trackpad events to tmux before egui_term can reinterpret
    /// them as local-grid scroll or application arrow keys. Tmux owns the
    /// authoritative history for the attached alternate-screen TUI.
    fn tmux_scroll_pass(
        scroll_points: &mut f32,
        ctx: &egui::Context,
        rect: egui::Rect,
        backend: &mut TerminalBackend,
    ) {
        if !backend
            .last_content()
            .terminal_mode
            .intersects(TermMode::MOUSE_MODE)
        {
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos().filter(|pos| rect.contains(*pos)) else {
            return;
        };

        let size = backend.last_content().terminal_size;
        let cell_height = size.cell_height.max(1) as f32;
        let page_lines = (rect.height() / cell_height).floor().max(1.0) as i32;
        let mut lines = 0_i32;
        ctx.input_mut(|input| {
            input.events.retain(|event| {
                let egui::Event::MouseWheel { unit, delta, .. } = event else {
                    return true;
                };
                lines += wheel_lines(*unit, delta.y, cell_height, page_lines, scroll_points);
                false
            });
        });

        let lines = lines.clamp(-24, 24);
        if lines == 0 {
            return;
        }
        let cell_width = size.cell_width.max(1) as f32;
        let cols = (rect.width() / cell_width).floor().max(1.0) as usize;
        let rows = (rect.height() / cell_height).floor().max(1.0) as usize;
        let col = (((pos.x - rect.left()) / cell_width) as usize + 1).clamp(1, cols);
        let row = (((pos.y - rect.top()) / cell_height) as usize + 1).clamp(1, rows);
        backend.process_command(BackendCommand::Write(encode_sgr_scroll(lines, col, row)));
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

    /// Drain PTY events (keeps the backend's subscription channel empty).
    /// Backend ids are attachment generations: replacing a client can enqueue
    /// its Exit after the new client is installed, so only the active
    /// generation is allowed to detach the pane.
    fn drain_events(&mut self) {
        let active_id = self.backend.as_ref().map(|backend| backend.id);
        if drain_reports_active_exit(&self.pty_rx, active_id) {
            self.detach();
        }
    }
}

fn drain_reports_active_exit(receiver: &Receiver<(u64, PtyEvent)>, active_id: Option<u64>) -> bool {
    let mut active_exited = false;
    while let Ok((id, event)) = receiver.try_recv() {
        if Some(id) == active_id && matches!(event, PtyEvent::Exit) {
            active_exited = true;
        }
    }
    active_exited
}

fn wheel_lines(
    unit: egui::MouseWheelUnit,
    delta_y: f32,
    cell_height: f32,
    page_lines: i32,
    scroll_points: &mut f32,
) -> i32 {
    match unit {
        egui::MouseWheelUnit::Line => (delta_y.signum() * delta_y.abs().ceil()) as i32,
        egui::MouseWheelUnit::Point => {
            *scroll_points += delta_y;
            let lines = (*scroll_points / cell_height.max(1.0)).trunc() as i32;
            *scroll_points -= lines as f32 * cell_height.max(1.0);
            lines
        }
        egui::MouseWheelUnit::Page => delta_y.signum() as i32 * page_lines,
    }
}

fn encode_sgr_scroll(lines: i32, col: usize, row: usize) -> Vec<u8> {
    let button = if lines > 0 { 64 } else { 65 };
    let sequence = format!("\x1b[<{button};{col};{row}M");
    sequence.repeat(lines.unsigned_abs() as usize).into_bytes()
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

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::{drain_reports_active_exit, encode_sgr_scroll, wheel_lines, TerminalPane};
    use egui_term::PtyEvent;

    #[test]
    fn stale_backend_exit_cannot_detach_the_active_generation() {
        let (sender, receiver) = channel();
        sender.send((1, PtyEvent::Exit)).unwrap();

        assert!(!drain_reports_active_exit(&receiver, Some(2)));
    }

    #[test]
    fn active_backend_exit_is_detected_among_stale_events() {
        let (sender, receiver) = channel();
        sender.send((1, PtyEvent::Exit)).unwrap();
        sender.send((2, PtyEvent::Wakeup)).unwrap();
        sender.send((2, PtyEvent::Exit)).unwrap();

        assert!(drain_reports_active_exit(&receiver, Some(2)));
        assert!(receiver.try_recv().is_err(), "the event queue is drained");
    }

    #[test]
    fn exits_are_inert_without_an_active_backend() {
        let (sender, receiver) = channel();
        sender.send((1, PtyEvent::Exit)).unwrap();

        assert!(!drain_reports_active_exit(&receiver, None));
    }

    #[test]
    fn detach_resets_all_per_attachment_interaction_state() {
        let mut pane = TerminalPane::default();
        pane.attached = Some("mission-a".into());
        pane.focused = true;
        pane.shift_selecting = true;
        pane.scroll_points = 7.5;

        pane.detach();

        assert!(pane.attached.is_none());
        assert!(!pane.focused);
        assert!(!pane.shift_selecting);
        assert_eq!(pane.scroll_points, 0.0);
    }

    #[test]
    fn line_and_page_wheels_preserve_direction() {
        let mut points = 0.0;
        assert_eq!(
            wheel_lines(egui::MouseWheelUnit::Line, 1.2, 16.0, 20, &mut points),
            2
        );
        assert_eq!(
            wheel_lines(egui::MouseWheelUnit::Line, -1.2, 16.0, 20, &mut points),
            -2
        );
        assert_eq!(
            wheel_lines(egui::MouseWheelUnit::Page, 1.0, 16.0, 20, &mut points),
            20
        );
    }

    #[test]
    fn trackpad_points_accumulate_into_rows() {
        let mut points = 0.0;
        assert_eq!(
            wheel_lines(egui::MouseWheelUnit::Point, 7.0, 16.0, 20, &mut points),
            0
        );
        assert_eq!(points, 7.0);
        assert_eq!(
            wheel_lines(egui::MouseWheelUnit::Point, 11.0, 16.0, 20, &mut points),
            1
        );
        assert_eq!(points, 2.0);
        assert_eq!(
            wheel_lines(egui::MouseWheelUnit::Point, -20.0, 16.0, 20, &mut points),
            -1
        );
        assert_eq!(points, -2.0);
    }

    #[test]
    fn sgr_scroll_is_batched_into_one_payload() {
        assert_eq!(encode_sgr_scroll(2, 4, 7), b"\x1b[<64;4;7M\x1b[<64;4;7M");
        assert_eq!(encode_sgr_scroll(-1, 4, 7), b"\x1b[<65;4;7M");
    }
}
