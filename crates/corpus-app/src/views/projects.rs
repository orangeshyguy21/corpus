//! Project view (app-flow chunk 3, app-parity-spec §5): the mock-faithful
//! detail screen for the SELECTED project — header `Project: <name>` +
//! Clone / Delete top-right + the dim `created:` stamp; a Plugin section
//! (flat dropdown with a live probe badge, Saved via a rebind); a Corpus
//! section (file/byte summary + an inline red Delete that wipes the corpus
//! behind a confirm, then the data-driven visual: a proportional strip of
//! category byte shares + legend); a Mission Logs section (the
//! `corpus/runs/` transcripts, summarized and listed on their own — they
//! outweigh the knowledge categories by orders of magnitude, so mixing
//! them into the Corpus numbers hides everything else); a Cost section
//! (per-model token/cost table aggregated from the exported run
//! transcripts, total row); Save bottom-right. The project LIST lives in
//! the sidebar (chunk 1), so this screen is a detail view, not a table.
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
    /// The Rename dialog + its edit buffer (the project's display label; the
    /// slug never moves).
    show_rename: bool,
    rename_name: String,
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
            show_rename: false,
            rename_name: String::new(),
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
                if theme::house_button(ui, "Rename").clicked() {
                    self.rename_name = name.clone();
                    self.show_rename = true;
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

        // --- Sources section: the project's rev pins (the data lives on
        // project.yaml — `pins` — so this is its home as well as the top
        // bar's). One dropdown per plugin-declared source, same widget as
        // the top bar; a pick persists immediately.
        ui.label(theme::section_heading("Sources"));
        ui.add_space(12.0);
        if state.source_revs.is_empty() {
            ui.label(
                RichText::new("no sources declared by this plugin")
                    .size(12.0)
                    .color(theme::TEXT_FAINT),
            );
        } else {
            let revs = state.source_revs.clone();
            ui.horizontal_wrapped(|ui| {
                for source in &revs {
                    let selected = state
                        .source_pins
                        .get(&source.name)
                        .cloned()
                        .unwrap_or_else(|| source.default_rev().to_string());
                    if let Some(rev) = crate::views::source_dropdown::source_dropdown(
                        ui,
                        &format!("project_source_{}", source.name),
                        source,
                        &selected,
                        None, // no live probe on the project page
                    ) {
                        if let Err(error) = state.set_source_pin(&slug, &source.name, &rev) {
                            toast(toasts, ToastKind::Error, error.to_string());
                        }
                    }
                }
            });
        }
        ui.add_space(28.0);

        // --- Corpus section (spec §5): heading, then the stats row + the
        // inline red Delete (wipe confirm), then the data-driven category
        // visual (proportional strip + legend). Knowledge categories only
        // — mission logs get their own section below, since a single run
        // transcript outweighs the whole corpus and would flatten the
        // strip to one grey bar.
        ui.label(theme::section_heading("Corpus"));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            match state.corpus_stats() {
                Some(stats) => {
                    ui.label(
                        RichText::new(format!("{} files", stats.knowledge_files()))
                            .size(14.0)
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(fmt_bytes(stats.knowledge_bytes()))
                            .size(14.0)
                            .color(theme::TEXT_MUTED),
                    );
                }
                None => {
                    ui.label(RichText::new("corpus not computed").color(theme::TEXT_FAINT));
                }
            };
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // The wipe is corpus-wide — say so, now that the mission
                // logs are shown as a section of their own.
                let delete = theme::destructive_button(ui, "Delete")
                    .on_hover_text("wipe the whole corpus — mission logs included");
                if delete.clicked() {
                    self.confirm_wipe = true;
                }
            });
        });
        ui.add_space(12.0);
        if let Some(stats) = state.corpus_stats() {
            if stats.knowledge_files() > 0 {
                corpus_visual(ui, &stats.categories);
            } else {
                ui.label(
                    RichText::new("empty — missions write findings, techniques, hypotheses and attacks here")
                        .size(12.0)
                        .color(theme::TEXT_FAINT),
                );
            }
        }
        ui.add_space(28.0);

        // --- Mission Logs section: the `corpus/runs/` transcripts, kept
        // apart from the corpus above. Summary row, then one row per log
        // (newest first) with its share of the total as a bar.
        ui.label(theme::section_heading("Mission Logs"));
        ui.add_space(12.0);
        let logs = state.corpus_stats().map(|s| s.logs.clone()).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} logs", logs.files))
                    .size(14.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(4.0);
            ui.label(RichText::new(fmt_bytes(logs.bytes)).size(14.0).color(theme::TEXT_MUTED));
        });
        ui.add_space(12.0);
        if logs.files == 0 {
            ui.label(
                RichText::new("no runs yet — each launched mission writes its transcript here")
                    .size(12.0)
                    .color(theme::TEXT_FAINT),
            );
        } else {
            mission_log_list(ui, state.mission_logs(), logs.bytes);
        }
        ui.add_space(28.0);

        // --- Cost section: per-model usage aggregated from the exported
        // run transcripts (runs/*.json), cost-desc, with a total row.
        ui.label(theme::section_heading("Cost"));
        ui.add_space(12.0);
        match state.corpus_cost() {
            Some(report) if !report.rows.is_empty() => {
                cost_headline(ui, report);
                ui.add_space(14.0);
                cost_table(ui, report);
            }
            _ => {
                ui.label(
                    RichText::new("no usage yet — updates each time an agent finishes a turn")
                        .size(12.0)
                        .color(theme::TEXT_FAINT),
                );
            }
        }
        ui.add_space(28.0);

        // --- Save (rebind) bottom-right (spec §5).
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
            if theme::house_button(ui, "Save").clicked() {
                self.save_binding(state, toasts, &slug);
            }
        });

        self.clone_window(ui, state, toasts, &slug);
        self.rename_window(ui, state, toasts, &slug);
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

    /// The Rename dialog: the project's display LABEL only. The slug is the
    /// project's identity — its directory name and the key agents, missions,
    /// run dirs and pins are filed under — so a rename never moves it.
    fn rename_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts, slug: &str) {
        if !self.show_rename {
            return;
        }
        let mut open = self.show_rename;
        let mut renamed = false;
        egui::Window::new("Rename project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name");
                let entry = ui.text_edit_singleline(&mut self.rename_name);
                ui.label(
                    RichText::new(format!("id stays `{slug}`"))
                        .size(12.0)
                        .color(theme::TEXT_FAINT),
                );
                ui.add_space(8.0);
                let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let named = !self.rename_name.trim().is_empty();
                let clicked = ui
                    .add_enabled_ui(named, |ui| theme::house_button(ui, "Rename"))
                    .inner
                    .clicked();
                if clicked || (submit && named) {
                    match state.rename_project(slug, &self.rename_name) {
                        Ok(project) => {
                            toast(
                                toasts,
                                ToastKind::Success,
                                format!("renamed project to {}", project.name),
                            );
                            state.refresh();
                            renamed = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        self.show_rename = open && !renamed;
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
                let entry = ui.text_edit_singleline(&mut self.clone_name);
                ui.checkbox(&mut self.clone_corpus, "copy the corpus");
                ui.add_space(8.0);
                let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if theme::house_button(ui, "Clone").clicked() || submit {
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

}

/// The corpus visual: a full-width strip segmented by each category's
/// byte share (hover a segment for its files/bytes), with a legend row
/// under it — the "what's in the corpus" answer at a glance. Shares are
/// taken over the categories PASSED IN (mission logs are excluded by the
/// caller), so the knowledge mix stays readable.
fn corpus_visual(ui: &mut Ui, categories: &[corpus_core::CategoryStat]) {
    let total: u64 = categories.iter().map(|c| c.bytes).sum();
    let width = ui.available_width().min(760.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 1.0, theme::PLATE_FRONT);
    let mut x = rect.left();
    for (i, category) in categories.iter().enumerate() {
        let share = category.bytes as f32 / total.max(1) as f32;
        let w = if i == categories.len() - 1 {
            rect.right() - x // last segment absorbs rounding
        } else {
            (rect.width() * share).max(2.0)
        };
        let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, rect.height()));
        let color = theme::CORPUS_PALETTE[i % theme::CORPUS_PALETTE.len()];
        painter.rect_filled(seg, 0.0, color);
        painter.rect_stroke(
            seg,
            0.0,
            egui::Stroke::new(1.0_f32, theme::BG),
            egui::StrokeKind::Inside,
        );
        ui.allocate_rect(seg, egui::Sense::hover()).on_hover_text(format!(
            "{} — {} files, {}",
            category.name,
            category.files,
            fmt_bytes(category.bytes)
        ));
        x += w;
    }
    ui.add_space(8.0);
    // Legend: swatch + name + files + bytes per category.
    for (i, category) in categories.iter().enumerate() {
        ui.horizontal(|ui| {
            let (dot, _) =
                ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(dot, 1.0, theme::CORPUS_PALETTE[i % theme::CORPUS_PALETTE.len()]);
            ui.label(RichText::new(&category.name).size(12.0).color(theme::TEXT));
            ui.label(
                RichText::new(format!("{} files", category.files))
                    .size(12.0)
                    .color(theme::TEXT_MUTED),
            );
            ui.label(
                RichText::new(fmt_bytes(category.bytes))
                    .size(12.0)
                    .color(theme::TEXT_FAINT),
            );
        });
    }
}

/// How many mission logs the list shows before folding the rest into a
/// tail line — the newest runs are the ones anyone reads.
const MISSION_LOG_ROWS: usize = 12;

/// The Mission Logs list: one row per transcript (newest first) — mission
/// name, run stamp, file name, size, and a bar sized to its share of the
/// logs total, so a runaway run is obvious at a glance.
fn mission_log_list(ui: &mut Ui, logs: &[corpus_core::MissionLog], total: u64) {
    let width = ui.available_width().min(760.0);
    for log in logs.iter().take(MISSION_LOG_ROWS) {
        // Fixed row width so the right-aligned file name tracks the strip
        // above it instead of the window edge.
        ui.allocate_ui(egui::vec2(width, 16.0), |ui| {
            ui.horizontal(|ui| {
                let (bar, _) =
                    ui.allocate_exact_size(egui::vec2(90.0, 10.0), egui::Sense::hover());
                let painter = ui.painter_at(bar);
                painter.rect_filled(bar, 1.0, theme::PLATE_FRONT);
                let share = log.bytes as f32 / total.max(1) as f32;
                let filled = egui::Rect::from_min_size(
                    bar.min,
                    egui::vec2((bar.width() * share).max(1.0), bar.height()),
                );
                painter.rect_filled(filled, 1.0, theme::MISSION_LOG);
                ui.label(RichText::new(&log.mission).size(12.0).color(theme::TEXT));
                ui.label(
                    RichText::new(fmt_bytes(log.bytes))
                        .size(12.0)
                        .monospace()
                        .color(theme::TEXT_MUTED),
                );
                if log.started > 0 {
                    ui.label(
                        RichText::new(fmt_epoch(log.started))
                            .size(12.0)
                            .color(theme::TEXT_FAINT),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&log.name)
                                .size(11.0)
                                .monospace()
                                .color(theme::TEXT_FAINT),
                        )
                        .truncate(),
                    )
                    .on_hover_text(format!("corpus/runs/{}", log.name));
                });
            });
        });
    }
    if logs.len() > MISSION_LOG_ROWS {
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!("+{} older", logs.len() - MISSION_LOG_ROWS))
                .size(12.0)
                .color(theme::TEXT_FAINT),
        );
    }
}

/// The Cost section's headline: the two figures that matter at a glance —
/// total tokens across the project, and total USD. Cost stays $0 for local /
/// free providers, so tokens lead: they are the real usage signal here. The
/// per-model table below is the breakdown.
fn cost_headline(ui: &mut Ui, report: &corpus_core::CostReport) {
    let stat = |ui: &mut Ui, value: String, label: &str| {
        ui.vertical(|ui| {
            ui.label(RichText::new(value).size(22.0).strong().color(theme::TEXT));
            ui.add_space(2.0);
            ui.label(RichText::new(label).size(11.0).color(theme::TEXT_FAINT));
        });
    };
    ui.horizontal(|ui| {
        stat(ui, crate::fmt::fmt_tokens(report.tokens), "total tokens");
        ui.add_space(32.0);
        stat(ui, crate::fmt::fmt_usd(report.cost), "total cost");
    });
}

/// The Cost table: one row per (model, provider) with token breakdown,
/// cost-desc; a bold total row closes it out.
fn cost_table(ui: &mut Ui, report: &corpus_core::CostReport) {
    use egui_extras::{Column, TableBuilder};
    let heading = |text: &str| RichText::new(text).size(12.0).color(theme::TEXT_FAINT);
    let cell = |text: String| RichText::new(text).size(12.5).color(theme::TEXT);
    let num = |text: String| RichText::new(text).size(12.5).monospace().color(theme::TEXT_MUTED);
    TableBuilder::new(ui)
        .id_salt("project_cost_table")
        .column(Column::initial(170.0).at_least(120.0)) // model
        .column(Column::initial(110.0).at_least(80.0)) // provider
        .column(Column::exact(70.0)) // input
        .column(Column::exact(70.0)) // output
        .column(Column::exact(70.0)) // reasoning
        .column(Column::exact(70.0)) // cache read
        .column(Column::exact(70.0)) // cache write
        .column(Column::exact(90.0)) // cost
        .header(20.0, |mut header| {
            for title in ["model", "provider", "in", "out", "reason", "cache r", "cache w", "cost"] {
                header.col(|ui| {
                    ui.label(heading(title));
                });
            }
        })
        .body(|mut body| {
            for row in &report.rows {
                body.row(20.0, |mut tr| {
                    tr.col(|ui| {
                        ui.label(cell(row.model.clone()));
                    });
                    tr.col(|ui| {
                        ui.label(cell(row.provider.clone()));
                    });
                    tr.col(|ui| {
                        ui.label(num(crate::fmt::fmt_tokens(row.tokens_input)));
                    });
                    tr.col(|ui| {
                        ui.label(num(crate::fmt::fmt_tokens(row.tokens_output)));
                    });
                    tr.col(|ui| {
                        ui.label(num(crate::fmt::fmt_tokens(row.tokens_reasoning)));
                    });
                    tr.col(|ui| {
                        ui.label(num(crate::fmt::fmt_tokens(row.cache_read)));
                    });
                    tr.col(|ui| {
                        ui.label(num(crate::fmt::fmt_tokens(row.cache_write)));
                    });
                    tr.col(|ui| {
                        ui.label(
                            RichText::new(crate::fmt::fmt_usd(row.cost))
                                .size(12.5)
                                .monospace()
                                .strong()
                                .color(theme::TEXT),
                        );
                    });
                });
            }
            // Total row.
            body.row(22.0, |mut tr| {
                let total_in: u64 = report.rows.iter().map(|r| r.tokens_input).sum();
                let total_out: u64 = report.rows.iter().map(|r| r.tokens_output).sum();
                let total_reason: u64 = report.rows.iter().map(|r| r.tokens_reasoning).sum();
                let total_cr: u64 = report.rows.iter().map(|r| r.cache_read).sum();
                let total_cw: u64 = report.rows.iter().map(|r| r.cache_write).sum();
                let strong_num = |text: String| {
                    RichText::new(text).size(12.5).monospace().strong().color(theme::TEXT)
                };
                tr.col(|ui| {
                    ui.label(strong_num("total".to_string()));
                });
                tr.col(|ui| {
                    ui.label(num(format!("{} tok", crate::fmt::fmt_tokens(report.tokens))));
                });
                tr.col(|ui| {
                    ui.label(strong_num(crate::fmt::fmt_tokens(total_in)));
                });
                tr.col(|ui| {
                    ui.label(strong_num(crate::fmt::fmt_tokens(total_out)));
                });
                tr.col(|ui| {
                    ui.label(strong_num(crate::fmt::fmt_tokens(total_reason)));
                });
                tr.col(|ui| {
                    ui.label(strong_num(crate::fmt::fmt_tokens(total_cr)));
                });
                tr.col(|ui| {
                    ui.label(strong_num(crate::fmt::fmt_tokens(total_cw)));
                });
                tr.col(|ui| {
                    ui.label(strong_num(crate::fmt::fmt_usd(report.cost)));
                });
            });
        });
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