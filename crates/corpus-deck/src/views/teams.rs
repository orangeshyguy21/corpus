//! Teams screen (deck-flow chunk 3): the selected project's teams with
//! their corpus generations, plus the full lifecycle — create (label,
//! agents defaulting to the core pair, optional pinned-rev override),
//! edit (label, rev, agent rows), clone (spec + corpus snapshot),
//! delete, and wipe (destructive: confirm dialog, then the generation
//! bumps and the clone stays untouched).
//!
//! No business logic here: corpus-core calls go through `DeckState`;
//! results surface as toasts.

use std::time::Duration;

use egui::{Align2, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::nav::Screen;
use crate::state::{AgentRow, DeckState};

/// The probe-level mission a fresh Launch dialog suggests (the chunk-5
/// acceptance runs one like this).
const PROBE_MISSION: &str = "Probe the environment: report the plugin probe status, the sandbox targets, and the available tools. Do not attack anything; map the surfaces and cite what you observe.";

/// Widget state for the Teams screen: the project picker choice, the
/// row selection, and the form fields. Corpus state lives in
/// `DeckState`.
pub struct TeamsView {
    /// The project whose teams this screen shows (picked from the
    /// dropdown; defaults to the first project).
    project: Option<String>,
    /// The project `state.teams` was loaded for; a mismatch means the
    /// cache is stale and must be reloaded.
    viewed_project: Option<String>,
    selected: Option<String>,
    show_create: bool,
    create_label: String,
    create_rev: String,
    create_budget: String,
    show_edit: bool,
    edit_label: String,
    edit_rev: String,
    edit_budget: String,
    edit_agents: Vec<AgentRow>,
    show_clone: bool,
    show_wipe: bool,
    show_launch: bool,
    launch_agent: Option<String>,
    launch_model: String,
    launch_mission: String,
}

impl Default for TeamsView {
    fn default() -> Self {
        Self {
            project: None,
            viewed_project: None,
            selected: None,
            show_create: false,
            create_label: String::new(),
            create_rev: String::new(),
            create_budget: String::new(),
            show_edit: false,
            edit_label: String::new(),
            edit_rev: String::new(),
            edit_budget: String::new(),
            edit_agents: Vec::new(),
            show_clone: false,
            show_wipe: false,
            show_launch: false,
            launch_agent: None,
            launch_model: String::new(),
            launch_mission: PROBE_MISSION.to_string(),
        }
    }
}

impl TeamsView {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        state: &mut DeckState,
        toasts: &mut Toasts,
        nav: &mut Option<Screen>,
    ) {
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
        // Keep the team cache honest.
        if self.viewed_project.as_deref() != Some(chosen.as_str()) {
            state.refresh_teams(&chosen);
            self.viewed_project = Some(chosen.clone());
        }

        ui.horizontal(|ui| {
            ui.heading("Teams");
            ui.add_space(8.0);
            ui.label("project");
            egui::ComboBox::from_id_salt("team_project")
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
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("+ New team").clicked() {
                self.create_label.clear();
                self.create_rev.clear();
                self.create_budget.clear();
                self.show_create = true;
            }
            let has_selection = self.selected.is_some();
            let edit = ui
                .add_enabled(has_selection, egui::Button::new("Edit…"))
                .on_disabled_hover_text("select a team row first");
            if edit.clicked() {
                self.load_edit_form(state);
                self.show_edit = true;
            }
            let clone = ui
                .add_enabled(has_selection, egui::Button::new("Clone…"))
                .on_disabled_hover_text("select a team row first");
            if clone.clicked() {
                self.show_clone = true;
            }
            let delete = ui
                .add_enabled(has_selection, egui::Button::new("Delete"))
                .on_disabled_hover_text("select a team row first");
            if delete.clicked() {
                self.delete_selected(state, toasts, &chosen);
            }
            let wipe = ui
                .add_enabled(has_selection, egui::Button::new("Wipe corpus…"))
                .on_disabled_hover_text("select a team row first");
            if wipe.clicked() {
                self.show_wipe = true;
            }
            let launch = ui
                .add_enabled(has_selection, egui::Button::new("Launch…"))
                .on_disabled_hover_text("select a team row first");
            if launch.clicked() {
                self.arm_launch(state);
            }
            if ui.button("Refresh").clicked() {
                state.refresh();
                state.refresh_teams(&chosen);
            }
            if let Some(slug) = &self.selected {
                ui.separator();
                ui.weak(format!("selected: {slug}"));
            }
        });
        ui.add_space(8.0);

        if state.teams.is_empty() {
            ui.add_space(40.0);
            ui.label(
                RichText::new("No teams yet — create your first team.")
                    .weak()
                    .size(18.0),
            );
            ui.add_space(8.0);
        }

        self.team_table(ui, state);

        self.create_window(ui, state, toasts, &chosen);
        self.edit_window(ui, state, toasts, &chosen);
        self.clone_window(ui, state, toasts, &chosen);
        self.wipe_window(ui, state, toasts, &chosen);
        self.launch_window(ui, state, toasts, &chosen, nav);
    }

    /// The striped team table; clicking a row selects it.
    fn team_table(&mut self, ui: &mut Ui, state: &mut DeckState) {
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::auto().at_least(140.0))
            .column(Column::remainder().at_least(120.0))
            .column(Column::auto().at_least(50.0))
            .column(Column::auto().at_least(180.0))
            .column(Column::auto().at_least(110.0))
            .column(Column::auto().at_least(100.0))
            .column(Column::auto().at_least(90.0))
            .header(24.0, |mut header| {
                header.col(|ui| {
                    ui.strong("id");
                });
                header.col(|ui| {
                    ui.strong("label");
                });
                header.col(|ui| {
                    ui.strong("gen");
                });
                header.col(|ui| {
                    ui.strong("agents");
                });
                header.col(|ui| {
                    ui.strong("rev override");
                });
                header.col(|ui| {
                    ui.strong("cloned from");
                });
            })
            .body(|mut body| {
                let local_teams = &state.teams;
                for (slug, spec) in local_teams {
                    let selected = self.selected.as_deref() == Some(slug.as_str());
                    body.row(24.0, |mut row| {
                        row.col(|ui| {
                            if ui
                                .selectable_label(selected, RichText::new(slug).monospace())
                                .clicked()
                            {
                                self.selected = Some(slug.clone());
                            }
                        });
                        row.col(|ui| {
                            ui.label(&spec.name);
                        });
                        row.col(|ui| {
                            ui.monospace(spec.corpus_generation.to_string());
                        });
                        row.col(|ui| {
                            let agents = spec
                                .agents
                                .iter()
                                .map(|(name, instance)| format!("{name}:{}", instance.template))
                                .collect::<Vec<_>>()
                                .join(", ");
                            ui.weak(agents);
                        });
                        row.col(|ui| {
                            ui.weak(spec.rev_override.as_deref().unwrap_or("—"));
                        });
                        row.col(|ui| {
                            ui.weak(spec.budget.as_deref().unwrap_or("—"));
                        });
                        row.col(|ui| {
                            if let Some(from) = &spec.cloned_from {
                                ui.weak(from);
                            }
                        });
                    });
                }
            });
    }

    /// The create form: label + optional pinned-rev override. Agents
    /// default to the core pair (operator + researcher); rows are for
    /// Edit.
    fn create_window(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let mut open = self.show_create;
        let mut created = false;
        egui::Window::new(format!("New team — {project}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Label (the human name — the id is generated)");
                ui.text_edit_singleline(&mut self.create_label);
                ui.label("Pinned rev override (optional — empty = the plugin's pin)");
                ui.text_edit_singleline(&mut self.create_rev);
                ui.label("Budget (optional — the whole team's execution budget, e.g. 40m / 10k$)");
                ui.text_edit_singleline(&mut self.create_budget);
                ui.add_space(4.0);
                ui.weak("Agents: operator (operator), researcher (researcher) — the core pair; customize later via Edit.");
                ui.add_space(8.0);
                if ui.button("Create team").clicked() {
                    let label = self.create_label.trim();
                    if label.is_empty() {
                        toast(toasts, ToastKind::Warning, "team label is required");
                    } else {
                        let rev = if self.create_rev.trim().is_empty() {
                            None
                        } else {
                            Some(self.create_rev.trim())
                        };
                        let budget = if self.create_budget.trim().is_empty() {
                            None
                        } else {
                            Some(self.create_budget.trim())
                        };
                        match state.create_team(project, label, rev, budget, &[]) {
                            Ok((id, spec)) => {
                                toast(
                                    toasts,
                                    ToastKind::Success,
                                    format!(
                                        "created team {project}/{id} ({} agents, generation {})",
                                        spec.agents.len(),
                                        spec.corpus_generation
                                    ),
                                );
                                state.refresh_teams(project);
                                self.create_label.clear();
                                self.create_rev.clear();
                                self.create_budget.clear();
                                created = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                        }
                    }
                }
            });
        self.show_create = open && !created;
    }

    /// The edit form: label, rev override, and the agent rows (exact —
    /// whatever is listed replaces the current set, drops stay dropped).
    fn edit_window(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(slug) = self.selected.clone() else {
            self.show_edit = false;
            return;
        };
        let mut open = self.show_edit;
        let mut saved = false;
        egui::Window::new(format!("Edit team: {project}/{slug}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Label");
                ui.text_edit_singleline(&mut self.edit_label);
                ui.label("Pinned rev override (empty = the plugin's pin)");
                ui.text_edit_singleline(&mut self.edit_rev);
                ui.label("Budget (empty = no cap on the team's execution)");
                ui.text_edit_singleline(&mut self.edit_budget);
                ui.add_space(4.0);
                ui.label("Agents (name · template · model — rows replace the set exactly)");
                let mut remove: Option<usize> = None;
                for (index, row) in self.edit_agents.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut row.name).desired_width(110.0),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut row.template).desired_width(110.0),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut row.model)
                                .desired_width(110.0)
                                .hint_text("model (optional)"),
                        );
                        if ui.button("✕").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    self.edit_agents.remove(index);
                }
                if ui.button("+ Add agent").clicked() {
                    self.edit_agents.push(AgentRow::default());
                }
                ui.add_space(8.0);
                if ui.button("Save").clicked() {
                    match crate::state::parse_agent_rows(&self.edit_agents) {
                        Ok(agents) => {
                            let label = self.edit_label.trim().to_string();
                            let rev = if self.edit_rev.trim().is_empty() {
                                None
                            } else {
                                Some(self.edit_rev.trim().to_string())
                            };
                            let budget = if self.edit_budget.trim().is_empty() {
                                None
                            } else {
                                Some(self.edit_budget.trim().to_string())
                            };
                            match state.update_team(project, &slug, |spec| {
                                spec.name = label;
                                spec.rev_override = rev;
                                spec.budget = budget;
                                spec.agents = agents;
                                Ok(())
                            }) {
                                Ok(_) => {
                                    toast(
                                        toasts,
                                        ToastKind::Success,
                                        format!("updated team {project}/{slug}"),
                                    );
                                    state.refresh_teams(project);
                                    saved = true;
                                }
                                Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                            }
                        }
                        Err(error) => {
                            toast(toasts, ToastKind::Warning, error.to_string());
                        }
                    }
                }
            });
        self.show_edit = open && !saved;
    }

    /// The clone form: a fresh id, the source's spec + full corpus, and
    /// a generation snapshot — one click.
    fn clone_window(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(from) = self.selected.clone() else {
            self.show_clone = false;
            return;
        };
        let mut open = self.show_clone;
        let mut cloned = false;
        egui::Window::new(format!("Clone team: {project}/{from}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.weak("Copies the team spec and its full corpus; the clone keeps the source's corpus_generation (a snapshot) — wiping the source later leaves the clone untouched.");
                ui.add_space(8.0);
                if ui.button("Clone").clicked() {
                    match state.clone_team(project, &from) {
                        Ok((to, spec)) => {
                            toast(
                                toasts,
                                ToastKind::Success,
                                format!(
                                    "cloned team {project}/{from} -> {to} (generation {})",
                                    spec.corpus_generation
                                ),
                            );
                            state.refresh_teams(project);
                            cloned = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        self.show_clone = open && !cloned;
    }

    /// The wipe confirm: destructive by design, so there is a dialog.
    fn wipe_window(&mut self, ui: &mut Ui, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(slug) = self.selected.clone() else {
            self.show_wipe = false;
            return;
        };
        let mut open = self.show_wipe;
        let mut wiped = false;
        let mut cancelled = false;
        egui::Window::new(format!("Wipe team corpus: {project}/{slug}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label(
                    RichText::new(
                        "Deletes the team's working corpus subtree (hypotheses, runs, techniques, findings, attacks).",
                    )
                    .color(egui::Color32::from_rgb(255, 180, 90)),
                );
                ui.weak("The team spec stays; corpus_generation bumps so old run logs remain attributable.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Wipe corpus").color(egui::Color32::from_rgb(255, 120, 90)))
                        .clicked()
                    {
                        match state.wipe_team_corpus(project, &slug) {
                            Ok(spec) => {
                                toast(
                                    toasts,
                                    ToastKind::Success,
                                    format!(
                                        "wiped team corpus {project}/{slug} (generation {})",
                                        spec.corpus_generation
                                    ),
                                );
                                state.refresh_teams(project);
                                wiped = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        self.show_wipe = open && !wiped && !cancelled;
    }

    /// Delete the selected team (spec + corpus subtree). Destructive;
    /// the plan gates only wipe behind a dialog.
    fn delete_selected(&mut self, state: &mut DeckState, toasts: &mut Toasts, project: &str) {
        let Some(slug) = self.selected.clone() else {
            return;
        };
        match state.delete_team(project, &slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, format!("deleted team {project}/{slug}"));
                self.selected = None;
                state.refresh_teams(project);
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// Populate the launch dialog from the selected team (agent pick
    /// defaults to the first agent on the spec; mission suggests a
    /// probe-level run).
    fn arm_launch(&mut self, state: &mut DeckState) {
        let Some(slug) = self.selected.clone() else {
            return;
        };
        let agents: Vec<String> = state
            .teams
            .iter()
            .find(|(s, _)| *s == slug)
            .map(|(_, spec)| spec.agents.keys().cloned().collect())
            .unwrap_or_default();
        self.launch_agent = agents.first().cloned();
        self.launch_model = state.suggested_model().unwrap_or_default();
        self.launch_mission = PROBE_MISSION.to_string();
        self.show_launch = !agents.is_empty();
    }

    /// The launch dialog: agent pick + optional model + the mission.
    /// On success the operator lands on the Launch screen's run view.
    fn launch_window(
        &mut self,
        ui: &mut Ui,
        state: &mut DeckState,
        toasts: &mut Toasts,
        project: &str,
        nav: &mut Option<Screen>,
    ) {
        let Some(team) = self.selected.clone() else {
            self.show_launch = false;
            return;
        };
        let agents: Vec<(String, String)> = state
            .teams
            .iter()
            .find(|(s, _)| *s == team)
            .map(|(_, spec)| {
                spec.agents
                    .iter()
                    .map(|(name, instance)| (name.clone(), instance.template.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut open = self.show_launch;
        let mut launched = false;
        let mut cancel = false;
        egui::Window::new(format!("Launch team: {project}/{team}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Agent");
                egui::ComboBox::from_id_salt("launch_agent")
                    .selected_text(self.launch_agent.clone().unwrap_or_else(|| "—".to_string()))
                    .show_ui(ui, |ui| {
                        for (name, template) in &agents {
                            let label = format!("{name} ({template})");
                            ui.selectable_value(&mut self.launch_agent, Some(name.clone()), label);
                        }
                    });
                ui.label("Model (explicit — opencode's ambient default is never used)");
                ui.text_edit_singleline(&mut self.launch_model);
                if self.launch_model.trim().is_empty() {
                    ui.weak("no model set — the launch will refuse until you fill this in");
                }
                ui.label("Mission");
                ui.add(
                    egui::TextEdit::multiline(&mut self.launch_mission)
                        .desired_rows(6)
                        .desired_width(460.0),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let run = ui.button("Launch");
                    if run.clicked() {
                        match self.launch_agent.clone() {
                            None => toast(
                                toasts,
                                ToastKind::Warning,
                                "this team has no agents to launch",
                            ),
                            Some(agent) => {
                                let model = if self.launch_model.trim().is_empty() {
                                    None
                                } else {
                                    Some(self.launch_model.trim())
                                };
                                let mission = self.launch_mission.clone();
                                if model.is_none() {
                                    toast(
                                        toasts,
                                        ToastKind::Warning,
                                        "set a model first — an explicit model is required \
                                         (opencode's ambient default is never used). It \
                                         pre-fills from the model registry.",
                                    );
                                } else {
                                    match state.launch(project, &team, &agent, model, &mission) {
                                        Ok(()) => {
                                            toast(
                                                toasts,
                                                ToastKind::Success,
                                                format!("launched {agent} on {project}/{team}"),
                                            );
                                            launched = true;
                                            // Land on the run view.
                                            *nav = Some(Screen::Launch);
                                        }
                                        Err(error) => {
                                            toast(toasts, ToastKind::Error, error.to_string())
                                        }
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
            self.show_launch = false;
        }
        if launched {
            self.show_launch = false;
        }
    }

    /// Load a team's spec into the edit form's fields.
    fn load_edit_form(&mut self, state: &mut DeckState) {
        let Some(slug) = self.selected.clone() else {
            return;
        };
        let Some((_, spec)) = state.teams.iter().find(|(s, _)| *s == slug) else {
            return;
        };
        self.edit_label = spec.name.clone();
        self.edit_rev = spec.rev_override.clone().unwrap_or_default();
        self.edit_budget = spec.budget.clone().unwrap_or_default();
        self.edit_agents = spec
            .agents
            .iter()
            .map(|(name, instance)| AgentRow {
                name: name.clone(),
                template: instance.template.clone(),
                model: instance.model.clone().unwrap_or_default(),
            })
            .collect();
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