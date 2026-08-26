//! The left project tree: one row per project with its agents and missions nested
//! directly beneath (full lists, dim mini-headers with `+` create
//! buttons; missions are siblings of agents, never nested under them).
//! The selected row gets a full-width ROW_HL fill (text stays TEXT); the
//! project, agent, and mission rows reveal a `dots_three_vertical`
//! action menu on hover, selection, or focus; bottom-left the fixed corpus summary (walked
//! via corpus-core `corpus_stats`, refreshed on selection change + a
//! refresh icon).
//!
//! The sidebar IS the navigation: clicking the section header, a project,
//! or a child row routes the main column to the matching screen (child
//! clicks select their project first — menu actions likewise operate on
//! the row's own project). Selection state lives on `AppState`; the tree
//! data is `AppState::trees`, rebuilt on the refresh paths, never per
//! frame. The `+` create flows (project / agent-clone-from-seed /
//! mission) are small modal windows the sidebar owns.

use std::time::Duration;

use egui::{Align2, RichText, Ui};
use egui_phosphor::regular as ph;
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::fmt::fmt_bytes;
use crate::nav::Screen;
use crate::state::{AppState, MissionDisplayState};
use crate::theme;
use crate::views::mission_actions;
use crate::views::plugin_picker::plugin_picker;

/// Row height for a sidebar list row (15px text + 5px vertical padding).
const ROW_H: f32 = 24.0;

/// Widget state for the sidebar: its three `+` create flows, row action
/// menus and dialogs, and the on-demand plugin probe.
pub struct Sidebar {
    create_project: bool,
    create_name: String,
    create_plugin: String,
    new_agent: bool,
    agent_role: corpus_core::AgentRole,
    clone_from: Option<String>,
    show_clone: bool,
    clone_name: String,
    clone_corpus: bool,
    /// The mission being renamed (Mission menu -> Rename…) — the project
    /// rides along: tree rows can belong to a non-selected project.
    rename_mission_project: Option<String>,
    rename_mission: Option<String>,
    rename_name: String,
    /// The project being renamed (Project kebab -> Rename…) and its edit
    /// buffer. Separate from the mission rename's state: both modals can be
    /// open at once, and one shared buffer would let them overwrite each
    /// other's text.
    rename_project: Option<String>,
    rename_project_name: String,
    /// The row whose overflow popup is open. Keeping this explicit means
    /// the button remains rendered while the pointer moves from the row into
    /// the popup; hover-only widgets otherwise disappear on the next frame.
    open_row_menu: Option<String>,
    /// Destructive sidebar actions use the same confirm-first ritual as the
    /// corresponding detail pages.
    delete_project: Option<(String, String)>,
    delete_agent: Option<(String, String, String)>,
    /// The project the new-agent modal targets (the `+` on a project's
    /// agents group sets it).
    new_agent_project: String,
    /// Schedule a fresh plugin probe aggregation (the create-project picker's
    /// Re-probe). Never per-frame.
    needs_probe: bool,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            create_project: false,
            create_name: String::new(),
            create_plugin: "cdk-regtest".to_string(),
            new_agent: false,
            agent_role: corpus_core::AgentRole::Researcher,
            clone_from: None,
            show_clone: false,
            clone_name: String::new(),
            clone_corpus: false,
            rename_mission_project: None,
            rename_mission: None,
            rename_name: String::new(),
            rename_project: None,
            rename_project_name: String::new(),
            open_row_menu: None,
            delete_project: None,
            delete_agent: None,
            new_agent_project: String::new(),
            needs_probe: false,
        }
    }
}

impl Sidebar {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let max = ui.available_rect_before_wrap();
        let footer_h = 50.0;
        let scroll_max = egui::Rect::from_min_max(
            max.min,
            egui::pos2(max.max.x, (max.max.y - footer_h).max(max.min.y)),
        );
        let mut scroll_ui = ui.new_child(egui::UiBuilder::new().max_rect(scroll_max));
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .id_salt("sidebar_sections")
            .show(&mut scroll_ui, |ui| {
                self.sections(ui, state, toasts);
            });

        // Bottom-fixed corpus summary + manual refresh.
        let footer_rect =
            egui::Rect::from_min_max(egui::pos2(max.min.x, max.max.y - footer_h), max.max);
        let mut footer_ui = ui.new_child(egui::UiBuilder::new().max_rect(footer_rect));
        self.footer(&mut footer_ui, state);

        // Drain a requested plugin re-probe before the modals render.
        if self.needs_probe {
            state.refresh_plugins(Some(&self.create_plugin));
            self.needs_probe = false;
        }

        self.create_project_window(ui, state, toasts);
        self.new_agent_window(ui, state, toasts);
        self.rename_window(ui, state, toasts);
        self.rename_project_window(ui, state, toasts);
        self.delete_project_window(ui, state, toasts);
        self.delete_agent_window(ui, state, toasts);
        self.clone_window(ui, state, toasts);
    }

    fn sections(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        ui.add_space(4.0);
        self.section_tree(ui, state, toasts);
    }

    /// The project tree: one row per project (action menus reveal on hover,
    /// selection, focus, or while open), with its agents and missions nested
    /// directly beneath — full lists, no per-group expanders. ACCORDION:
    /// only the selected project's children show; selecting another
    /// project collapses the rest. Projects sort newest-created first,
    /// so the default-open project is the most recent.
    fn section_tree(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let (header, plus) = section_header(ui, "Projects");
        if header {
            state.current_screen = Screen::Projects;
        }
        if plus {
            self.create_project = true;
            self.needs_probe = true;
        }
        ui.add_space(2.0);
        let selected = state.effective_project();
        let projects = state.projects.clone();
        let trees = state.trees.clone();
        for (slug, project) in &projects {
            let open = selected.as_deref() == Some(slug.as_str());
            self.project_row(ui, state, slug, project, open);
            if open {
                if let Some(tree) = trees.get(slug) {
                    self.project_children(ui, state, toasts, slug, tree);
                }
            }
        }
        if projects.is_empty() {
            row_hint(ui, 8.0, "no projects — press +");
        }
    }

    /// One project row (display name, hover/selection kebab) — the tree's
    /// parent node.
    fn project_row(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        slug: &str,
        project: &corpus_core::Project,
        is_sel: bool,
    ) {
        let Row {
            ui: mut rui,
            rect,
            click,
            hovered,
        } = row_ui(ui, is_sel, true, slug);
        // Row label is the display NAME only (slug falls back when the
        // name is empty); the slug moves to the hover text (defect 1a).
        let name = if project.name.is_empty() {
            slug.to_string()
        } else {
            project.name.clone()
        };
        let menu_key = format!("project:{slug}");
        let show_menu = row_menu_visible(
            is_sel,
            hovered,
            click.has_focus(),
            self.open_row_menu.as_deref() == Some(menu_key.as_str()),
        );
        if show_menu {
            let menu_rect = egui::Rect::from_min_max(
                egui::pos2(rect.max.x - KEBAB_STRIP, rect.min.y),
                rect.max,
            );
            let open = rui
                .allocate_new_ui(
                    egui::UiBuilder::new()
                        .max_rect(menu_rect)
                        .layout(egui::Layout::right_to_left(egui::Align::Center)),
                    |ui| {
                        ui.push_id(&menu_key, |ui| {
                            egui::menu::menu_custom_button(ui, overflow_button(), |ui| {
                                if ui.button("Rename…").clicked() {
                                    self.rename_project = Some(slug.to_string());
                                    self.rename_project_name = name.clone();
                                    ui.close_menu();
                                }
                                if ui.button("Clone…").clicked() {
                                    self.prep_clone(slug.to_string());
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui
                                    .button(RichText::new("Delete…").color(theme::SIGNAL_RED))
                                    .clicked()
                                {
                                    self.delete_project = Some((slug.to_string(), name.clone()));
                                    ui.close_menu();
                                }
                            })
                            .inner
                            .is_some()
                        })
                        .inner
                    },
                )
                .inner;
            self.remember_open_menu(&menu_key, open);
        }
        let label_rect = row_label_rect(rect, true);
        let label_resp = rui
            .allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(label_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(RichText::new(&name).size(15.0).color(theme::TEXT))
                            .sense(egui::Sense::click())
                            .truncate(),
                    )
                },
            )
            .inner;
        if label_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if click.clicked() || label_resp.clicked() {
            state.select_project(slug);
            state.current_screen = Screen::Projects;
        }
        click.on_hover_text(format!("{name} · {slug}"));
    }

    /// A project's nested children: the full agent list, then the full
    /// mission list (with its row menus), each under a dim mini-header
    /// with a `+`. Missions are siblings of agents, never nested under
    /// them.
    fn project_children(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        tree: &crate::state::ProjectTree,
    ) {
        let project_selected = state.effective_project().as_deref() == Some(project);
        // --- agents ---
        let (agents_header, agents_plus) = mini_header(ui, &format!("{project}-agents"), "agents");
        if agents_header {
            state.current_screen = Screen::Agents;
        }
        if agents_plus {
            self.new_agent_project = project.to_string();
            self.new_agent = true;
        }
        let on_screen = project_selected && state.current_screen == Screen::Agents;
        for (slug, agent) in &tree.agents {
            let is_sel = on_screen && state.selected_agent.as_deref() == Some(slug.as_str());
            let Row {
                ui: mut rui,
                rect,
                click,
                hovered,
            } = row_ui(ui, is_sel, true, ("agent", project, slug));
            // Row label is the display NAME, never the opaque slug (a UUID);
            // the slug moves to the hover text for identity.
            let name = crate::state::agent_label(&agent.meta.name, slug);
            let menu_key = format!("agent:{project}:{slug}");
            let show_menu = row_menu_visible(
                is_sel,
                hovered,
                click.has_focus(),
                self.open_row_menu.as_deref() == Some(menu_key.as_str()),
            );
            if show_menu {
                let menu_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.max.x - KEBAB_STRIP, rect.min.y),
                    rect.max,
                );
                let open = rui
                    .allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(menu_rect)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                        |ui| {
                            ui.push_id(&menu_key, |ui| {
                                egui::menu::menu_custom_button(ui, overflow_button(), |ui| {
                                    if ui.button("Clone…").clicked() {
                                        match state.clone_agent(project, slug) {
                                            Ok(()) => {
                                                toast(toasts, ToastKind::Success, "agent cloned");
                                                state.refresh();
                                            }
                                            Err(error) => {
                                                toast(toasts, ToastKind::Error, error.to_string())
                                            }
                                        }
                                        ui.close_menu();
                                    }
                                    ui.separator();
                                    if ui
                                        .button(RichText::new("Delete…").color(theme::SIGNAL_RED))
                                        .clicked()
                                    {
                                        self.delete_agent = Some((
                                            project.to_string(),
                                            slug.to_string(),
                                            name.clone(),
                                        ));
                                        ui.close_menu();
                                    }
                                })
                                .inner
                                .is_some()
                            })
                            .inner
                        },
                    )
                    .inner;
                self.remember_open_menu(&menu_key, open);
            }
            let label = rui
                .allocate_new_ui(
                    egui::UiBuilder::new()
                        .max_rect(row_label_rect(rect, true))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        ui.add_space(24.0);
                        ui.add(
                            egui::Label::new(RichText::new(&name).size(13.5).color(theme::TEXT))
                                .sense(egui::Sense::click())
                                .truncate(),
                        )
                    },
                )
                .inner;
            if label.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if click.clicked() || label.clicked() {
                state.selected_agent = Some(slug.clone());
                state.current_screen = Screen::Agents;
            }
            click.on_hover_text(format!(
                "{project} · {} · {slug}",
                crate::state::agent_label(&agent.meta.name, slug)
            ));
        }
        if tree.agents.is_empty() {
            row_hint(ui, 24.0, "no agents — press +");
        }
        // --- missions (siblings of agents, NOT nested under them) ---
        let (missions_header, missions_plus) =
            mini_header(ui, &format!("{project}-missions"), "missions");
        if missions_header {
            state.current_screen = Screen::Missions;
        }
        if missions_plus {
            self.new_mission(state, toasts, project);
        }
        let on_screen = project_selected && state.current_screen == Screen::Missions;
        for (slug, mission) in &tree.missions {
            let is_sel = on_screen && state.selected_mission.as_deref() == Some(slug.as_str());
            let label_text = crate::state::mission_label(mission.name.as_deref(), slug);
            // One static status language across lifecycle and activity.
            // A session parked at its prompt is live, not busy, and uses
            // the quieter green rather than a transition/warning color.
            let display_state = state.mission_display_state(project, slug);
            // A mission row always reserves the kebab strip (⋮ shown on
            // the selected row and on row hover).
            let Row {
                ui: mut rui,
                rect,
                click,
                hovered,
            } = row_ui(ui, is_sel, true, ("mission", project, slug));
            let menu_key = format!("mission:{project}:{slug}");
            let show_menu = row_menu_visible(
                is_sel,
                hovered,
                click.has_focus(),
                self.open_row_menu.as_deref() == Some(menu_key.as_str()),
            );
            if show_menu {
                let menu_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.max.x - KEBAB_STRIP, rect.min.y),
                    rect.max,
                );
                let open = rui
                    .allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(menu_rect)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                        |ui| {
                            ui.push_id(&menu_key, |ui| {
                                self.mission_menu(ui, state, toasts, project, slug, &label_text)
                            })
                            .inner
                        },
                    )
                    .inner;
                self.remember_open_menu(&menu_key, open);
            }
            let label_resp = rui
                .allocate_new_ui(
                    egui::UiBuilder::new()
                        .max_rect(row_label_rect(rect, true))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    |ui| {
                        // Status dot inside the 24px tree indent: the label's
                        // x is unchanged when the overflow button appears.
                        ui.add_space(12.0);
                        let (dot_rect, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, ROW_H), egui::Sense::hover());
                        status_dot(ui, dot_rect, display_state);
                        ui.add(
                            egui::Label::new(
                                RichText::new(&label_text).size(13.5).color(theme::TEXT),
                            )
                            .sense(egui::Sense::click())
                            .truncate(),
                        )
                    },
                )
                .inner;
            if label_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if click.clicked() || label_resp.clicked() {
                state.select_mission(project, slug);
            }
            click.on_hover_text(format!(
                "{project} · agent={} · {}",
                mission.agent,
                display_state.label()
            ));
        }
        if tree.missions.is_empty() {
            row_hint(ui, 24.0, "no missions — press +");
        }
    }

    /// One-click mission creation: no modal. Agent = the sidebar-
    /// selected agent (when the project is already selected), else
    /// `operator` if present, else the first agent (refuses with a toast
    /// when the project has none). Pins = the project's top-bar pins.
    /// Creates, selects, and launches. A `+` on
    /// a non-selected project's group selects that project first.
    fn new_mission(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str) {
        if state.effective_project().as_deref() != Some(project) {
            state.select_project(project);
        }
        let project = project.to_string();
        let agent = state
            .selected_agent
            .as_ref()
            .filter(|a| state.agents.iter().any(|(s, _)| s == *a))
            .cloned()
            .or_else(|| {
                state
                    .agents
                    .iter()
                    .find(|(s, _)| s == "operator")
                    .map(|(s, _)| s.clone())
            })
            .or_else(|| state.agents.first().map(|(s, _)| s.clone()));
        let Some(agent) = agent else {
            toast(
                toasts,
                ToastKind::Warning,
                "no agents on this project — create one first",
            );
            return;
        };
        match state.create_mission(&project, &agent, "") {
            Ok(slug) => {
                state.refresh_missions(&project);
                // Launch owns the success feedback; creation and launch are
                // one operator action.
                let _ = mission_actions::launch(state, toasts, &project, &slug);
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// The mission-row `⋮` menu: Rename…, Delete. Delete owns any live-run
    /// teardown and transcript export. Operates on the mission record of the
    /// row's OWN project (tree rows can belong to a non-selected
    /// project), so it works regardless of the view.
    fn mission_menu(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        name: &str,
    ) -> bool {
        egui::menu::menu_custom_button(ui, overflow_button(), |ui| {
            if ui.button("Rename…").clicked() {
                self.rename_mission_project = Some(project.to_string());
                self.rename_mission = Some(slug.to_string());
                self.rename_name = name.to_string();
                ui.close_menu();
            }
            if ui
                .add_enabled(
                    state.mission_delete_available(project, slug),
                    egui::Button::new(RichText::new("Delete…").color(theme::SIGNAL_RED)),
                )
                .clicked()
            {
                mission_actions::delete(state, toasts, project, slug);
                ui.close_menu();
            }
        })
        .inner
        .is_some()
    }

    fn remember_open_menu(&mut self, key: &str, open: bool) {
        if open {
            self.open_row_menu = Some(key.to_string());
        } else if self.open_row_menu.as_deref() == Some(key) {
            self.open_row_menu = None;
        }
    }

    fn delete_project_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some((slug, name)) = self.delete_project.clone() else {
            return;
        };
        let mut open = true;
        let mut finished = false;
        theme::dialog(ui.ctx(), "sidebar_delete_project", "Delete project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("Delete project “{name}”?"));
                ui.weak("Its agents, missions, runs, and corpus are removed. There is no undo.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::destructive_button(ui, "Delete project").clicked() {
                        delete_project(state, toasts, &slug);
                        finished = true;
                    }
                    if theme::house_button(ui, "Cancel").clicked() {
                        finished = true;
                    }
                });
            });
        if !open || finished {
            self.delete_project = None;
        }
    }

    fn delete_agent_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some((project, slug, name)) = self.delete_agent.clone() else {
            return;
        };
        let mut open = true;
        let mut finished = false;
        theme::dialog(ui.ctx(), "sidebar_delete_agent", "Delete agent")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("Delete agent “{name}”?"));
                ui.weak("Its configuration is removed from this project. There is no undo.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::destructive_button(ui, "Delete agent").clicked() {
                        match state.delete_agent(&project, &slug) {
                            Ok(()) => {
                                toast(toasts, ToastKind::Success, "agent deletion started");
                                if state.effective_project().as_deref() == Some(project.as_str())
                                    && state.selected_agent.as_deref() == Some(slug.as_str())
                                {
                                    state.selected_agent = None;
                                }
                                state.refresh();
                                finished = true;
                            }
                            Err(error) => {
                                toast(toasts, ToastKind::Error, error.to_string());
                            }
                        }
                    }
                    if theme::house_button(ui, "Cancel").clicked() {
                        finished = true;
                    }
                });
            });
        if !open || finished {
            self.delete_agent = None;
        }
    }

    /// The project Rename… modal: sets the project's display LABEL. The slug
    /// is its identity — directory name, and the key every agent, mission,
    /// run dir, pin and chat session is filed under — so it never moves.
    fn rename_project_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = self.rename_project.clone() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        theme::dialog(ui.ctx(), "sidebar_rename_project", "Rename project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -120.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name");
                let entry = ui.text_edit_singleline(&mut self.rename_project_name);
                ui.label(
                    egui::RichText::new(format!("id stays `{slug}`"))
                        .small()
                        .color(theme::TEXT_FAINT),
                );
                ui.add_space(8.0);
                let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let named = !self.rename_project_name.trim().is_empty();
                let clicked = ui
                    .add_enabled_ui(named, |ui| theme::house_button(ui, "Rename"))
                    .inner
                    .clicked();
                if (clicked || (submit && named))
                    && match state.rename_project(&slug, &self.rename_project_name) {
                        Ok(_) => {
                            toast(toasts, ToastKind::Success, "project renamed");
                            true
                        }
                        Err(error) => {
                            toast(toasts, ToastKind::Error, error.to_string());
                            false
                        }
                    }
                {
                    // The sidebar rows, the chat header and the project view
                    // all read the label off the store — one refresh repaints
                    // every one of them.
                    state.refresh();
                    done = true;
                }
            });
        if !open || done {
            self.rename_project = None;
        }
    }

    /// The Rename… modal: sets the record's display name (keeps the slug).
    fn rename_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = self.rename_mission.clone() else {
            return;
        };
        let Some(project) = self.rename_mission_project.clone() else {
            self.rename_mission = None;
            return;
        };
        let mut open = true;
        theme::dialog(ui.ctx(), "sidebar_rename_mission", "Rename mission")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -120.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (the slug stays as the id)");
                let entry = ui.text_edit_singleline(&mut self.rename_name);
                ui.add_space(8.0);
                let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if theme::house_button(ui, "Rename").clicked() || submit {
                    match state.rename_mission(&project, &slug, &self.rename_name) {
                        Ok(()) => {
                            state.refresh_missions(&project);
                            self.rename_mission = None;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        if !open {
            self.rename_mission = None;
        }
    }

    /// The pinned footer: a caption row — `Corpus` on the left, the
    /// `{files} · {bytes}` count on the right — over a full-width
    /// segmented bar of the knowledge categories (same colors as the
    /// project view, no legend). Mission logs are excluded, matching the
    /// count. Self-updating: the corpus is re-walked on a throttle
    /// (`poll_project_scope`), so there is no refresh button.
    fn footer(&mut self, ui: &mut Ui, state: &mut AppState) {
        // Corpus-only totals: mission logs live in the project view's own
        // section, never folded into the line that says `Corpus`.
        let (files, bytes, categories) = match state.corpus_stats() {
            Some(stats) => (
                stats.knowledge_files(),
                stats.knowledge_bytes(),
                stats.categories.clone(),
            ),
            None => (0, 0, Vec::new()),
        };

        // A hairline across the top edge divides the footer from the
        // scrolling tree above it.
        let top = ui.max_rect();
        ui.painter().hline(
            top.left()..=top.right(),
            top.top(),
            egui::Stroke::new(1.0_f32, theme::HAIRLINE),
        );

        ui.add_space(10.0);
        // Caption row: label left, count right — one clean line.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Corpus").size(12.0).color(theme::TEXT_MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(fmt_bytes(bytes))
                        .size(11.0)
                        .color(theme::TEXT_FAINT),
                );
                ui.label(RichText::new("·").size(11.0).color(theme::TEXT_FAINT));
                ui.label(
                    RichText::new(format!("{files} files"))
                        .size(11.5)
                        .color(theme::TEXT_MUTED),
                );
            });
        });
        ui.add_space(6.0);
        corpus_strip(ui, &categories, bytes);
    }

    // --- create flows (modal windows) ---

    fn create_project_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        if !self.create_project {
            return;
        }
        let mut open = self.create_project;
        let mut done = false;
        theme::dialog(ui.ctx(), "sidebar_new_project", "New project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (the id is generated)");
                let entry = ui.text_edit_singleline(&mut self.create_name);
                ui.label("Environment plugin");
                plugin_picker(
                    ui,
                    &mut self.create_plugin,
                    state.plugins(),
                    &mut self.needs_probe,
                );
                ui.add_space(8.0);
                let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if theme::house_button(ui, "Create").clicked() || submit {
                    let name = self.create_name.trim();
                    if name.is_empty() {
                        toast(toasts, ToastKind::Warning, "display name is required");
                    } else {
                        match state.create_project(name, self.create_plugin.trim()) {
                            Ok((id, _)) => {
                                toast(toasts, ToastKind::Success, "project created");
                                state.refresh();
                                state.select_project(&id);
                                // Land on the new project's page, not
                                // wherever the operator happened to be.
                                state.current_screen = Screen::Projects;
                                self.create_name.clear();
                                done = true;
                            }
                            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                        }
                    }
                }
            });
        self.create_project = open && !done;
    }

    fn new_agent_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        if !self.new_agent {
            return;
        }
        let mut open = self.new_agent;
        let mut done = false;
        theme::dialog(ui.ctx(), "sidebar_new_agent", "New agent")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -120.0))
            .show(ui.ctx(), |ui| {
                ui.label("Role");
                egui::ComboBox::from_id_salt("agent_role")
                    .selected_text(format!(
                        "{} — {}",
                        self.agent_role.as_str(),
                        crate::views::policy::short_description(self.agent_role)
                    ))
                    .show_ui(ui, |ui| {
                        for role in corpus_core::AgentRole::ALL {
                            ui.selectable_value(
                                &mut self.agent_role,
                                role,
                                format!(
                                    "{:<10}  {}",
                                    role.as_str().to_uppercase(),
                                    crate::views::policy::short_description(role)
                                ),
                            );
                        }
                    });
                ui.add_space(8.0);
                let submit = ui.input(|i| i.key_pressed(egui::Key::Enter));
                if theme::house_button(ui, "Create").clicked() || submit {
                    let project = if self.new_agent_project.is_empty() {
                        state.effective_project().unwrap_or_default()
                    } else {
                        self.new_agent_project.clone()
                    };
                    if project.is_empty() {
                        toast(toasts, ToastKind::Warning, "create a project first");
                        self.new_agent = false;
                        return;
                    }
                    match state.create_agent_with_role(&project, self.agent_role) {
                        Ok(_) => {
                            toast(toasts, ToastKind::Success, "agent created");
                            state.refresh_agents(&project);
                            done = true;
                        }
                        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                    }
                }
            });
        self.new_agent = open && !done;
    }

    fn prep_clone(&mut self, slug: String) {
        self.clone_from = Some(slug);
        self.clone_name.clear();
        self.clone_corpus = false;
        self.show_clone = true;
    }

    fn clone_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        if !self.show_clone {
            return;
        }
        let Some(from) = self.clone_from.clone() else {
            self.show_clone = false;
            return;
        };
        let mut open = self.show_clone;
        let mut done = false;
        theme::dialog(
            ui.ctx(),
            "sidebar_clone_project",
            format!("Clone project: {from}"),
        )
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -100.0))
        .show(ui.ctx(), |ui| {
            ui.label("Display name (optional — defaults to the source's)");
            let entry = ui.text_edit_singleline(&mut self.clone_name);
            ui.checkbox(&mut self.clone_corpus, "copy the shared corpus");
            ui.add_space(8.0);
            let submit = entry.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if theme::house_button(ui, "Clone").clicked() || submit {
                let name = if self.clone_name.trim().is_empty() {
                    None
                } else {
                    Some(self.clone_name.trim())
                };
                match state.clone_project(&from, name, self.clone_corpus) {
                    Ok((to, _)) => {
                        toast(toasts, ToastKind::Success, "project cloned");
                        state.refresh();
                        state.select_project(&to);
                        done = true;
                    }
                    Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
                }
            }
        });
        self.show_clone = open && !done;
    }
}

/// A full-width selectable row: paints the selected/hover fill edge-to-edge
/// (spec §4), wires the pointer cursor, and returns a child `Ui` for the
/// row's content plus a row-band click response.
struct Row {
    ui: egui::Ui,
    rect: egui::Rect,
    /// The row-band click response — the full row MINUS the reserved kebab
    /// strip when the row carries a menu, so the row click and the kebab
    /// button never overlap interact regions. `clicked()` = the row action.
    click: egui::Response,
    /// Whether the pointer is over the row band.
    hovered: bool,
}

/// Width of the right-hand strip reserved for a row's kebab menu button.
const KEBAB_STRIP: f32 = 28.0;

/// Lay out one sidebar row: paints ROW_HL (selected) or ROW_HOVER (hovered)
/// full-panel-width (expand-by-PANEL_MARGIN like the old ROW_HL), sets the
/// pointer cursor on hover, and returns a [`Row`]. `has_kebab` reserves the
/// right strip so its click region never overlaps the kebab's button rect.
/// `id_seed` is the row's unique slug/title — stable across scrolling.
fn row_ui(ui: &mut Ui, selected: bool, has_kebab: bool, id_seed: impl std::hash::Hash) -> Row {
    let full = ui.available_width();
    let (rect, band) = ui.allocate_exact_size(egui::vec2(full, ROW_H), egui::Sense::hover());
    let hovered = band.hovered();
    if selected || hovered {
        // Edge-to-edge fill: expand past the panel margin so the band runs to
        // the panel's actual edges. Painted on the PARENT painter (clip
        // widened to the band) before the child exists.
        let fill = if selected {
            theme::ROW_HL
        } else {
            theme::ROW_HOVER
        };
        let band_rect = rect.expand2(egui::vec2(theme::PANEL_MARGIN, 0.0));
        let mut painter = ui.painter().clone();
        painter.set_clip_rect(band_rect);
        painter.rect_filled(band_rect, egui::CornerRadius::ZERO, fill);
        if selected {
            painter.rect_filled(
                egui::Rect::from_min_size(band_rect.min, egui::vec2(2.0, band_rect.height())),
                egui::CornerRadius::ZERO,
                theme::INTERACTION,
            );
        }
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let click_rect = if has_kebab {
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - KEBAB_STRIP, rect.max.y))
    } else {
        rect
    };
    let click = ui.interact(
        click_rect,
        ui.id().with(("row", id_seed)),
        egui::Sense::click(),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Clip the row's child so nothing it paints can ever overlap a
    // neighbouring row (defect 1c).
    child.set_clip_rect(rect);
    Row {
        ui: child,
        rect,
        click,
        hovered,
    }
}

/// The overflow strip is reserved on every actionable row, whether its
/// button is currently visible or not. This keeps truncation and label
/// positions stable across hover transitions.
fn row_label_rect(rect: egui::Rect, has_kebab: bool) -> egui::Rect {
    let right = if has_kebab {
        rect.max.x - KEBAB_STRIP
    } else {
        rect.max.x
    };
    egui::Rect::from_min_max(rect.min, egui::pos2(right.max(rect.min.x), rect.max.y))
}

fn row_menu_visible(selected: bool, hovered: bool, focused: bool, open: bool) -> bool {
    selected || hovered || focused || open
}

fn overflow_button() -> egui::Button<'static> {
    egui::Button::new(theme::icon_text(
        ph::DOTS_THREE_VERTICAL,
        16.0,
        theme::TEXT_MUTED,
    ))
    .frame(false)
}

/// Static mission status language: gray is idle, amber is a transition,
/// bright/dim green distinguish working from waiting, and red requires
/// attention or marks destructive teardown.
fn status_dot(ui: &Ui, rect: egui::Rect, state: MissionDisplayState) {
    let center = rect.center();
    match state {
        MissionDisplayState::Working => {
            ui.painter()
                .circle_filled(center, 5.0, theme::HEALTHY.gamma_multiply(0.20));
            ui.painter().circle_filled(center, 2.2, theme::HEALTHY);
        }
        MissionDisplayState::Waiting => {
            ui.painter()
                .circle_filled(center, 2.2, theme::HEALTHY.gamma_multiply(0.55));
        }
        MissionDisplayState::Queued
        | MissionDisplayState::Preparing
        | MissionDisplayState::Starting
        | MissionDisplayState::Stopping
        | MissionDisplayState::Exporting => {
            ui.painter().circle_filled(center, 2.2, theme::INTERACTION);
        }
        MissionDisplayState::Failed | MissionDisplayState::Deleting => {
            ui.painter().circle_filled(center, 2.2, theme::SIGNAL_RED);
        }
        MissionDisplayState::Idle => {
            ui.painter().circle_filled(center, 2.2, theme::TEXT_FAINT);
        }
    }
}

/// The sidebar's compact corpus bar: a full-width strip segmented by each
/// knowledge category's byte share, palette-colored to match the project
/// view (no legend — the colors carry it). Hover a segment for its
/// files/bytes. An empty corpus paints just the faint plate.
fn corpus_strip(ui: &mut Ui, categories: &[corpus_core::CategoryStat], total: u64) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 9.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::PLATE_FRONT);
    if total == 0 || categories.is_empty() {
        return;
    }
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
        // Round only the outer ends so the strip reads as one pill.
        let rounding = if i == 0 {
            egui::CornerRadius {
                nw: 2,
                sw: 2,
                ne: 0,
                se: 0,
            }
        } else if i == categories.len() - 1 {
            egui::CornerRadius {
                nw: 0,
                sw: 0,
                ne: 2,
                se: 2,
            }
        } else {
            egui::CornerRadius::ZERO
        };
        painter.rect_filled(seg, rounding, color);
        // Hairline gap so adjacent segments stay distinct.
        painter.rect_stroke(
            seg,
            rounding,
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
}

/// A dim hint row (empty-list / placeholder), matching the row padding.
fn row_hint(ui: &mut Ui, indent: f32, text: &str) {
    let Row { ui: mut rui, .. } = row_ui(ui, false, false, ("hint", text));
    rui.add_space(indent);
    rui.add(egui::Label::new(RichText::new(text).size(12.0).color(theme::TEXT_FAINT)).truncate());
}

/// A tree group's mini-header: the dim label (11px, faint) — clickable,
/// routes to the group's screen — with a small right-aligned `+`.
/// Returns `(header_clicked, plus_clicked)`.
fn mini_header(ui: &mut Ui, id: &str, title: &str) -> (bool, bool) {
    let mut plus = false;
    let Row {
        ui: mut rui,
        click,
        hovered,
        ..
    } = row_ui(ui, false, false, ("mini", id));
    rui.add_space(24.0);
    let color = if hovered {
        theme::TEXT_MUTED
    } else {
        theme::TEXT_FAINT
    };
    let label = rui.add(
        egui::Label::new(RichText::new(title).size(11.0).color(color))
            .sense(egui::Sense::click())
            .truncate(),
    );
    if label.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    rui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if theme::icon_button(ui, ph::PLUS, 13.0).clicked() {
            plus = true;
        }
    });
    (click.clicked() || label.clicked(), plus)
}

/// A section header (spec §4): the title in REGULAR case, 13px TEXT_MUTED
/// (brightens to TEXT on hover — FIX 2c), clickable (routes the screen),
/// with a right-aligned `plus` icon button, and a full-panel-width hairline
/// rule directly beneath the header band (defect 3a). Returns
/// `(header_clicked, plus_clicked)`.
fn section_header(ui: &mut Ui, title: &str) -> (bool, bool) {
    let mut plus = false;
    let Row {
        ui: mut rui,
        rect: row,
        click,
        hovered,
    } = row_ui(ui, false, false, title);
    let color = if hovered {
        theme::TEXT
    } else {
        theme::TEXT_MUTED
    };
    rui.add_space(8.0);
    // The label senses clicks and unions with the band: title text and
    // padding route identically (a hover-only label swallows both).
    let label = rui.add(
        egui::Label::new(RichText::new(title).size(13.0).color(color))
            .sense(egui::Sense::click())
            .truncate(),
    );
    if label.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    rui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if theme::icon_button(ui, ph::PLUS, 16.0).clicked() {
            plus = true;
        }
    });
    let header = click.clicked() || label.clicked();
    // Full-panel-width hairline directly under the header band — the band is
    // expanded past the panel margin so the rule reaches the panel edges
    // (not the margin-inset available width).
    let band = row.expand2(egui::vec2(theme::PANEL_MARGIN, 0.0));
    let mut hairline = ui.painter().clone();
    hairline.set_clip_rect(band);
    let y = band.max.y - 1.0;
    hairline.line_segment(
        [egui::pos2(band.min.x, y), egui::pos2(band.max.x, y)],
        egui::Stroke::new(1.0_f32, theme::HAIRLINE),
    );
    (header, plus)
}

/// Delete a project from the project-row menu; the default-project refusal
/// bubbles
/// up as a toast (the operator never loses `default`).
fn delete_project(state: &mut AppState, toasts: &mut Toasts, slug: &str) {
    let deleting_selected = state.effective_project().as_deref() == Some(slug);
    match state.delete_project(slug) {
        Ok(()) => {
            toast(toasts, ToastKind::Success, "project deletion started");
            state.refresh();
            if deleting_selected {
                // ensure_selection re-picks a project next frame. Deleting a
                // different row must not move the operator off their page.
                state.selected_project = None;
            }
        }
        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_rows_reveal_overflow_for_every_interactive_state() {
        assert!(!row_menu_visible(false, false, false, false));
        assert!(row_menu_visible(true, false, false, false));
        assert!(row_menu_visible(false, true, false, false));
        assert!(row_menu_visible(false, false, true, false));
        assert!(row_menu_visible(false, false, false, true));
    }

    #[test]
    fn actionable_row_label_width_does_not_change_on_hover() {
        let row = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(240.0, ROW_H));
        let label = row_label_rect(row, true);
        assert_eq!(label.left(), row.left());
        assert_eq!(label.right(), row.right() - KEBAB_STRIP);
        assert_eq!(label.height(), row.height());
    }

    #[test]
    fn open_menu_identity_persists_until_that_menu_closes() {
        let mut sidebar = Sidebar::default();
        sidebar.remember_open_menu("agent:p:a", true);
        assert_eq!(sidebar.open_row_menu.as_deref(), Some("agent:p:a"));

        // A closed sibling must not clear the active popup.
        sidebar.remember_open_menu("mission:p:m", false);
        assert_eq!(sidebar.open_row_menu.as_deref(), Some("agent:p:a"));

        sidebar.remember_open_menu("agent:p:a", false);
        assert!(sidebar.open_row_menu.is_none());
    }
}
