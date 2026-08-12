//! Projects screen (app-flow chunks 1+2): the project list as a striped
//! table, create (display name + plugin from a dropdown over the
//! discovered environment plugins, each with a live probe badge; the
//! machine id is auto-generated), edit (change the plugin binding the
//! same way), clone (with or without the shared corpus), delete (the
//! default-project refusal surfaces as a toast).
//!
//! First-run detection: a store with zero projects lands on the create
//! form — the seed of the chunk-6 onboarding wizard.
//!
//! No business logic here: corpus-core calls go through `AppState`;
//! results surface as toasts. Probing is a corpus-core aggregation
//! (`AppState::refresh_plugins`), scheduled on demand, never per-frame.

use std::time::Duration;

use egui::{Align2, Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use corpus_core::PluginStatus;

use crate::state::AppState;

/// Probe-status badge colors (green = ready, amber = not ready).
const READY: Color32 = Color32::from_rgb(120, 200, 120);
const NOT_READY: Color32 = Color32::from_rgb(255, 180, 90);

/// Widget state for the Projects screen: the row selection plus the
/// create/clone/edit form fields. Corpus state lives in `AppState`.
pub struct ProjectsView {
    selected: Option<String>,
    last_was_empty: bool,
    show_create: bool,
    create_name: String,
    create_plugin: String,
    show_clone: bool,
    clone_name: String,
    clone_corpus: bool,
    show_edit: bool,
    edit_plugin: String,
    /// Schedule a fresh plugin probe aggregation next frame (probe state
    /// is fetched on demand, not continuously).
    needs_probe: bool,
}

impl Default for ProjectsView {
    fn default() -> Self {
        Self {
            selected: None,
            last_was_empty: false,
            show_create: false,
            create_name: String::new(),
            create_plugin: "cdk-regtest".to_string(),
            show_clone: false,
            clone_name: String::new(),
            clone_corpus: false,
            show_edit: false,
            edit_plugin: String::new(),
            needs_probe: false,
        }
    }
}

impl ProjectsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        // First-run (and first-run-after-everything-deleted): land on the
        // create form. Only on the empty -> non-empty transition, so a
        // dismissed form stays dismissed while the store is empty.
        let empty = state.projects.is_empty();
        if empty && !self.last_was_empty {
            self.show_create = true;
            self.needs_probe = true;
        }
        self.last_was_empty = empty;

        ui.horizontal(|ui| {
            ui.heading("Projects");
            ui.add_space(8.0);
            ui.weak(state.store_root());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("+ New project").clicked() {
                self.show_create = true;
                self.needs_probe = true;
            }
            let has_selection = self.selected.is_some();
            let edit = ui
                .add_enabled(has_selection, egui::Button::new("Edit…"))
                .on_disabled_hover_text("select a project row first");
            if edit.clicked() {
                if let Some((_, project)) = state
                    .projects
                    .iter()
                    .find(|(slug, _)| self.selected.as_deref() == Some(slug.as_str()))
                {
                    self.edit_plugin = project.plugin.clone();
                }
                self.show_edit = true;
                self.needs_probe = true;
            }
            let clone = ui
                .add_enabled(has_selection, egui::Button::new("Clone…"))
                .on_disabled_hover_text("select a project row first");
            if clone.clicked() {
                self.clone_name.clear();
                self.show_clone = true;
            }
            let delete = ui
                .add_enabled(has_selection, egui::Button::new("Delete"))
                .on_disabled_hover_text("select a project row first");
            if delete.clicked() {
                self.delete_selected(state, toasts);
            }
            if ui.button("Refresh").clicked() {
                state.refresh();
                self.needs_probe = true;
            }
            if let Some(slug) = &self.selected {
                ui.separator();
                ui.weak(format!("selected: {slug}"));
            }
        });
        ui.add_space(8.0);

        // Drain a requested plugin re-probe before the forms render, so a
        // freshly opened picker shows current badges.
        if self.needs_probe {
            state.refresh_plugins();
            self.needs_probe = false;
        }

        if empty {
            ui.add_space(40.0);
            ui.label(
                RichText::new("No projects yet — create your first project.")
                    .weak()
                    .size(18.0),
            );
            ui.add_space(8.0);
        }

        self.project_table(ui, state);

        self.create_window(ui, state, toasts);
        self.clone_window(ui, state, toasts);
        self.edit_window(ui, state, toasts);
    }

    /// The striped project table; clicking a row selects it.
    fn project_table(&mut self, ui: &mut Ui, state: &mut AppState) {
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(140.0))
            .column(Column::remainder().at_least(120.0))
            .column(Column::auto().at_least(110.0))
            .column(Column::auto().at_least(150.0))
            .column(Column::auto().at_least(90.0))
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong("id");
                });
                header.col(|ui| {
                    ui.strong("name");
                });
                header.col(|ui| {
                    ui.strong("plugin");
                });
                header.col(|ui| {
                    ui.strong("created");
                });
                header.col(|ui| {
                    ui.strong("cloned from");
                });
            })
            .body(|mut body| {
                let local_projects = &state.projects;
                for (slug, project) in local_projects {
                    let selected = self.selected.as_deref() == Some(slug.as_str());
                    body.row(26.0, |mut row| {
                        row.col(|ui| {
                            if ui
                                .selectable_label(selected, RichText::new(slug).monospace())
                                .clicked()
                            {
                                self.selected = Some(slug.clone());
                            }
                        });
                        row.col(|ui| {
                            ui.label(&project.name);
                        });
                        row.col(|ui| {
                            ui.monospace(&project.plugin);
                        });
                        row.col(|ui| {
                            ui.weak(fmt_epoch(project.created));
                        });
                        row.col(|ui| {
                            if let Some(from) = &project.cloned_from {
                                ui.weak(from);
                            }
                        });
                    });
                }
            });
    }

    /// The create form: display name + the plugin picker (dropdown over
    /// the discovered plugins with live probe badges). The machine id is
    /// generated (state.rs) — the operator never types one.
    fn create_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let mut open = self.show_create;
        let mut created = false;
        egui::Window::new("New project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (this shows in lists — the id is generated)");
                ui.text_edit_singleline(&mut self.create_name);
                ui.label("Environment plugin");
                plugin_picker(ui, &mut self.create_plugin, state.plugins(), &mut self.needs_probe);
                ui.add_space(8.0);
                if ui.button("Create").clicked() {
                    let name = self.create_name.trim();
                    if name.is_empty() {
                        toast(toasts, ToastKind::Warning, "display name is required");
                    } else {
                        match state.create_project(name, self.create_plugin.trim()) {
                            Ok((id, project)) => {
                                toast(
                                    toasts,
                                    ToastKind::Success,
                                    format!("created project {} ({id})", project.name),
                                );
                                state.refresh();
                                self.create_name.clear();
                                created = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                        }
                    }
                }
            });
        self.show_create = open && !created;
    }

    /// The clone form: display name (falls back to the source's) plus
    /// the copy-corpus toggle. The new id is generated, like create's.
    fn clone_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(from) = self.selected.clone() else {
            self.show_clone = false;
            return;
        };
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
                ui.checkbox(&mut self.clone_corpus, "copy the shared corpus");
                ui.add_space(8.0);
                if ui.button("Clone").clicked() {
                    let name = if self.clone_name.trim().is_empty() {
                        None
                    } else {
                        Some(self.clone_name.trim())
                    };
                    match state.clone_project(&from, name, self.clone_corpus) {
                        Ok((to, _)) => {
                            toast(
                                toasts,
                                ToastKind::Success,
                                format!("cloned project {from} -> {to}"),
                            );
                            state.refresh();
                            self.clone_name.clear();
                            cloned = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        self.show_clone = open && !cloned;
    }

    /// Delete the selected project; the default-project refusal bubbles
    /// up as an error toast (the operator never loses `default`).
    fn delete_selected(&mut self, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = self.selected.clone() else {
            return;
        };
        match state.delete_project(&slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, format!("deleted project {slug}"));
                self.selected = None;
                state.refresh();
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// The edit form: change the selected project's plugin binding with
    /// the same badge-carrying picker as create.
    fn edit_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = self.selected.clone() else {
            self.show_edit = false;
            return;
        };
        let name = state
            .projects
            .iter()
            .find(|(s, _)| *s == slug)
            .map(|(_, p)| p.name.clone())
            .unwrap_or_default();
        let mut open = self.show_edit;
        let mut rebound = false;
        egui::Window::new(format!("Edit project: {slug}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name");
                ui.weak(&name);
                ui.label("Environment plugin");
                plugin_picker(ui, &mut self.edit_plugin, state.plugins(), &mut self.needs_probe);
                ui.add_space(8.0);
                if ui.button("Save binding").clicked() {
                    if self.edit_plugin.trim().is_empty() {
                        toast(toasts, ToastKind::Warning, "pick a plugin first");
                    } else {
                        match state.rebind_project(&slug, self.edit_plugin.trim()) {
                            Ok(project) => {
                                toast(
                                    toasts,
                                    ToastKind::Success,
                                    format!("rebound project {slug} -> plugin {}", project.plugin),
                                );
                                state.refresh();
                                rebound = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                        }
                    }
                }
            });
        self.show_edit = open && !rebound;
    }
}

/// The plugin dropdown with a live probe badge per entry: colored dot +
/// name, plus the failing plugin's notes inline (real probe detail on
/// hover). An inline Re-probe button schedules a fresh aggregation.
fn plugin_picker(ui: &mut Ui, current: &mut String, plugins: &[PluginStatus], needs_probe: &mut bool) {
    if plugins.is_empty() {
        ui.horizontal(|ui| {
            ui.weak("no plugins discovered — check CORPUS_PLUGINS_DIR");
            if ui.small_button("Re-probe").clicked() {
                *needs_probe = true;
            }
        });
        return;
    }
    egui::ComboBox::from_id_salt("plugin_picker")
        .selected_text(
            RichText::new(format!("{}  {current}", dot_marker(current, plugins)))
                .color(badge_color(current, plugins)),
        )
        .show_ui(ui, |ui| {
            for status in plugins {
                let color = badge_color(&status.name, plugins);
                ui.horizontal(|ui| {
                    ui.colored_label(color, "●");
                    let response =
                        ui.selectable_value(current, status.name.clone(), RichText::new(&status.name).color(color));
                    if !status.ready && !status.notes.is_empty() {
                        response.on_hover_text(&status.notes);
                        let short: String = status.notes.chars().take(48).collect();
                        ui.weak(format!("{short}"));
                    }
                });
            }
        });
}

/// The probe badge for a plugin label: ● when ready, ○ when not.
fn dot_marker(name: &str, plugins: &[PluginStatus]) -> &'static str {
    if plugins.iter().any(|p| p.name == name && p.ready) {
        "●"
    } else {
        "○"
    }
}

/// The badge color for a plugin: green when the live probe is ready,
/// amber otherwise (an unknown binding counts as not ready).
fn badge_color(name: &str, plugins: &[PluginStatus]) -> Color32 {
    if plugins.iter().any(|p| p.name == name && p.ready) {
        READY
    } else {
        NOT_READY
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
/// formatting for the created column — no date dependency needed.
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