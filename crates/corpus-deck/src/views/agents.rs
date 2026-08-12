//! Agents screen (deck-flow chunk 4): template authoring for all three
//! kinds — permission (a YAML block validated on save), prompt (markdown
//! with an egui_commonmark preview), agent (a composer that picks
//! permission + prompt + mode + model) — plus add-agent-to-
//! team from any agent template, and delete (core templates are
//! read-only; a project template shadows core by slug).
//!
//! No business logic here: corpus-core calls go through `DeckState`;
//! results surface as toasts.

use std::time::Duration;

use egui::{Align2, Color32, RichText, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_extras::{Column, TableBuilder};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use corpus_core::TemplateKind;

use crate::state::{DeckState, TemplateEntry};
use crate::views::model_picker::{ModelField, ModelPicker};

const GOOD: Color32 = Color32::from_rgb(120, 200, 120);
const BAD: Color32 = Color32::from_rgb(255, 90, 90);

/// The one editor buffer, reused across the three kinds (the editor
/// window knows which fields it edits).
#[derive(Default)]
struct EditorBuf {
    /// Some = editing an existing template; None = a new one (the slug
    /// is generated at save).
    slug: Option<String>,
    label: String,
    description: String,
    permission: String,
    prompt: String,
    mode: String,
    perm_ref: String,
    prompt_ref: String,
    model: String,
}

/// Widget state for the Agents screen: the project picker choice, the
/// template kind, the row selection, and the editor windows' fields.
pub struct AgentsView {
    project: Option<String>,
    kind: TemplateKind,
    selected: Option<String>,
    entries: Vec<TemplateEntry>,
    entries_project: Option<String>,
    entries_kind: Option<TemplateKind>,
    dirty: bool,
    /// Which kind's editor is open (None = none).
    editor: Option<TemplateKind>,
    buf: EditorBuf,
    /// Last inline permission-block validation result.
    validation: Option<Result<(), String>>,
    markdown_cache: CommonMarkCache,
    preview: bool,
    show_add: bool,
    add_template: Option<String>,
    add_team: Option<String>,
    add_name: String,
    add_model: String,
    /// The agent composer's model picker (chunk 8).
    composer_picker: ModelPicker,
    /// The add-agent-to-team window's model picker (chunk 8).
    add_picker: ModelPicker,
}

impl Default for AgentsView {
    fn default() -> Self {
        Self {
            project: None,
            kind: TemplateKind::Permission,
            selected: None,
            entries: Vec::new(),
            entries_project: None,
            entries_kind: None,
            dirty: true,
            editor: None,
            buf: EditorBuf::default(),
            validation: None,
            markdown_cache: CommonMarkCache::default(),
            preview: false,
            show_add: false,
            add_template: None,
            add_team: None,
            add_name: String::new(),
            add_model: String::new(),
            composer_picker: ModelPicker::default(),
            add_picker: ModelPicker::default(),
        }
    }
}

impl AgentsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts) {
        // Pick a project: the dropdown choice, or the first project.
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
        }

        // Keep the entry cache honest (project or kind changed, or
        // something was written/deleted since the last load).
        if self.entries_project.as_deref() != Some(chosen.as_str())
            || self.entries_kind != Some(self.kind)
            || self.dirty
        {
            self.entries = state.template_entries(&chosen, self.kind);
            self.entries_project = Some(chosen.clone());
            self.entries_kind = Some(self.kind);
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
                        let label = format!("{slug} — {}", project.name);
                        ui.selectable_value(&mut self.project, Some(slug.clone()), label);
                    }
                });
            ui.separator();
            ui.label("kind");
            egui::ComboBox::from_id_salt("agent_kind")
                .selected_text(kind_label(self.kind))
                .show_ui(ui, |ui| {
                    for kind in [
                        TemplateKind::Permission,
                        TemplateKind::Prompt,
                        TemplateKind::Agent,
                    ] {
                        if ui
                            .selectable_value(&mut self.kind, kind, kind_label(kind))
                            .clicked()
                        {
                            // A kind switch invalidates the selection and
                            // any open editor.
                            self.selected = None;
                            self.editor = None;
                            self.validation = None;
                            self.dirty = true;
                        }
                    }
                });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("+ New template").clicked() {
                self.open_editor(state, &chosen, None);
            }
            let has_selection = self.selected.is_some();
            let edit = ui
                .add_enabled(has_selection, egui::Button::new("Edit…"))
                .on_disabled_hover_text("select a template row first");
            if edit.clicked() {
                self.open_editor(state, &chosen, self.selected.clone());
            }
            // Core templates are read-only: delete only makes sense for
            // project-authored files.
            let project_row = self.entries.iter().any(|e| {
                self.selected.as_deref() == Some(e.slug.as_str()) && e.is_project
            });
            let delete = ui
                .add_enabled(project_row, egui::Button::new("Delete"))
                .on_disabled_hover_text(
                    "core templates are read-only — project templates only",
                );
            if delete.clicked() {
                self.delete_selected(state, toasts, &chosen);
            }
            let add = ui
                .add_enabled(
                    self.kind == TemplateKind::Agent && has_selection,
                    egui::Button::new("Add to team…"),
                )
                .on_disabled_hover_text("select an agent template first");
            if add.clicked() {
                self.prepare_add(state, &chosen);
            }
            if ui.button("Refresh").clicked() {
                self.dirty = true;
            }
            if let Some(slug) = &self.selected {
                ui.separator();
                ui.weak(format!("selected: {slug}"));
            }
        });
        ui.add_space(8.0);

        if self.entries.is_empty() {
            ui.add_space(40.0);
            ui.label(
                RichText::new(format!("No {} templates here yet — create one.", kind_label(self.kind)))
                    .weak()
                    .size(18.0),
            );
            ui.add_space(8.0);
        }

        self.template_table(ui);

        self.editor_window(ui, state, toasts, &chosen);
        self.add_window(ui, state, toasts, &chosen);
    }

    /// The striped template table; the columns follow the kind.
    fn template_table(&mut self, ui: &mut Ui) {
        let mut selected = self.selected.clone();
        let kind = self.kind;
        let entries = &self.entries;
        let mut build = |ui: &mut Ui, kind: TemplateKind| {
            match kind {
                TemplateKind::Permission | TemplateKind::Prompt => TableBuilder::new(ui)
                    .striped(true)
                    .column(Column::remainder().at_least(140.0))
                    .column(Column::auto().at_least(120.0))
                    .column(Column::auto().at_least(70.0))
                    .column(Column::auto().at_least(220.0))
                    .header(24.0, |mut header| {
                        header.col(|ui| {
                            ui.strong("name");
                        });
                        header.col(|ui| {
                            ui.strong("slug");
                        });
                        header.col(|ui| {
                            ui.strong("origin");
                        });
                        header.col(|ui| {
                            ui.strong("description");
                        });
                    })
                    .body(|mut body| {
                        for entry in entries {
                            let is_sel = selected.as_deref() == Some(entry.slug.as_str());
                            body.row(24.0, |mut row| {
                                row.col(|ui| {
                                    if ui.selectable_label(is_sel, &entry.name).clicked() {
                                        selected = Some(entry.slug.clone());
                                    }
                                });
                                row.col(|ui| {
                                    ui.monospace(&entry.slug);
                                });
                                row.col(|ui| {
                                    origin_label(ui, entry);
                                });
                                row.col(|ui| {
                                    ui.weak(&entry.description);
                                });
                            });
                        }
                    }),
                TemplateKind::Agent => TableBuilder::new(ui)
                    .striped(true)
                    .column(Column::remainder().at_least(140.0))
                    .column(Column::auto().at_least(110.0))
                    .column(Column::auto().at_least(70.0))
                    .column(Column::auto().at_least(90.0))
                    .column(Column::auto().at_least(110.0))
                    .column(Column::auto().at_least(110.0))
                    .column(Column::auto().at_least(110.0))
                    .header(24.0, |mut header| {
                        header.col(|ui| {
                            ui.strong("name");
                        });
                        header.col(|ui| {
                            ui.strong("slug");
                        });
                        header.col(|ui| {
                            ui.strong("origin");
                        });
                        header.col(|ui| {
                            ui.strong("mode");
                        });
                        header.col(|ui| {
                            ui.strong("permission");
                        });
                        header.col(|ui| {
                            ui.strong("prompt");
                        });
                        header.col(|ui| {
                            ui.strong("model");
                        });
                    })
                    .body(|mut body| {
                        for entry in entries {
                            let is_sel = selected.as_deref() == Some(entry.slug.as_str());
                            body.row(24.0, |mut row| {
                                row.col(|ui| {
                                    if ui.selectable_label(is_sel, &entry.name).clicked() {
                                        selected = Some(entry.slug.clone());
                                    }
                                });
                                row.col(|ui| {
                                    ui.monospace(&entry.slug);
                                });
                                row.col(|ui| {
                                    origin_label(ui, entry);
                                });
                                row.col(|ui| {
                                    ui.weak(entry.mode.as_deref().unwrap_or("-"));
                                });
                                row.col(|ui| {
                                    ui.monospace(entry.permission_ref.as_deref().unwrap_or("-"));
                                });
                                row.col(|ui| {
                                    ui.monospace(entry.prompt_ref.as_deref().unwrap_or("-"));
                                });
                                row.col(|ui| {
                                    ui.weak(entry.model.as_deref().unwrap_or("-"));
                                });
                            });
                        }
                    }),
            }
        };
        build(ui, kind);
        self.selected = selected;
    }

    /// Open the editor for the current kind (None = blank new template).
    fn open_editor(&mut self, state: &DeckState, project: &str, slug: Option<String>) {
        self.editor = Some(self.kind);
        self.validation = None;
        let mut buf = EditorBuf {
            slug: slug.clone(),
            ..Default::default()
        };
        if let Some(slug) = &slug {
            match self.kind {
                TemplateKind::Permission => {
                    if let Ok(template) = state.load_permission(project, slug) {
                        buf.label = template.name;
                        buf.description = template.description;
                        buf.permission = template.permission;
                    }
                }
                TemplateKind::Prompt => {
                    if let Ok(template) = state.load_prompt(project, slug) {
                        buf.label = template.name;
                        buf.description = template.description;
                        buf.prompt = template.body;
                    }
                }
                TemplateKind::Agent => {
                    if let Ok(template) = state.load_agent(project, slug) {
                        buf.label = template.name;
                        buf.description = template.description;
                        buf.mode = template.mode;
                        buf.perm_ref = template.permission_ref;
                        buf.prompt_ref = template.prompt_ref;
                        buf.model = template.model.unwrap_or_default();
                    }
                }
            }
        } else {
            buf.mode = "primary".to_string();
            buf.prompt = String::from("You are a corpus agent.\n");
        }
        self.buf = buf;
    }

    /// The editor window for the current kind.
    fn editor_window(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(kind) = self.editor else {
            return;
        };
        let is_new = self.buf.slug.is_none();
        // A core-originated template being edited saves as a project
        // shadow; anything else overwrites the project file.
        let shadows_core = self
            .buf
            .slug
            .as_ref()
            .map(|slug| !self.entries.iter().any(|e| &e.slug == slug && e.is_project))
            .unwrap_or(false);
        let title = match (is_new, kind) {
            (true, k) => format!("New {} template", kind_label(k)),
            (false, k) => format!("Edit {} template", kind_label(k)),
        };
        let mut open = true;
        let mut saved = false;
        let mut cancel = false;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 520.0])
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
            .show(ui.ctx(), |ui| {
                if !is_new {
                    ui.horizontal(|ui| {
                        ui.weak(format!("id: {}", self.buf.slug.as_deref().unwrap_or("")));
                        if shadows_core {
                            ui.colored_label(
                                Color32::from_rgb(255, 180, 90),
                                "shadows a core template",
                            );
                        }
                    });
                }
                ui.label("Name (the human label)");
                ui.text_edit_singleline(&mut self.buf.label);
                ui.label("Description (optional)");
                ui.text_edit_singleline(&mut self.buf.description);
                match kind {
                    TemplateKind::Permission => self.permission_fields(ui),
                    TemplateKind::Prompt => self.prompt_fields(ui),
                    TemplateKind::Agent => self.agent_fields(ui, state, project),
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let save_label = if is_new {
                        "Save template"
                    } else if shadows_core {
                        "Save as project template (shadows core)"
                    } else {
                        "Save changes"
                    };
                    let save = ui.button(save_label);
                    if save.clicked() {
                        match self.save(state, project) {
                            Ok(()) => {
                                toast(
                                    toasts,
                                    ToastKind::Success,
                                    format!(
                                        "saved {} template {}",
                                        kind_label(kind),
                                        self.buf.label
                                    ),
                                );
                                self.dirty = true;
                                saved = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel || !open {
            self.editor = None;
        }
        if saved {
            self.editor = None;
        }
    }

    /// Permission editor fields: the YAML block, validated inline and
    /// again at save (a malformed block is never written).
    fn permission_fields(&mut self, ui: &mut Ui) {
        ui.label("Permission block (YAML — validated on save)");
        ui.add(
            egui::TextEdit::multiline(&mut self.buf.permission)
                .code_editor()
                .desired_rows(12),
        );
        ui.horizontal(|ui| {
            if ui.button("Validate block").clicked() {
                self.validation =
                    Some(corpus_core::validate_permission_block(&self.buf.permission)
                        .map_err(|e| e.to_string()));
            }
            match &self.validation {
                Some(Ok(())) => {
                    ui.colored_label(GOOD, "valid YAML — saveable");
                }
                Some(Err(error)) => {
                    ui.colored_label(BAD, error);
                }
                None => {}
            }
        });
    }

    /// Prompt editor fields: the markdown body plus a rendered preview.
    fn prompt_fields(&mut self, ui: &mut Ui) {
        ui.checkbox(&mut self.preview, "rendered preview");
        if self.preview {
            ui.columns(2, |columns| {
                let (left, right) = columns.split_at_mut(1);
                let left = &mut left[0];
                let right = &mut right[0];
                left.label("Markdown body");
                left.add(
                    egui::TextEdit::multiline(&mut self.buf.prompt)
                        .code_editor()
                        .desired_rows(16),
                );
                right.label("Preview");
                egui::ScrollArea::vertical().max_height(400.0).show(right, |ui| {
                    CommonMarkViewer::new().show(ui, &mut self.markdown_cache, &self.buf.prompt);
                });
            });
        } else {
            ui.label("Markdown body");
            ui.add(
                egui::TextEdit::multiline(&mut self.buf.prompt)
                    .code_editor()
                    .desired_rows(16),
            );
        }
    }

    /// Agent composer fields: mode + resolved ref picks + model picker.
    fn agent_fields(&mut self, ui: &mut Ui, state: &mut DeckState, project: &str) {
        let permissions = state.template_entries(project, TemplateKind::Permission);
        let prompts = state.template_entries(project, TemplateKind::Prompt);
        state.ensure_models();
        ui.horizontal(|ui| {
            ui.label("Mode");
            egui::ComboBox::from_id_salt("agent_mode")
                .selected_text(if self.buf.mode.is_empty() {
                    "—".to_string()
                } else {
                    self.buf.mode.clone()
                })
                .show_ui(ui, |ui| {
                    for mode in ["primary", "subagent", "all"] {
                        ui.selectable_value(&mut self.buf.mode, mode.to_string(), mode);
                    }
                });
        });
        ui.label("Permission template (project-then-core)");
        ref_picker(
            ui,
            "agent_perm_ref",
            &mut self.buf.perm_ref,
            &permissions,
        );
        ui.label("Prompt template (project-then-core)");
        ref_picker(ui, "agent_prompt_ref", &mut self.buf.prompt_ref, &prompts);
        ui.label("Model (optional — empty = decide at launch)");
        ui.horizontal(|ui| {
            self.composer_picker.field(
                ui,
                "agent_template_model",
                &mut self.buf.model,
                ModelField {
                    models: state.models(),
                    badges: state.benchmarked_ids(),
                    degrade_note: state.models_error(),
                    allow_none: true,
                },
            );
            if state.models_loading() {
                ui.spinner();
            } else if ui
                .button("↻")
                .on_hover_text("refresh the model list from opencode")
                .clicked()
            {
                state.refresh_models(true);
            }
        });
    }

    /// Save the editor buffer for its kind. Returns a user-facing error
    /// on anything the write refused (invalid permission YAML, dangling
    /// refs, empty prompt, empty label).
    fn save(&mut self, state: &mut DeckState, project: &str) -> Result<(), String> {
        let label = self.buf.label.trim();
        if label.is_empty() {
            return Err("template name is required".to_string());
        }
        let slug = self
            .buf
            .slug
            .clone()
            .unwrap_or_else(DeckState::fresh_id);
        match self.kind {
            TemplateKind::Permission => state
                .write_permission(
                    project,
                    &slug,
                    &corpus_core::PermissionTemplate {
                        name: label.to_string(),
                        description: self.buf.description.trim().to_string(),
                        permission: self.buf.permission.clone(),
                    },
                )
                .map_err(|e| e.to_string()),
            TemplateKind::Prompt => state
                .write_prompt(
                    project,
                    &slug,
                    &corpus_core::PromptTemplate {
                        name: label.to_string(),
                        description: self.buf.description.trim().to_string(),
                        body: self.buf.prompt.clone(),
                    },
                )
                .map_err(|e| e.to_string()),
            TemplateKind::Agent => state
                .write_agent(
                    project,
                    &slug,
                    &corpus_core::AgentTemplate {
                        name: label.to_string(),
                        description: self.buf.description.trim().to_string(),
                        mode: self.buf.mode.clone(),
                        permission_ref: self.buf.perm_ref.clone(),
                        prompt_ref: self.buf.prompt_ref.clone(),
                        model: Some(self.buf.model.trim().to_string())
                            .filter(|m| !m.is_empty()),
                    },
                )
                .map_err(|e| e.to_string()),
        }
    }

    /// Delete the selected project template (core slugs refuse).
    fn delete_selected(&mut self, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(slug) = self.selected.clone() else {
            return;
        };
        match state.delete_template(project, self.kind, &slug) {
            Ok(()) => {
                toast(
                    toasts,
                    ToastKind::Success,
                    format!("deleted {kind} template {slug}", kind = kind_label(self.kind)),
                );
                self.selected = None;
                self.dirty = true;
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// Prepare the add-agent-to-team window for the selected template.
    fn prepare_add(&mut self, state: &mut DeckState, project: &str) {
        let Some(slug) = self.selected.clone() else {
            return;
        };
        let name = self
            .entries
            .iter()
            .find(|e| e.slug == slug)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| slug.clone());
        state.refresh_teams(project);
        state.ensure_models();
        self.add_template = Some(slug);
        self.add_team = state.teams.first().map(|(slug, _)| slug.clone());
        self.add_name = name;
        self.add_model.clear();
        self.show_add = true;
    }

    /// The add-agent-to-team window: team pick, agent name, model.
    fn add_window(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(template) = self.add_template.clone() else {
            return;
        };
        let mut open = self.show_add;
        let mut added = false;
        let mut cancel = false;
        egui::Window::new(format!("Add agent to team — {template}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Team");
                egui::ComboBox::from_id_salt("add_team")
                    .selected_text(self.add_team.clone().unwrap_or_else(|| "—".to_string()))
                    .show_ui(ui, |ui| {
                        for (slug, spec) in &state.teams {
                            let label = format!("{slug} — {}", spec.name);
                            ui.selectable_value(&mut self.add_team, Some(slug.clone()), label);
                        }
                    });
                ui.label("Agent name (the key on the team spec)");
                ui.text_edit_singleline(&mut self.add_name);
                ui.label("Model (optional — empty = the template default)");
                ui.horizontal(|ui| {
                    self.add_picker.field(
                        ui,
                        "add_agent_model",
                        &mut self.add_model,
                        ModelField {
                            models: state.models(),
                            badges: state.benchmarked_ids(),
                            degrade_note: state.models_error(),
                            allow_none: true,
                        },
                    );
                    if state.models_loading() {
                        ui.spinner();
                    } else if ui
                        .button("↻")
                        .on_hover_text("refresh the model list from opencode")
                        .clicked()
                    {
                        state.refresh_models(true);
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Add").clicked() {
                        match self.add_team.clone() {
                            None => toast(toasts, ToastKind::Warning, "pick a team first"),
                            Some(team) => {
                                let model = if self.add_model.trim().is_empty() {
                                    None
                                } else {
                                    Some(self.add_model.trim())
                                };
                                match state.add_agent_to_team(
                                    project,
                                    &team,
                                    &self.add_name,
                                    &template,
                                    model,
                                ) {
                                    Ok(()) => {
                                        toast(
                                            toasts,
                                            ToastKind::Success,
                                            format!(
                                                "added agent {} to team {project}/{team}",
                                                self.add_name
                                            ),
                                        );
                                        added = true;
                                    }
                                    Err(error) => {
                                        toast(toasts, ToastKind::Error, error.to_string())
                                    }
                                }
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel || !open {
            self.show_add = false;
        }
        if added {
            self.show_add = false;
        }
    }
}

/// A ref-picker combo over template entries (project-then-core union).
fn ref_picker(ui: &mut Ui, id: &str, current: &mut String, entries: &[TemplateEntry]) {
    let selected_text = entries
        .iter()
        .find(|e| &e.slug == current)
        .map(|e| format!("{} · {}", e.name, e.slug))
        .unwrap_or_else(|| "— pick a template —".to_string());
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            for entry in entries {
                let label = format!("{} · {}", entry.name, entry.slug);
                ui.selectable_value(current, entry.slug.clone(), label);
            }
        });
}

/// The origin cell: project (with a shadow marker) or core.
fn origin_label(ui: &mut Ui, entry: &TemplateEntry) {
    if entry.is_project {
        if entry.shadows_core {
            ui.colored_label(Color32::from_rgb(255, 180, 90), "project*");
        } else {
            ui.weak("project");
        }
    } else {
        ui.weak("core");
    }
}

fn kind_label(kind: TemplateKind) -> &'static str {
    match kind {
        TemplateKind::Permission => "permissions",
        TemplateKind::Prompt => "prompts",
        TemplateKind::Agent => "agents",
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