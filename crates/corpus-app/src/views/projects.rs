//! Project command dashboard for the selected project. Its fixed header keeps
//! conditional Save visible and moves Rename/Clone/Delete into overflow. The
//! responsive body presents environment configuration, team, missions,
//! corpus, run provenance, and cost from real `AppState` data. The project list
//! remains in the scoped sidebar.
//!
//! No business logic here: corpus-core calls go through `AppState`;
//! results surface as toasts. Probing is a corpus-core aggregation
//! (`AppState::refresh_plugins`), scheduled on demand, never per-frame.

use std::time::Duration;

use egui::{Align2, RichText, Ui};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use corpus_core::FindingSeverity;

use crate::fmt::fmt_bytes;
use crate::state::{AppState, FindingDiscovery};
use crate::theme;
use crate::views::components;
use crate::views::plugin_picker::plugin_picker;

const TWO_COLUMN_AT: f32 = 940.0;

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
    /// Project deletion is destructive and is never dispatched directly
    /// from the overflow menu.
    confirm_delete: bool,
    /// Schedule a fresh plugin probe aggregation next frame (probe state
    /// is fetched on demand, not continuously).
    needs_probe: bool,
    show_install: bool,
    install_path: String,
    plugin_details_open: bool,
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
            confirm_delete: false,
            needs_probe: false,
            show_install: false,
            install_path: String::new(),
            plugin_details_open: false,
        }
    }
}

impl ProjectsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = state.effective_project() else {
            ui.add_space(24.0);
            ui.add(egui::Label::new(
                RichText::new("no project selected").color(theme::TEXT_FAINT),
            ));
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
            self.plugin_details_open = false;
        }
        // Drain a requested plugin re-probe before the picker renders.
        if self.needs_probe {
            state.refresh_plugins(Some(&self.edit_plugin));
            self.needs_probe = false;
        }

        let name = if project.name.is_empty() {
            slug.clone()
        } else {
            project.name.clone()
        };
        let binding_dirty = binding_is_dirty(&self.edit_plugin, &project.plugin);
        self.header(
            ui,
            state,
            toasts,
            &slug,
            &name,
            project.created,
            binding_dirty,
        );

        // The command rail stays fixed while the operational dashboard
        // scrolls beneath it.
        egui::ScrollArea::vertical()
            .id_salt("project_body")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(18.0);
                self.status_band(ui, state, &slug);
                ui.add_space(12.0);
                self.dashboard(ui, state, toasts, &slug);
                ui.add_space(18.0);
            });

        self.clone_window(ui, state, toasts, &slug);
        self.rename_window(ui, state, toasts, &slug);
        self.delete_confirm_window(ui, state, toasts, &slug, &name);
        self.wipe_confirm_window(ui, state, toasts, &slug);
        self.install_window(ui, state, toasts);
    }

    fn status_band(&self, ui: &mut Ui, state: &AppState, slug: &str) {
        components::panel_card(ui, "System status", |ui| {
            let env = state.env_status(slug);
            ui.horizontal_wrapped(|ui| {
                match env {
                    Some(ref env) if env.ready => {
                        components::status_badge(ui, "environment ready", components::StatusTone::Healthy)
                            .on_hover_text(&env.notes);
                    }
                    Some(ref env) => {
                        components::status_badge(ui, "environment degraded", components::StatusTone::Danger)
                            .on_hover_text(&env.notes);
                    }
                    None => {
                        components::status_badge(ui, "probe unavailable", components::StatusTone::Warning);
                    }
                }
                ui.add_space(18.0);
                components::metric_cell(
                    ui,
                    "source pins",
                    state.source_pins.len().to_string(),
                    components::StatusTone::Interaction,
                );
                ui.add_space(18.0);
                components::metric_cell(
                    ui,
                    "agents",
                    state.agents.len().to_string(),
                    components::StatusTone::Neutral,
                );
                ui.add_space(18.0);
                components::metric_cell(
                    ui,
                    "missions",
                    state.missions.len().to_string(),
                    components::StatusTone::Neutral,
                );
                if let Some(stats) = state.corpus_stats() {
                    ui.add_space(18.0);
                    components::metric_cell(
                        ui,
                        "corpus files",
                        stats.knowledge_files().to_string(),
                        components::StatusTone::Neutral,
                    );
                }
            });
        });
    }

    /// Two operational columns on a full command canvas; one predictable
    /// reading order when the chat panel or a narrow window reduces space.
    fn dashboard(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
    ) {
        if dashboard_columns(ui.available_width()) == 2 {
            ui.columns(2, |columns| {
                let (left, right) = columns.split_at_mut(1);
                self.configuration_card(&mut left[0], state, toasts, slug);
                card_gap(&mut left[0]);
                self.team_card(&mut left[0], state);
                card_gap(&mut left[0]);
                self.corpus_card(&mut left[0], state);

                self.missions_card(&mut right[0], state, slug);
                card_gap(&mut right[0]);
                self.logs_card(&mut right[0], state);
                card_gap(&mut right[0]);
                self.cost_card(&mut right[0], state);
            });
        } else {
            self.configuration_card(ui, state, toasts, slug);
            card_gap(ui);
            self.team_card(ui, state);
            card_gap(ui);
            self.missions_card(ui, state, slug);
            card_gap(ui);
            self.corpus_card(ui, state);
            card_gap(ui);
            self.logs_card(ui, state);
            card_gap(ui);
            self.cost_card(ui, state);
        }
    }

    fn configuration_card(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
    ) {
        components::panel_card(ui, "Configuration", |ui| {
            ui.label(command_label("Environment plugin"));
            ui.add_space(6.0);
            plugin_picker(
                ui,
                &mut self.edit_plugin,
                state.plugins(),
                &mut self.needs_probe,
            );
            ui.add_space(8.0);
            let selected_status = state
                .plugins()
                .iter()
                .find(|plugin| plugin.name == self.edit_plugin)
                .cloned();
            let leases = state.plugin_leases().to_vec();
            let plugin_busy = state.plugin_work_active();
            ui.horizontal_wrapped(|ui| {
                if let Some(status) = selected_status.as_ref() {
                    plugin_summary(ui, status);
                }
                ui.add_space(12.0);
                let setup_label = match state.plugin_operation() {
                    Some(ref operation)
                        if operation.operation == "setup"
                            && matches!(
                                operation.state,
                                crate::state::PluginOperationState::Failed
                                    | crate::state::PluginOperationState::Cancelled
                            ) =>
                    {
                        "Retry setup"
                    }
                    _ => "Setup",
                };
                for (label, operation) in [(setup_label, "setup"), ("Doctor", "doctor")] {
                    let enabled = !plugin_busy;
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                        match state.start_plugin_lifecycle(&self.edit_plugin, operation) {
                            Ok(true) => toast(
                                toasts,
                                ToastKind::Info,
                                format!("{} {operation} started", self.edit_plugin),
                            ),
                            Ok(false) => toast(
                                toasts,
                                ToastKind::Warning,
                                "another plugin operation is already running",
                            ),
                            Err(error) => toast(toasts, ToastKind::Error, error),
                        }
                    }
                }
                ui.menu_button("⋮", |ui| {
                    let environments_live = !leases.is_empty();
                    let stop = ui
                        .add_enabled(
                            !environments_live && !plugin_busy,
                            egui::Button::new("Stop"),
                        )
                        .on_disabled_hover_text(
                            "Stop active mission environments before stopping the plugin.",
                        );
                    if stop.clicked() {
                        match state.start_plugin_lifecycle(&self.edit_plugin, "stop") {
                            Ok(true) => toast(
                                toasts,
                                ToastKind::Info,
                                format!("{} stop started", self.edit_plugin),
                            ),
                            Ok(false) => toast(
                                toasts,
                                ToastKind::Warning,
                                "another plugin operation is already running",
                            ),
                            Err(error) => toast(toasts, ToastKind::Error, error),
                        }
                        ui.close_menu();
                    }
                    if state.plugin_lifecycle_active("setup") && ui.button("Cancel setup").clicked()
                    {
                        if state.cancel_plugin_lifecycle("setup") {
                            toast(toasts, ToastKind::Info, "cancelling plugin setup");
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(!plugin_busy, egui::Button::new("Install bundle…"))
                        .clicked()
                    {
                        self.show_install = true;
                        ui.close_menu();
                    }
                });
            });
            if let Some(operation) = state.plugin_operation() {
                ui.add_space(8.0);
                plugin_operation(ui, &operation);
            }
            ui.add_space(12.0);
            ui.label(command_label("Source revisions"));
            ui.add_space(6.0);
            if state.source_revisions_loading(slug) {
                empty_hint(ui, "loading source revisions…");
            } else if state.source_revs.is_empty() {
                empty_hint(ui, "no sources declared by this plugin");
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
                            None,
                        ) {
                            if let Err(error) = state.set_source_pin(slug, &source.name, &rev) {
                                toast(toasts, ToastKind::Error, error.to_string());
                            }
                        }
                    }
                });
                ui.add_space(6.0);
                empty_hint(ui, "changes apply immediately");
            }
            ui.add_space(12.0);
            details_toggle(ui, &mut self.plugin_details_open);
            if self.plugin_details_open {
                ui.add_space(10.0);
                if let Some(status) = selected_status.as_ref() {
                    plugin_identity(ui, status);
                }
                ui.add_space(12.0);
                plugin_environments(ui, &leases);
            }
        });
    }

    fn team_card(&self, ui: &mut Ui, state: &mut AppState) {
        components::panel_card(ui, "Team", |ui| {
            let agents = state.agents.clone();
            if agents.is_empty() {
                empty_hint(ui, "no agents in this project");
                return;
            }
            for (slug, agent) in agents {
                let name = crate::state::agent_label(&agent.meta.name, &slug);
                let role = agent.meta.role().as_str();
                if command_row(ui, ("project-agent", &slug), &name, role, components::StatusTone::Interaction)
                    .clicked()
                {
                    state.selected_agent = Some(slug);
                    state.current_screen = crate::nav::Screen::Agents;
                }
            }
        });
    }

    fn missions_card(&self, ui: &mut Ui, state: &mut AppState, project: &str) {
        components::panel_card(ui, "Missions", |ui| {
            let missions = state.missions.clone();
            if missions.is_empty() {
                empty_hint(ui, "no missions in this project");
                return;
            }
            for (slug, mission) in missions {
                let name = crate::state::mission_label(mission.name.as_deref(), &slug);
                let activity = state.mission_activity(project, &slug);
                let old_repo_revision = state.plugin_leases().iter().any(|environment| {
                    environment.mission == slug
                        && environment.drift.iter().any(|detail| {
                            detail.contains(" pin resolves to ")
                                && detail.contains(" but lease runs ")
                        })
                });
                let (activity_status, activity_tone) = match activity {
                    crate::state::MissionActivity::Working => {
                        ("working", components::StatusTone::Healthy)
                    }
                    crate::state::MissionActivity::Waiting => {
                        ("waiting", components::StatusTone::Warning)
                    }
                    crate::state::MissionActivity::Idle => {
                        ("idle", components::StatusTone::Neutral)
                    }
                };
                let status = if old_repo_revision {
                    format!("{activity_status} · old repo revision")
                } else {
                    activity_status.to_string()
                };
                let tone = if old_repo_revision {
                    components::StatusTone::Warning
                } else {
                    activity_tone
                };
                if command_row(ui, ("project-mission", &slug), &name, &status, tone).clicked() {
                    state.select_mission(project, &slug);
                }
            }
        });
    }

    fn corpus_card(&mut self, ui: &mut Ui, state: &AppState) {
        let findings = finding_summary_model(state.finding_discovery());
        components::panel_card(ui, "Corpus signal", |ui| {
            ui.horizontal(|ui| {
                match state.corpus_stats() {
                    Some(stats) => {
                        ui.label(
                            RichText::new(format!("{} files", stats.knowledge_files()))
                                .size(14.0)
                                .strong()
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(fmt_bytes(stats.knowledge_bytes()))
                                .size(13.0)
                                .color(theme::TEXT_MUTED),
                        );
                    }
                    None => empty_hint(ui, "corpus not computed"),
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::destructive_button(ui, "Wipe corpus…")
                        .on_hover_text("findings, techniques, attacks, hypotheses and logs")
                        .clicked()
                    {
                        self.confirm_wipe = true;
                    }
                });
            });
            ui.add_space(10.0);
            if let Some(stats) = state.corpus_stats() {
                if stats.knowledge_files() > 0 {
                    corpus_visual(ui, &stats.categories);
                } else {
                    empty_hint(
                        ui,
                        "empty — missions write findings, techniques, hypotheses and attacks here",
                    );
                }
            }
            if finding_summary_visible(&findings) {
                ui.add_space(14.0);
                components::soft_rule(ui);
                ui.add_space(10.0);
                ui.label(command_label("Findings"));
                ui.add_space(8.0);
                finding_summary(ui, &findings);
            }
        });
    }

    fn logs_card(&self, ui: &mut Ui, state: &AppState) {
        components::panel_card(ui, "Mission logs", |ui| {
            let logs = state
                .corpus_stats()
                .map(|stats| stats.logs.clone())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} logs", logs.files))
                        .size(14.0)
                        .strong()
                        .color(theme::TEXT),
                );
                ui.label(
                    RichText::new(fmt_bytes(logs.bytes))
                        .size(13.0)
                        .color(theme::TEXT_MUTED),
                );
            });
            ui.add_space(10.0);
            if logs.files == 0 {
                empty_hint(ui, "no runs yet — missions write their transcripts here");
            } else {
                mission_log_list(ui, state, logs.bytes);
            }
        });
    }

    fn cost_card(&self, ui: &mut Ui, state: &AppState) {
        components::panel_card(ui, "Cost", |ui| match state.corpus_cost() {
            Some(report) if !report.rows.is_empty() => {
                cost_headline(ui, report);
                ui.add_space(12.0);
                egui::ScrollArea::horizontal()
                    .id_salt("project_cost_scroll")
                    .show(ui, |ui| cost_table(ui, report));
            }
            _ => empty_hint(ui, "no usage yet — updates when an agent finishes a turn"),
        });
    }

    /// Fixed Project command rail. Save exists only while the plugin binding
    /// is dirty; record-level secondary and destructive actions live in the
    /// overflow menu.
    fn header(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
        name: &str,
        created: u64,
        binding_dirty: bool,
    ) {
        components::page_header(
            ui,
            "Project",
            name,
            &format!("created: {}", fmt_epoch(created)),
            |ui| {
                components::action_menu(ui, |ui| {
                    if ui.button("Rename…").clicked() {
                        self.rename_name = name.to_string();
                        self.show_rename = true;
                        ui.close_menu();
                    }
                    if ui.button("Clone…").clicked() {
                        self.clone_name.clear();
                        self.clone_corpus = false;
                        self.show_clone = true;
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .button(RichText::new("Delete…").color(theme::SIGNAL_RED))
                        .clicked()
                    {
                        self.confirm_delete = true;
                        ui.close_menu();
                    }
                });
                if binding_dirty && theme::house_button(ui, "Save •").clicked() {
                    self.save_binding(state, toasts, slug);
                }
            },
        );
    }

    /// Rebind the project's plugin and refresh (projects + the source/env
    /// the top bar and sidebar derive from the new binding).
    fn save_binding(&mut self, state: &mut AppState, toasts: &mut Toasts, slug: &str) {
        if self.edit_plugin.trim().is_empty() {
            toast(toasts, ToastKind::Warning, "pick a plugin first");
            return;
        }
        match state.rebind_project(slug, self.edit_plugin.trim()) {
            Ok(_) => {
                toast(toasts, ToastKind::Success, "environment updated");
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
                toast(toasts, ToastKind::Success, "project deleted");
                state.refresh();
                // ensure_selection re-picks a project next frame.
                state.selected_project = None;
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    fn delete_confirm_window(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
        name: &str,
    ) {
        if !self.confirm_delete {
            return;
        }
        let mut open = self.confirm_delete;
        let mut deleted = false;
        let mut cancel = false;
        egui::Window::new("Delete project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!("Delete {name}?"));
                ui.label(
                    RichText::new(format!(
                        "Project id `{slug}`, its agents, missions, corpus and run logs will be removed. There is no undo."
                    ))
                    .size(12.0)
                    .color(theme::TEXT_MUTED),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::destructive_button(ui, "Delete project").clicked() {
                        self.delete_project(state, toasts, slug);
                        deleted = true;
                    }
                    if theme::house_button(ui, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        self.confirm_delete = open && !deleted && !cancel;
    }

    /// The Corpus Delete confirm: wiping empties the categories and bumps
    /// `corpus_generation` (verified via CLI); the project + agents survive.
    fn wipe_confirm_window(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
    ) {
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
                            Ok(_) => {
                                toast(toasts, ToastKind::Success, "corpus deleted");
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
    fn rename_window(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
    ) {
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
                        Ok(_) => {
                            toast(toasts, ToastKind::Success, "project renamed");
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
                            toast(toasts, ToastKind::Success, "project cloned");
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

    fn install_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        if !self.show_install {
            return;
        }
        let mut open = self.show_install;
        let mut started = false;
        egui::Window::new("Install environment plugin")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Unpacked plugin bundle directory");
                let entry = ui.add(
                    egui::TextEdit::singleline(&mut self.install_path)
                        .desired_width(500.0)
                        .hint_text("/path/to/corpus-plugin-nutshell"),
                );
                ui.label(
                    RichText::new(
                        "Corpus validates manifest v1, executable shape and immutable bundle identity, then selects the installed version.",
                    )
                    .size(12.0)
                    .color(theme::TEXT_FAINT),
                );
                ui.add_space(8.0);
                let submit = entry.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let enabled = !self.install_path.trim().is_empty();
                let clicked = ui
                    .add_enabled_ui(enabled, |ui| theme::house_button(ui, "Install"))
                    .inner
                    .clicked();
                if clicked || (submit && enabled) {
                    match state.start_plugin_install(&self.install_path) {
                        Ok(true) => {
                            toast(toasts, ToastKind::Info, "plugin installation started");
                            started = true;
                        }
                        Ok(false) => toast(
                            toasts,
                            ToastKind::Warning,
                            "another plugin operation is already running",
                        ),
                        Err(error) => toast(toasts, ToastKind::Error, error),
                    }
                }
            });
        self.show_install = open && !started;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FindingCounts {
    critical: usize,
    high: usize,
    medium: usize,
    low: usize,
    unrated: usize,
}

impl FindingCounts {
    fn from_cards(cards: &[corpus_core::FindingCard]) -> Self {
        let mut counts = Self::default();
        for card in cards {
            match card.severity {
                Some(FindingSeverity::Critical) => counts.critical += 1,
                Some(FindingSeverity::High) => counts.high += 1,
                Some(FindingSeverity::Medium) => counts.medium += 1,
                Some(FindingSeverity::Low) => counts.low += 1,
                None => counts.unrated += 1,
            }
        }
        counts
    }

    fn total(self) -> usize {
        self.critical + self.high + self.medium + self.low + self.unrated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FindingSummaryModel {
    Loading,
    Ready(FindingCounts),
    Failed {
        message: String,
        last_good: FindingCounts,
    },
}

fn finding_summary_model(discovery: &FindingDiscovery) -> FindingSummaryModel {
    match discovery {
        FindingDiscovery::Loading => FindingSummaryModel::Loading,
        FindingDiscovery::Ready(cards) => {
            FindingSummaryModel::Ready(FindingCounts::from_cards(cards))
        }
        FindingDiscovery::Failed { message, last_good } => FindingSummaryModel::Failed {
            message: message.clone(),
            last_good: FindingCounts::from_cards(last_good),
        },
    }
}

fn finding_summary_visible(model: &FindingSummaryModel) -> bool {
    !matches!(model, FindingSummaryModel::Ready(counts) if counts.total() == 0)
}

fn finding_summary(ui: &mut Ui, model: &FindingSummaryModel) {
    let counts = match model {
        FindingSummaryModel::Loading => {
            empty_hint(ui, "loading findings…");
            return;
        }
        FindingSummaryModel::Ready(counts) => counts,
        FindingSummaryModel::Failed { message, last_good } => {
            components::status_badge(ui, "refresh failed", components::StatusTone::Danger)
                .on_hover_text(message);
            ui.add_space(8.0);
            last_good
        }
    };
    let width = finding_tile_width(ui.available_width());
    ui.horizontal_wrapped(|ui| {
        for (label, count, color) in finding_count_entries(*counts) {
            if count == 0 {
                continue;
            }
            finding_count_tile(ui, width, label, count, color);
        }
    });
}

fn finding_count_entries(counts: FindingCounts) -> [(&'static str, usize, egui::Color32); 5] {
    [
        ("CRITICAL", counts.critical, theme::FINDING_CRITICAL),
        ("HIGH", counts.high, theme::FINDING_HIGH),
        ("MEDIUM", counts.medium, theme::FINDING_MEDIUM),
        ("LOW", counts.low, theme::FINDING_LOW),
        ("UNRATED", counts.unrated, theme::FINDING_UNRATED),
    ]
}

fn finding_tile_width(available: f32) -> f32 {
    if available >= 480.0 {
        ((available - 32.0) / 5.0).max(72.0)
    } else if available >= 240.0 {
        ((available - 8.0) / 2.0).max(96.0)
    } else {
        available.max(96.0)
    }
}

fn finding_count_tile(
    ui: &mut Ui,
    width: f32,
    label: &str,
    count: usize,
    color: egui::Color32,
) {
    egui::Frame::default()
        .fill(color.gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.90)))
        .corner_radius(egui::CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width((width - 20.0).max(52.0));
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(label)
                        .size(10.5)
                        .monospace()
                        .strong()
                        .color(color),
                );
                ui.add_space(3.0);
                ui.label(
                    RichText::new(count.to_string())
                        .size(24.0)
                        .monospace()
                        .strong()
                        .color(color),
                );
            });
        });
}

fn card_gap(ui: &mut Ui) {
    ui.add_space(12.0);
}

fn command_label(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(10.5)
        .monospace()
        .color(theme::TEXT_FAINT)
}

fn empty_hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.0).color(theme::TEXT_FAINT));
}

fn plugin_summary(ui: &mut Ui, status: &corpus_core::PluginStatus) {
    components::status_badge(
        ui,
        if status.ready { "ready" } else { "not ready" },
        if status.ready {
            components::StatusTone::Healthy
        } else {
            components::StatusTone::Danger
        },
    )
    .on_hover_text(&status.notes);
    ui.label(
        RichText::new(format!(
            "v{}",
            status.version.as_deref().unwrap_or("unknown")
        ))
        .monospace()
        .size(12.0)
        .color(theme::TEXT_MUTED),
    );
}

fn details_toggle(ui: &mut Ui, open: &mut bool) {
    egui::Frame::default()
        .stroke(egui::Stroke::new(1.0_f32, theme::KEYLINE_SOFT))
        .corner_radius(egui::CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            let marker = if *open { "⌄" } else { "›" };
            let state = if *open { "Expanded" } else { "Collapsed" };
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{marker}  Details"))
                                .monospace()
                                .size(12.0)
                                .color(theme::TEXT_MUTED),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    *open = !*open;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(state)
                            .monospace()
                            .size(10.5)
                            .color(theme::TEXT_FAINT),
                    );
                });
            });
        });
}

fn plugin_identity(ui: &mut Ui, status: &corpus_core::PluginStatus) {
    let origin = match status.origin {
        corpus_core::PluginOrigin::Direct => "development override",
        corpus_core::PluginOrigin::Installed => "selected install",
    };
    ui.horizontal_wrapped(|ui| {
        components::status_badge(
            ui,
            if status.ready { "ready" } else { "not ready" },
            if status.ready {
                components::StatusTone::Healthy
            } else {
                components::StatusTone::Danger
            },
        )
        .on_hover_text(&status.notes);
        ui.label(
            RichText::new(format!(
                "{} · {} · {}",
                status.protocol.as_deref().unwrap_or("legacy protocol"),
                status.version.as_deref().unwrap_or("unversioned"),
                origin
            ))
            .size(12.0)
            .color(theme::TEXT_MUTED),
        );
    });
    if let Some(digest) = status.bundle_digest.as_deref() {
        identity_line(ui, "bundle", digest);
    }
    if status.prepared.docker_required == Some(true) {
        identity_line(ui, "runtime", "Docker required");
    }
    if let Some(topology) = status.prepared.topology.as_deref() {
        identity_line(ui, "topology", topology);
    }
    if let Some(ownership) = status.prepared.backbone_ownership.as_deref() {
        identity_line(ui, "backbone", ownership);
    }
    if let Some(lock) = status.prepared.environment_lock.as_deref() {
        identity_line(ui, "environment", lock);
    }
    if let Some(image) = status.prepared.image_digest.as_deref() {
        identity_line(ui, "prepared image", image);
    }
    if !status.capabilities.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.label(command_label("Capabilities"));
            for capability in &status.capabilities {
                ui.label(
                    RichText::new(capability)
                        .monospace()
                        .size(11.0)
                        .color(theme::TEXT_MUTED),
                );
            }
        });
    }
    if !status.notes.is_empty() {
        empty_hint(ui, &status.notes);
    }
}

fn identity_line(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(command_label(label));
        ui.label(
            RichText::new(short_identity(value))
                .monospace()
                .size(11.0)
                .color(theme::TEXT_MUTED),
        )
        .on_hover_text(value);
    });
}

fn short_identity(value: &str) -> String {
    const MAX: usize = 28;
    if value.chars().count() <= MAX {
        return value.to_string();
    }
    let head: String = value.chars().take(16).collect();
    let tail: String = value
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn plugin_operation(ui: &mut Ui, operation: &crate::state::PluginOperationView) {
    let (label, tone) = match operation.state {
        crate::state::PluginOperationState::Running => {
            ui.spinner();
            ("running", components::StatusTone::Interaction)
        }
        crate::state::PluginOperationState::Succeeded => {
            ("complete", components::StatusTone::Healthy)
        }
        crate::state::PluginOperationState::Failed => ("failed", components::StatusTone::Danger),
        crate::state::PluginOperationState::Cancelled => {
            ("cancelled", components::StatusTone::Warning)
        }
    };
    ui.horizontal_wrapped(|ui| {
        components::status_badge(ui, label, tone);
        ui.label(
            RichText::new(format!("{} {}", operation.plugin, operation.operation))
                .monospace()
                .size(12.0)
                .color(theme::TEXT),
        );
        if let Some(phase) = operation.phase.as_deref() {
            ui.label(RichText::new(phase).size(12.0).color(theme::TEXT_MUTED));
        }
    });
    if !operation.detail.is_empty() {
        empty_hint(ui, &operation.detail);
    }
    if let Some(recovery) = operation.recovery.as_deref() {
        ui.label(RichText::new(recovery).size(12.0).color(theme::WARN));
    }
}

fn plugin_environments(ui: &mut Ui, leases: &[crate::state::PluginLeaseView]) {
    ui.label(command_label("Active mission environments"));
    ui.add_space(4.0);
    if leases.is_empty() {
        empty_hint(ui, "no mission environment is active");
        return;
    }
    for lease in leases {
        let healthy = lease.state == corpus_core::EnvironmentSessionState::Ready
            && lease.error.is_none()
            && lease.drift.is_empty();
        ui.horizontal_wrapped(|ui| {
            components::status_badge(
                ui,
                if healthy { "aligned" } else { "attention" },
                if healthy {
                    components::StatusTone::Healthy
                } else {
                    components::StatusTone::Danger
                },
            );
            ui.label(
                RichText::new(format!(
                    "{} · {:?} · plugin {}",
                    lease.mission, lease.state, lease.plugin_version
                ))
                .monospace()
                .size(12.0)
                .color(theme::TEXT),
            );
        });
        identity_line(ui, "bundle", &lease.plugin_digest);
        if let Some(lock) = lease.environment_lock.as_deref() {
            identity_line(ui, "environment", lock);
        }
        if let Some(image) = lease.image_digest.as_deref() {
            identity_line(ui, "target image", image);
        }
        for (source, sha) in &lease.source_shas {
            identity_line(ui, &format!("source {source}"), sha);
        }
        for drift in &lease.drift {
            ui.label(
                RichText::new(format!("pin drift: {drift}"))
                    .size(12.0)
                    .color(theme::WARN),
            );
        }
        if let Some(error) = lease.error.as_deref() {
            ui.label(RichText::new(error).size(12.0).color(theme::SIGNAL_RED));
        }
        ui.add_space(8.0);
    }
}

/// Dense, full-width navigation row used by the Team and Missions cards.
/// The right label carries semantic state; the whole row is one target.
fn command_row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    label: &str,
    status: &str,
    tone: components::StatusTone,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 34.0),
        egui::Sense::click(),
    );
    let response = ui.interact(rect, ui.make_persistent_id(id), egui::Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter().rect_filled(rect, 2.0, theme::ROW_HOVER);
    }
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        egui::Stroke::new(1.0_f32, theme::KEYLINE_SOFT),
    );
    let status = status.to_uppercase();
    let status_galley = ui
        .painter()
        .layout_no_wrap(status.clone(), theme::mono(10.5), tone.color());
    let label_clip = egui::Rect::from_min_max(
        rect.min,
        egui::pos2(
            (rect.right() - status_galley.size().x - 24.0).max(rect.left()),
            rect.bottom(),
        ),
    );
    ui.painter().with_clip_rect(label_clip).text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        theme::font(13.5),
        theme::TEXT,
    );
    ui.painter().text(
        egui::pos2(rect.right() - 8.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        status,
        theme::mono(10.5),
        tone.color(),
    );
    response
}

fn binding_is_dirty(edit: &str, saved: &str) -> bool {
    edit != saved
}

fn dashboard_columns(available_width: f32) -> usize {
    if available_width >= TWO_COLUMN_AT { 2 } else { 1 }
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
        let seg =
            egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, rect.height()));
        let color = theme::CORPUS_PALETTE[i % theme::CORPUS_PALETTE.len()];
        painter.rect_filled(seg, 0.0, color);
        painter.rect_stroke(
            seg,
            0.0,
            egui::Stroke::new(1.0_f32, theme::BG),
            egui::StrokeKind::Inside,
        );
        ui.allocate_rect(seg, egui::Sense::hover())
            .on_hover_text(format!(
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
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(
                dot,
                1.0,
                theme::CORPUS_PALETTE[i % theme::CORPUS_PALETTE.len()],
            );
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
/// agent label, run stamp, file name, size, and a bar sized to its share of the
/// logs total, so a runaway run is obvious at a glance.
fn mission_log_list(ui: &mut Ui, state: &AppState, total: u64) {
    let width = ui.available_width().min(760.0);
    let logs = state.mission_logs();
    for log in logs.iter().take(MISSION_LOG_ROWS) {
        // Fixed row width so the right-aligned file name tracks the strip
        // above it instead of the window edge.
        ui.allocate_ui(egui::vec2(width, 16.0), |ui| {
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(egui::vec2(90.0, 10.0), egui::Sense::hover());
                let painter = ui.painter_at(bar);
                painter.rect_filled(bar, 1.0, theme::PLATE_FRONT);
                let share = log.bytes as f32 / total.max(1) as f32;
                let filled = egui::Rect::from_min_size(
                    bar.min,
                    egui::vec2((bar.width() * share).max(1.0), bar.height()),
                );
                painter.rect_filled(filled, 1.0, theme::MISSION_LOG);
                let agent = log
                    .agent
                    .as_deref()
                    .map(|slug| state.agent_label(slug))
                    .unwrap_or_else(|| "unknown agent".to_string());
                ui.label(RichText::new(agent).size(12.0).color(theme::TEXT));
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
    let num = |text: String| {
        RichText::new(text)
            .size(12.5)
            .monospace()
            .color(theme::TEXT_MUTED)
    };
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
            for title in [
                "model", "provider", "in", "out", "reason", "cache r", "cache w", "cost",
            ] {
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
                    RichText::new(text)
                        .size(12.5)
                        .monospace()
                        .strong()
                        .color(theme::TEXT)
                };
                tr.col(|ui| {
                    ui.label(strong_num("total".to_string()));
                });
                tr.col(|ui| {
                    ui.label(num(format!(
                        "{} tok",
                        crate::fmt::fmt_tokens(report.tokens)
                    )));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn finding_card(severity: Option<FindingSeverity>) -> corpus_core::FindingCard {
        corpus_core::FindingCard {
            path: std::path::PathBuf::from("findings/f.md"),
            title: "Finding".into(),
            title_source: corpus_core::FindingTitleSource::Title,
            severity,
            timestamp: None,
            time_source: None,
            reference: "F-1".into(),
            reference_source: corpus_core::FindingReferenceSource::Id,
            status: None,
            oracle_verified: None,
            sensitivity: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn project_save_is_visible_only_for_a_dirty_plugin_binding() {
        assert!(!binding_is_dirty("cdk-regtest", "cdk-regtest"));
        assert!(binding_is_dirty("other", "cdk-regtest"));
    }

    #[test]
    fn dashboard_stacks_when_chat_or_window_reduces_the_canvas() {
        assert_eq!(dashboard_columns(TWO_COLUMN_AT - 1.0), 1);
        assert_eq!(dashboard_columns(TWO_COLUMN_AT), 2);
        assert_eq!(dashboard_columns(1_440.0), 2);
    }

    #[test]
    fn finding_summary_preserves_loading_failure_and_empty_counts() {
        assert_eq!(
            finding_summary_model(&FindingDiscovery::Loading),
            FindingSummaryModel::Loading
        );
        assert_eq!(
            finding_summary_model(&FindingDiscovery::Ready(Vec::new())),
            FindingSummaryModel::Ready(FindingCounts::default())
        );
        let failed = FindingDiscovery::Failed {
            message: "watch failed".into(),
            last_good: vec![finding_card(Some(FindingSeverity::High))],
        };
        match finding_summary_model(&failed) {
            FindingSummaryModel::Failed { message, last_good } => {
                assert_eq!(message, "watch failed");
                assert_eq!(last_good.high, 1);
            }
            other => panic!("expected failed model, got {other:?}"),
        }
    }

    #[test]
    fn finding_summary_counts_every_severity_and_keeps_unrated_visible() {
        let cards = vec![
            finding_card(Some(FindingSeverity::Critical)),
            finding_card(Some(FindingSeverity::High)),
            finding_card(Some(FindingSeverity::High)),
            finding_card(Some(FindingSeverity::Medium)),
            finding_card(Some(FindingSeverity::Low)),
            finding_card(None),
            finding_card(None),
        ];
        let FindingSummaryModel::Ready(counts) =
            finding_summary_model(&FindingDiscovery::Ready(cards))
        else {
            panic!("expected ready counts")
        };
        assert_eq!(
            counts,
            FindingCounts {
                critical: 1,
                high: 2,
                medium: 1,
                low: 1,
                unrated: 2,
            }
        );
    }

    #[test]
    fn empty_summary_is_hidden_and_zero_severity_boxes_are_omitted() {
        let empty = FindingSummaryModel::Ready(FindingCounts::default());
        assert!(!finding_summary_visible(&empty));
        assert!(finding_summary_visible(&FindingSummaryModel::Loading));
        assert!(finding_summary_visible(&FindingSummaryModel::Failed {
            message: "unknown".into(),
            last_good: FindingCounts::default(),
        }));

        let counts = FindingCounts {
            critical: 2,
            low: 1,
            ..FindingCounts::default()
        };
        let visible = finding_count_entries(counts)
            .into_iter()
            .filter_map(|(label, count, _)| (count > 0).then_some((label, count)))
            .collect::<Vec<_>>();
        assert_eq!(visible, [("CRITICAL", 2), ("LOW", 1)]);
    }

    #[test]
    fn finding_tiles_wrap_without_becoming_tiny() {
        assert!(finding_tile_width(900.0) >= 160.0);
        assert!(finding_tile_width(479.0) >= 96.0);
        assert_eq!(finding_tile_width(200.0), 200.0);
    }
}
