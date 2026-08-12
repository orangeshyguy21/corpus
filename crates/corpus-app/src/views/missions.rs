//! Missions screen: mission list + launch button reusing the existing
//! run view. The launch dialog picks an agent + model + mission text.

use std::time::Duration;

use egui::{Align2, Ui};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::nav::Screen;
use crate::state::AppState;
use crate::views::model_picker::{ModelField, ModelPicker};

const PROBE_MISSION: &str = "Probe the environment: report the plugin probe status, the sandbox targets, and the available tools. Do not attack anything; map the surfaces and cite what you observe.";

pub struct MissionsView {
    project: Option<String>,
    viewed_project: Option<String>,
    dirty: bool,
    show_launch: bool,
    launch_agent: Option<String>,
    launch_model: String,
    launch_mission: String,
    launch_picker: ModelPicker,
}

impl Default for MissionsView {
    fn default() -> Self {
        Self {
            project: None,
            viewed_project: None,
            dirty: true,
            show_launch: false,
            launch_agent: None,
            launch_model: String::new(),
            launch_mission: PROBE_MISSION.to_string(),
            launch_picker: ModelPicker::default(),
        }
    }
}

impl MissionsView {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        nav: &mut Option<Screen>,
    ) {
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
        if self.dirty {
            self.dirty = false;
        }

        ui.horizontal(|ui| {
            ui.heading("Missions");
            ui.add_space(8.0);
            ui.label("project");
            egui::ComboBox::from_id_salt("mission_project")
                .selected_text(match state.projects.iter().find(|(s, _)| *s == chosen) {
                    Some((slug, project)) => format!("{slug} — {}", project.name),
                    None => chosen.clone(),
                })
                .show_ui(ui, |ui| {
                    for (slug, project) in &state.projects {
                        ui.selectable_value(&mut self.project, Some(slug.clone()), format!("{slug} — {p}", p = project.name));
                    }
                });
        });
        ui.add_space(4.0);

        // Launch button — pick an agent and mission text.
        ui.horizontal(|ui| {
            if ui.button("+ New launch…").clicked() {
                self.arm_launch(state);
            }
            if ui.button("Refresh").clicked() {
                self.dirty = true;
            }
        });

        ui.add_space(8.0);
        ui.weak("Ad-hoc launch — pick an agent, model, and mission. Persistent mission records (corpus mission list) land with the Mission entity CRUD.");
        ui.add_space(8.0);

        self.launch_window(ui, state, toasts, &chosen, nav);
    }

    fn arm_launch(&mut self, state: &mut AppState) {
        let project = self.project.clone().unwrap_or_default();
        state.refresh_agents(&project);
        self.launch_agent = state.agents.first().map(|(s, _)| s.clone());
        self.launch_model = self
            .launch_agent
            .as_deref()
            .and_then(|agent| state.agent_default_model(&project, agent))
            .unwrap_or_default();
        self.launch_mission = PROBE_MISSION.to_string();
        state.ensure_models();
        self.show_launch = !state.agents.is_empty();
    }

    fn launch_window(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        nav: &mut Option<Screen>,
    ) {
        if !self.show_launch { return; }
        let agents: Vec<String> = state.agents.iter().map(|(s, _)| s.clone()).collect();
        let mut open = self.show_launch;
        let mut launched = false;
        let mut cancel = false;
        egui::Window::new("Launch mission")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Agent");
                egui::ComboBox::from_id_salt("launch_agent")
                    .selected_text(self.launch_agent.clone().unwrap_or_else(|| "—".to_string()))
                    .show_ui(ui, |ui| {
                        for name in &agents {
                            if ui.selectable_value(&mut self.launch_agent, Some(name.clone()), name).clicked() {
                                self.launch_model = state
                                    .agent_default_model(project, name)
                                    .unwrap_or_default();
                            }
                        }
                    });
                ui.label("Model (explicit — opencode's ambient default is never used)");
                ui.horizontal(|ui| {
                    self.launch_picker.field(
                        ui,
                        "launch_model",
                        &mut self.launch_model,
                        ModelField {
                            models: state.models(),
                            badges: state.benchmarked_ids(),
                            degrade_note: state.models_error(),
                            allow_none: false,
                        },
                    );
                    if state.models_loading() { ui.spinner(); }
                    else if ui.button("↻").on_hover_text("refresh the model list from opencode").clicked() {
                        state.refresh_models(true);
                    }
                });
                if self.launch_model.trim().is_empty() {
                    ui.weak("no model picked — the launch will refuse until you pick one");
                }
                ui.label("Mission");
                ui.add(
                    egui::TextEdit::multiline(&mut self.launch_mission)
                        .desired_rows(6)
                        .desired_width(460.0),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Launch").clicked() {
                        match self.launch_agent.clone() {
                            None => toast(toasts, ToastKind::Warning, "project has no agents to launch"),
                            Some(agent) => {
                                let model = if self.launch_model.trim().is_empty() { None } else { Some(self.launch_model.trim()) };
                                let mission = self.launch_mission.clone();
                                if model.is_none() {
                                    toast(toasts, ToastKind::Warning,
                                        "pick a model first — an explicit model is required \
                                         (opencode's ambient default is never used).");
                                } else {
                                    match state.launch(project, &agent, model, &mission) {
                                        Ok(()) => {
                                            toast(toasts, ToastKind::Success, format!("launched {agent} on {project}"));
                                            launched = true;
                                            *nav = Some(Screen::Launch);
                                        }
                                        Err(error) => { toast(toasts, ToastKind::Error, error.to_string()) }
                                    }
                                }
                            }
                        }
                    }
                    if ui.button("Cancel").clicked() { cancel = true; }
                });
            });
        if cancel || !open { self.show_launch = false; }
        if launched { self.show_launch = false; }
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