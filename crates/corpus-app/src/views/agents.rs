//! Agents screen: the selected project's agents — list, raw JSON editor
//! with core-side validation on save, clone, delete.

use std::time::Duration;

use egui::{Align2, Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::AppState;

const GOOD: Color32 = Color32::from_rgb(120, 200, 120);
const BAD: Color32 = Color32::from_rgb(255, 90, 90);

/// Widget state for the Agents screen.
pub struct AgentsView {
    project: Option<String>,
    viewed_project: Option<String>,
    selected: Option<String>,
    dirty: bool,
    show_editor: bool,
    /// Raw JSON text being edited (displayed as code).
    editor_text: String,
    /// Last validation result from corpus-core.
    validation: Option<Result<(), String>>,
}

impl Default for AgentsView {
    fn default() -> Self {
        Self {
            project: None,
            viewed_project: None,
            selected: None,
            dirty: true,
            show_editor: false,
            editor_text: String::new(),
            validation: None,
        }
    }
}

impl AgentsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let chosen = match &self.project {
            Some(project) if state.projects.iter().any(|(slug, _)| slug == project) => {
                project.clone()
            }
            _ => match state.projects.first() {
                Some((slug, _)) => slug.clone(),
                None => {
                    ui.add_space(24.0);
                    ui.weak("no projects yet — create one on the Projects screen");
                    return;
                }
            },
        };
        if self.project != Some(chosen.clone()) {
            self.project = Some(chosen.clone());
            self.dirty = true;
        }
        if self.viewed_project.as_deref() != Some(chosen.as_str()) || self.dirty {
            state.refresh_agents(&chosen);
            self.viewed_project = Some(chosen.clone());
            self.dirty = false;
        }

        ui.horizontal(|ui| {
            ui.heading("Agents");
            ui.add_space(8.0);
            ui.label("project");
            egui::ComboBox::from_id_salt("agent_project")
                .selected_text(match state.projects.iter().find(|(s, _)| *s == chosen) {
                    Some((slug, project)) => format!("{slug} — {}", project.name),
                    None => chosen.clone(),
                })
                .show_ui(ui, |ui| {
                    for (slug, project) in &state.projects {
                        ui.selectable_value(&mut self.project, Some(slug.clone()), format!("{slug} — {}", project.name));
                    }
                });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("+ New agent").clicked() {
                // Create blank, then open editor.
                match state.create_blank_agent(&chosen) {
                    Ok((slug, _)) => {
                        toast(toasts, ToastKind::Success, format!("created agent {chosen}/{slug}"));
                        self.dirty = true;
                        self.selected = Some(slug);
                    }
                    Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                }
            }
            let has_selection = self.selected.is_some();
            let edit = ui
                .add_enabled(has_selection, egui::Button::new("Edit…"))
                .on_disabled_hover_text("select an agent row first");
            if edit.clicked() {
                self.open_editor(state, &chosen);
            }
            let clone = ui
                .add_enabled(has_selection, egui::Button::new("Clone…"))
                .on_disabled_hover_text("select an agent row first");
            if clone.clicked() {
                if let Some(slug) = &self.selected {
                    match state.clone_agent(&chosen, slug) {
                        Ok(()) => {
                            toast(toasts, ToastKind::Success, "agent cloned");
                            self.dirty = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            }
            let delete = ui
                .add_enabled(has_selection, egui::Button::new("Delete"))
                .on_disabled_hover_text("select an agent row first");
            if delete.clicked() {
                if let Some(slug) = self.selected.clone() {
                    match state.delete_agent(&chosen, &slug) {
                        Ok(()) => {
                            toast(toasts, ToastKind::Success, format!("deleted agent {chosen}/{slug}"));
                            self.selected = None;
                            self.dirty = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            }
            if ui.button("Refresh").clicked() {
                self.dirty = true;
            }
        });
        ui.add_space(8.0);

        if state.agents.is_empty() {
            ui.add_space(40.0);
            ui.label(RichText::new("No agents yet — create one.").weak().size(18.0));
            ui.add_space(8.0);
        }

        self.agent_table(ui, state, &chosen);

        self.editor_window(ui, state, toasts, &chosen);
    }

    fn agent_table(&mut self, ui: &mut Ui, state: &mut AppState, project: &str) {
        let mut selected = self.selected.clone();
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::remainder().at_least(100.0))
            .column(Column::auto().at_least(80.0))
            .column(Column::auto().at_least(60.0))
            .header(24.0, |mut header| {
                header.col(|ui| { ui.strong("slug"); });
                header.col(|ui| { ui.strong("name"); });
                header.col(|ui| { ui.strong("hash"); });
            })
            .body(|mut body| {
                for (slug, agent) in &state.agents {
                    let is_sel = selected.as_deref() == Some(slug.as_str());
                    body.row(24.0, |mut row| {
                        row.col(|ui| {
                            if ui.selectable_label(is_sel, RichText::new(slug).monospace()).clicked() {
                                selected = Some(slug.clone());
                            }
                        });
                        row.col(|ui| { ui.label(&agent.meta.name); });
                        row.col(|ui| {
                            ui.weak(&state.agent_config_hash(project, slug)[..12]);
                        });
                    });
                }
            });
        self.selected = selected;
    }

    fn open_editor(&mut self, state: &AppState, project: &str) {
        let Some(slug) = &self.selected else { return };
        if let Ok(agent) = state.load_agent(project, slug) {
            self.editor_text = serde_json::to_string_pretty(&agent.doc).unwrap_or_default();
            self.show_editor = true;
            self.validation = None;
        }
    }

    fn editor_window(&mut self, ui: &mut Ui, state: &mut AppState, _toasts: &mut Toasts, project: &str) {
        if !self.show_editor { return; }
        let Some(slug) = self.selected.clone() else {
            self.show_editor = false;
            return;
        };
        let mut open = true;
        let mut saved = false;
        let mut cancel = false;
        egui::Window::new(format!("Edit agent: {project}/{slug}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 480.0])
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.weak(format!("{project}/{slug} — raw opencode.json"));
                });
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::multiline(&mut self.editor_text)
                        .code_editor()
                        .desired_rows(20),
                );
                ui.horizontal(|ui| {
                    if ui.button("Validate").clicked() {
                        self.validation = Some(match serde_json::from_str::<serde_json::Value>(&self.editor_text) {
                            Ok(doc) => match state.save_agent(project, &slug, &doc) {
                                Ok(()) => Ok(()),
                                Err(e) => Err(e.to_string()),
                            },
                            Err(e) => Err(format!("invalid JSON: {e}")),
                        });
                    }
                    match &self.validation {
                        Some(Ok(())) => { ui.colored_label(GOOD, "valid — and saved"); }
                        Some(Err(e)) => { ui.colored_label(BAD, e); }
                        None => {}
                    }
                    if let Some(Ok(())) = self.validation {
                        if ui.button("Close").clicked() {
                            self.dirty = true;
                            saved = true;
                        }
                    }
                });
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        if cancel || !open {
            self.show_editor = false;
        }
        if saved {
            self.show_editor = false;
        }
    }
}

fn toast(toasts: &mut Toasts, kind: ToastKind, text: impl Into<String>) {
    toasts.add(
        Toast::new()
            .kind(kind)
            .text(text.into())
            .options(ToastOptions::default().duration(Duration::from_secs(4))),
    );
}