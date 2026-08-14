//! Project view (app-flow chunk 3, app-parity-spec §5): the mock-faithful
//! detail screen for the SELECTED project — header `Project: <name>` +
//! Clone / Delete top-right + the dim `created:` stamp; a Plugin section
//! (flat dropdown with a live probe badge, Saved via a rebind); a Corpus
//! section (file/byte summary + an inline red Delete that wipes the corpus
//! behind a confirm, and the painted stack-of-plates graphic); Save
//! bottom-right. The project LIST lives in the sidebar (chunk 1), so this
//! screen is a detail view, not a table.
//!
//! No business logic here: corpus-core calls go through `AppState`;
//! results surface as toasts. Probing is a corpus-core aggregation
//! (`AppState::refresh_plugins`), scheduled on demand, never per-frame.

use std::time::Duration;

use egui::{Align2, RichText, Ui};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::fmt::fmt_bytes;
use crate::state::AppState;
use crate::theme;
use crate::views::plugin_picker::plugin_picker;

/// Widget state for the Project view: the plugin picker in progress, the
/// wipe confirm, and the clone dialog. The selected project itself lives on
/// `AppState`.
pub struct ProjectsView {
    /// The slug this view is bound to (drives `edit_plugin` re-sync on
    /// project switch).
    project: Option<String>,
    /// Plugin binding being edited (Saved to rebind the project).
    edit_plugin: String,
    /// Open the confirm dialog before a corpus wipe.
    confirm_wipe: bool,
    show_clone: bool,
    clone_name: String,
    clone_corpus: bool,
    /// Schedule a fresh plugin probe aggregation next frame (probe state
    /// is fetched on demand, not continuously).
    needs_probe: bool,
}

impl Default for ProjectsView {
    fn default() -> Self {
        Self {
            project: None,
            edit_plugin: String::new(),
            confirm_wipe: false,
            show_clone: false,
            clone_name: String::new(),
            clone_corpus: false,
            needs_probe: false,
        }
    }
}

impl ProjectsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = state.effective_project() else {
            ui.add_space(24.0);
            ui.add(
                egui::Label::new(RichText::new("no project selected").color(theme::TEXT_FAINT)),
            );
            return;
        };
        // Owned spec copy: no reference into `state` is held, so the view
        // can call `&mut state` methods below (save, wipe, delete).
        let Some(project) = state
            .projects
            .iter()
            .find(|(s, _)| s == &slug)
            .map(|(_, p)| p.clone())
        else {
            return;
        };

        // Sync the in-progress plugin binding when the viewed project
        // changes (so opening a different project shows its current plugin).
        if self.project.as_deref() != Some(slug.as_str()) {
            self.project = Some(slug.clone());
            self.edit_plugin = project.plugin.clone();
            self.confirm_wipe = false;
        }
        // Drain a requested plugin re-probe before the picker renders.
        if self.needs_probe {
            state.refresh_plugins();
            self.needs_probe = false;
        }

        // --- header (spec §5): `Project: <name>` + Delete / Clone /
        // created stamp, then a hairline.
        let name = if project.name.is_empty() {
            slug.clone()
        } else {
            project.name.clone()
        };
        ui.horizontal(|ui| {
            ui.label(theme::screen_header(format!("Project: {name}")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::destructive_button(ui, "Delete").clicked() {
                    self.delete_project(state, toasts, &slug);
                }
                if theme::house_button(ui, "Clone").clicked() {
                    self.clone_name.clear();
                    self.clone_corpus = false;
                    self.show_clone = true;
                }
                ui.label(
                    RichText::new(format!("created: {}", fmt_epoch(project.created)))
                        .size(12.0)
                        .color(theme::TEXT_FAINT),
                );
            });
        });
        theme::hairline(ui);
        ui.add_space(24.0);

        // --- Plugin section (spec §5): heading, then the flat field. NO
        // helper text under it.
        ui.label(theme::section_heading("Plugin"));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            plugin_picker(ui, &mut self.edit_plugin, state.plugins(), &mut self.needs_probe);
        });
        ui.add_space(28.0);

        // --- Corpus section (spec §5): heading, then the stats row + the
        // inline red Delete (wipe confirm), then the stack graphic.
        ui.label(theme::section_heading("Corpus"));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            match state.corpus_stats() {
                Some(stats) => {
                    ui.label(
                        RichText::new(format!("{} files", stats.files))
                            .size(14.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(fmt_bytes(stats.bytes))
                            .size(14.0)
                            .color(theme::TEXT_MUTED),
                    );
                }
                None => {
                    ui.label(RichText::new("corpus not computed").color(theme::TEXT_FAINT));
                }
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::destructive_button(ui, "Delete").clicked() {
                    self.confirm_wipe = true;
                }
            });
        });
        ui.add_space(8.0);

        // The painted stack-of-plates graphic (decorative v1; the TODO
        // marks the data-driven revision).
        self.stack_graphic(ui);
        ui.add_space(8.0);

        // --- Save (rebind) bottom-right (spec §5).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
            if theme::house_button(ui, "Save").clicked() {
                self.save_binding(state, toasts, &slug);
            }
        });

        self.clone_window(ui, state, toasts, &slug);
        self.wipe_confirm_window(ui, state, toasts, &slug);
    }

    /// Rebind the project's plugin and refresh (projects + the source/env
    /// the top bar and sidebar derive from the new binding).
    fn save_binding(&mut self, state: &mut AppState, toasts: &mut Toasts, slug: &str) {
        if self.edit_plugin.trim().is_empty() {
            toast(toasts, ToastKind::Warning, "pick a plugin first");
            return;
        }
        match state.rebind_project(slug, self.edit_plugin.trim()) {
            Ok(project) => {
                toast(
                    toasts,
                    ToastKind::Success,
                    format!("rebound {slug} -> plugin {}", project.plugin),
                );
                state.refresh();
                // Refresh the per-source pins + env for the new binding.
                state.select_project(slug);
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// Delete the project (header Delete); the default-project refusal
    /// bubbles up as a toast.
    fn delete_project(&mut self, state: &mut AppState, toasts: &mut Toasts, slug: &str) {
        match state.delete_project(slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, format!("deleted project {slug}"));
                state.refresh();
                // ensure_selection re-picks a project next frame.
                state.selected_project = None;
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// The Corpus Delete confirm: wiping empties the categories and bumps
    /// `corpus_generation` (verified via CLI); the project + agents survive.
    fn wipe_confirm_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts, slug: &str) {
        if !self.confirm_wipe {
            return;
        }
        let mut open = self.confirm_wipe;
        let mut wiped = false;
        let mut cancel = false;
        egui::Window::new("Delete corpus")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
            .show(ui.ctx(), |ui| {
                ui.label("This wipes the project corpus and bumps the generation.");
                ui.weak("Findings, techniques, hypotheses, attacks and run logs are removed; the project and its agents survive. There is no undo.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::destructive_button(ui, "Wipe corpus").clicked() {
                        match state.wipe_project_corpus(slug) {
                            Ok(project) => {
                                toast(
                                    toasts,
                                    ToastKind::Success,
                                    format!(
                                        "corpus wiped (generation {})",
                                        project.corpus_generation
                                    ),
                                );
                                state.refresh();
                                state.refresh_corpus_stats(slug);
                                wiped = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                        }
                    }
                    if theme::house_button(ui, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        self.confirm_wipe = open && !wiped && !cancel;
    }

    /// The Clone dialog: display name (defaults to the source's) + the
    /// copy-corpus toggle.
    fn clone_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts, from: &str) {
        if !self.show_clone {
            return;
        }
        let mut open = self.show_clone;
        let mut cloned = false;
        egui::Window::new(format!("Clone project: {from}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (optional — defaults to the source's)");
                ui.text_edit_singleline(&mut self.clone_name);
                ui.checkbox(&mut self.clone_corpus, "copy the corpus");
                ui.add_space(8.0);
                if theme::house_button(ui, "Clone").clicked() {
                    let name = if self.clone_name.trim().is_empty() {
                        None
                    } else {
                        Some(self.clone_name.trim())
                    };
                    match state.clone_project(from, name, self.clone_corpus) {
                        Ok((to, _)) => {
                            toast(toasts, ToastKind::Success, format!("cloned project {from} -> {to}"));
                            state.refresh();
                            state.select_project(&to);
                            cloned = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        self.show_clone = open && !cloned;
    }

    /// The decorative stack-of-plates graphic (spec §5, `paint_corpus_stack`):
    /// N = 12 parallelograms receding up-right, drawn back-to-front; only
    /// the front plate fills. Purely painted — no data binding.
    /// TODO: data-driven plate count.
    fn stack_graphic(&mut self, ui: &mut Ui) {
        let avail = ui.available_width();
        let size = egui::vec2(avail.min(760.0), 240.0);
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        paint_corpus_stack(&painter, rect);
    }
}

/// The stack-of-plates painter (spec §5): plates recede up-right, stroke
/// PLATE_LINE, the front (i = 0) plate also fills PLATE_FRONT.
fn paint_corpus_stack(painter: &egui::Painter, rect: egui::Rect) {
    let n = 12usize;
    let x0 = 60.0;
    let y0 = rect.bottom() - 60.0;
    let stroke = egui::Stroke::new(1.0_f32, theme::PLATE_LINE);
    for i in (0..n).rev() {
        let ox = x0 + (i as f32) * 34.0;
        let oy = y0 - (i as f32) * 15.0;
        let corners = [
            egui::pos2(ox, oy),
            egui::pos2(ox + 300.0, oy),
            egui::pos2(ox + 330.0, oy - 20.0),
            egui::pos2(ox + 30.0, oy - 20.0),
        ];
        if i == 0 {
            painter.add(egui::Shape::convex_polygon(
                corners.to_vec(),
                theme::PLATE_FRONT,
                stroke,
            ));
        } else {
            painter.add(egui::Shape::closed_line(corners.to_vec(), stroke));
        }
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

/// Format epoch seconds as `YYYY-MM-DD HH:MMZ` (UTC). Display-only
/// formatting for the created stamp — no date dependency needed.
fn fmt_epoch(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let secs = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// Howard Hinnant's civil-from-days algorithm (public domain).
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}