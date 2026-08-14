//! The left sidebar (app-flow chunk 1, app-parity-spec §4): three scoped
//! sections — Projects, Agents, Missions — each a flat list of the
//! selected project's entries with a `+` icon create button; the selected
//! row gets a full-width ROW_HL fill (text stays TEXT); the selected
//! PROJECT row additionally carries a `dots_three_vertical` menu (Clone /
//! Delete); bottom-left the fixed corpus summary (walked via corpus-core
//! `corpus_stats`, refreshed on selection change + a refresh icon).
//!
//! The sidebar IS the navigation: clicking a section header or an entry
//! routes the main column to the matching screen. Selection state lives on
//! `AppState` (`selected_project`, the scoped caches) — the sidebar only
//! renders it and requests actions. The `+` create flows (project /
//! agent-clone-from-seed / mission-with-pins) are small modal windows the
//! sidebar owns.

use std::time::Duration;

use egui::{Align2, RichText, Ui};
use egui_phosphor::regular as ph;
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::fmt::fmt_bytes;
use crate::nav::Screen;
use crate::state::AppState;
use crate::theme;
use crate::views::plugin_picker::plugin_picker;

/// Row height for a sidebar list row (15px text + 5px vertical padding).
const ROW_H: f32 = 24.0;

/// Widget state for the sidebar: its three `+` create flows, the project
/// clone dialog, and the on-demand plugin probe.
pub struct Sidebar {
    create_project: bool,
    create_name: String,
    create_plugin: String,
    new_agent: bool,
    agent_seed: String,
    clone_from: Option<String>,
    show_clone: bool,
    clone_name: String,
    clone_corpus: bool,
    /// The mission being renamed (Mission menu -> Rename…).
    rename_mission: Option<String>,
    rename_name: String,
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
            agent_seed: "operator".to_string(),
            clone_from: None,
            show_clone: false,
            clone_name: String::new(),
            clone_corpus: false,
            rename_mission: None,
            rename_name: String::new(),
            needs_probe: false,
        }
    }
}

impl Sidebar {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let max = ui.available_rect_before_wrap();
        let footer_h = 44.0;
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
        let footer_rect = egui::Rect::from_min_max(
            egui::pos2(max.min.x, max.max.y - footer_h),
            max.max,
        );
        let mut footer_ui = ui.new_child(egui::UiBuilder::new().max_rect(footer_rect));
        self.footer(&mut footer_ui, state);

        // Drain a requested plugin re-probe before the modals render.
        if self.needs_probe {
            state.refresh_plugins();
            self.needs_probe = false;
        }

        self.create_project_window(ui, state, toasts);
        self.new_agent_window(ui, state, toasts);
        self.rename_window(ui, state, toasts);
        self.clone_window(ui, state, toasts);
    }

    fn sections(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        ui.add_space(4.0);
        self.section_projects(ui, state, toasts);
        ui.add_space(18.0);
        self.section_agents(ui, state);
        ui.add_space(18.0);
        self.section_missions(ui, state, toasts);
    }

    fn section_projects(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
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
        for (slug, project) in &projects {
            let is_sel = selected.as_deref() == Some(slug.as_str());
            // Only the selected PROJECT row carries the Clone/Delete kebab.
            let Row { ui: mut rui, rect, click, .. } = row_ui(ui, is_sel, is_sel, slug);
            // Row label is the display NAME only (slug falls back when the
            // name is empty); the slug moves to the hover text (defect 1a).
            let name = if project.name.is_empty() {
                slug.clone()
            } else {
                project.name.clone()
            };
            let text = RichText::new(&name).size(15.0).color(theme::TEXT);
            let (_label_resp, _kebab_w) = if is_sel {
                // Render the right-aligned kebab zone FIRST, then the
                // truncated label so truncation measures the remaining width
                // and never collides with the icon (defect 1d).
                let kebab =
                    rui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::menu::menu_custom_button(
                            ui,
                            egui::Button::new(theme::icon_text(
                                ph::DOTS_THREE_VERTICAL,
                                16.0,
                                theme::TEXT_MUTED,
                            ))
                            .frame(false),
                            |ui| {
                                if ui.button("Clone…").clicked() {
                                    self.prep_clone(slug.clone());
                                    ui.close_menu();
                                }
                                if ui.button("Delete").clicked() {
                                    delete_project(state, toasts, slug);
                                    ui.close_menu();
                                }
                            },
                        );
                    });
                let kebab_w = kebab.response.rect.width();
                let fill = rect.width() - 8.0 - kebab_w - 8.0;
                let label_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x, rect.min.y),
                    egui::vec2(fill.max(0.0), rect.height()),
                );
                let resp = rui
                    .allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(label_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        |ui| {
                            ui.add_space(8.0); // left text inset
                            ui.add(egui::Label::new(text).truncate())
                        },
                    )
                    .inner;
                (resp, kebab_w)
            } else {
                rui.add_space(8.0); // left text inset
                let resp = rui.add(egui::Label::new(text).truncate());
                (resp, 0.0)
            };
            if click.clicked() {
                state.select_project(slug);
                state.current_screen = Screen::Projects;
            }
            click.on_hover_text(format!("{name} · {slug}"));
        }
        if state.projects.is_empty() {
            row_hint(ui, "no projects — press +");
        }
    }

    fn section_agents(&mut self, ui: &mut Ui, state: &mut AppState) {
        let (header, plus) = section_header(ui, "Agents");
        if header {
            state.current_screen = Screen::Agents;
        }
        if plus {
            self.new_agent = true;
        }
        ui.add_space(2.0);
        let on_screen = state.current_screen == Screen::Agents;
        for (slug, agent) in &state.agents {
            let is_sel = on_screen && state.selected_agent.as_deref() == Some(slug.as_str());
            let Row { ui: mut rui, click, .. } = row_ui(ui, is_sel, false, slug);
            rui.add_space(8.0);
            rui.add(
                egui::Label::new(RichText::new(slug.clone()).size(15.0).color(theme::TEXT))
                    .truncate(),
            );
            if click.clicked() {
                state.selected_agent = Some(slug.clone());
                state.current_screen = Screen::Agents;
            }
            click.on_hover_text(&agent.meta.name);
        }
        if state.agents.is_empty() {
            row_hint(ui, "no agents — press +");
        }
    }

    fn section_missions(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let (header, plus) = section_header(ui, "Missions");
        if header {
            state.current_screen = Screen::Missions;
        }
        if plus {
            self.new_mission(state, toasts);
        }
        ui.add_space(2.0);
        let on_screen = state.current_screen == Screen::Missions;
        // Clone the list: the row menu calls `&mut state` (abort/dismiss/rename).
        let missions = state.missions.clone();
        for (slug, mission) in &missions {
            let is_sel = on_screen && state.selected_mission.as_deref() == Some(slug.as_str());
            let label = if mission.name.as_deref().is_some_and(|n| !n.is_empty()) {
                mission.name.clone().unwrap_or_default()
            } else {
                "new".to_string()
            };
            let live = mission
                .session
                .as_ref()
                .is_some_and(|s| state.live_sessions.iter().any(|l| l == s));
            // A mission row always reserves the kebab strip (⋮ shown on the
            // selected row and on row hover); the strip keeps its own
            // unambiguous click area.
            let Row { ui: mut rui, rect, click, hovered } = row_ui(ui, is_sel, true, slug);
            let (_label_resp, _menu_w) = if is_sel || hovered {
                let menu_w = rui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.mission_menu(ui, state, toasts, slug, live, &label)
                    })
                    .response
                    .rect
                    .width();
                let fill = (rect.width() - 8.0 - menu_w - 8.0).max(0.0);
                let label_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x, rect.min.y),
                    egui::vec2(fill, rect.height()),
                );
                let resp = rui
                    .allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(label_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                        |ui| {
                            ui.add_space(8.0); // left text inset
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&label).size(15.0).color(theme::TEXT),
                                )
                                .truncate(),
                            )
                        },
                    )
                    .inner;
                (resp, menu_w)
            } else {
                rui.add_space(8.0);
                let resp = rui.add(egui::Label::new(RichText::new(&label)).truncate());
                (resp, 0.0)
            };
            if click.clicked() {
                state.selected_mission = Some(slug.clone());
                state.current_screen = Screen::Missions;
            }
            click.on_hover_text(format!("agent={} · {}", mission.agent, mission.status));
        }
        if state.missions.is_empty() {
            row_hint(ui, "no missions — press +");
        }
    }

    /// One-click mission create + launch: no modal. Agent = the sidebar-
    /// selected agent, else `operator` if present, else the first agent
    /// (refuses with a toast when the project has none). Pins = the current
    /// top-bar pins. Creates, selects, and auto-launches on the mission view.
    fn new_mission(&mut self, state: &mut AppState, toasts: &mut Toasts) {
        let Some(project) = state.effective_project() else {
            toast(toasts, ToastKind::Warning, "create a project first");
            return;
        };
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
            toast(toasts, ToastKind::Warning, "no agents on this project — create one first");
            return;
        };
        match state.create_mission(&project, &agent, "") {
            Ok(slug) => {
                state.refresh_missions(&project);
                state.selected_mission = Some(slug.clone());
                state.pending_launch = Some(slug.clone());
                state.current_screen = Screen::Missions;
                toast(
                    toasts,
                    ToastKind::Success,
                    format!("launched {agent} on {project} — new mission"),
                );
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// The mission-row `⋮` menu: Abort run / Dismiss (gated on a live
    /// session), Rename…, Delete. Operates on the mission record, so it
    /// works regardless of the view.
    fn mission_menu(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        slug: &str,
        live: bool,
        name: &str,
    ) -> egui::Response {
        egui::menu::menu_custom_button(
            ui,
            egui::Button::new(theme::icon_text(
                ph::DOTS_THREE_VERTICAL,
                16.0,
                theme::TEXT_MUTED,
            ))
            .frame(false),
            |ui| {
                let abort = ui.add_enabled(live, egui::Button::new("Abort run"));
                if abort.clicked() {
                    abort_mission(state, toasts, slug);
                    ui.close_menu();
                }
                let dismiss = ui.add_enabled(live, egui::Button::new("Dismiss"));
                if dismiss.clicked() {
                    dismiss_mission(state, toasts, slug);
                    ui.close_menu();
                }
                if ui.button("Rename…").clicked() {
                    self.rename_mission = Some(slug.to_string());
                    self.rename_name = name.to_string();
                    ui.close_menu();
                }
                if ui.button("Delete").clicked() {
                    delete_mission(state, toasts, slug, state.selected_mission.as_deref() == Some(slug));
                    ui.close_menu();
                }
            },
        )
        .response
    }

    /// The Rename… modal: sets the record's display name (keeps the slug).
    fn rename_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(slug) = self.rename_mission.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Rename mission")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -120.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (the slug stays as the id)");
                ui.text_edit_singleline(&mut self.rename_name);
                ui.add_space(8.0);
                if theme::house_button(ui, "Rename").clicked() {
                    match state.rename_mission(
                        &state.effective_project().unwrap_or_default(),
                        &slug,
                        &self.rename_name,
                    ) {
                        Ok(()) => {
                            if let Some(project) = state.effective_project() {
                                state.refresh_missions(&project);
                            }
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

    /// The pinned footer (spec §4): ↔ `arrow_clockwise` (TEXT_FAINT, hover
    /// TEXT_MUTED) + `Corpus` 12px TEXT_FAINT on the left; right-aligned two
    /// stacked lines — `{files} files` 13px TEXT over `{bytes}` 11px
    /// TEXT_FAINT.
    fn footer(&mut self, ui: &mut Ui, state: &mut AppState) {
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let refresh =
                    theme::icon_button(ui, ph::ARROW_CLOCKWISE, 16.0)
                        .on_hover_text("re-walk the corpus");
                if refresh.clicked() {
                    if let Some(project) = state.effective_project() {
                        state.refresh_corpus_stats(&project);
                    }
                }
                ui.add(
                    egui::Label::new(
                        RichText::new("Corpus").size(12.0).color(theme::TEXT_FAINT),
                    ),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::RIGHT), |ui| {
                        let (files, bytes) = match state.corpus_stats() {
                            Some(stats) => (stats.files, fmt_bytes(stats.bytes)),
                            None => (0, "–".to_string()),
                        };
                        ui.label(
                            RichText::new(format!("{files} files"))
                                .size(13.0)
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(bytes).size(11.0).color(theme::TEXT_FAINT),
                        );
                    });
                });
            });
        });
    }

    // --- create flows (modal windows) ---

    fn create_project_window(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let mut open = self.create_project;
        let mut done = false;
        egui::Window::new("New project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (the id is generated)");
                ui.text_edit_singleline(&mut self.create_name);
                ui.label("Environment plugin");
                plugin_picker(ui, &mut self.create_plugin, state.plugins(), &mut self.needs_probe);
                ui.add_space(8.0);
                if theme::house_button(ui, "Create").clicked() {
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
                                state.select_project(&id);
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
        let mut open = self.new_agent;
        let mut done = false;
        egui::Window::new("New agent")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -120.0))
            .show(ui.ctx(), |ui| {
                ui.label("Clone from seed");
                egui::ComboBox::from_id_salt("agent_seed")
                    .selected_text(match self.agent_seed.as_str() {
                        "researcher" => "researcher".to_string(),
                        "blank" => "blank".to_string(),
                        _ => "operator".to_string(),
                    })
                    .show_ui(ui, |ui| {
                        for seed in ["operator", "researcher", "blank"] {
                            ui.selectable_value(&mut self.agent_seed, seed.to_string(), seed);
                        }
                    });
                ui.weak(seed_note(&self.agent_seed));
                ui.add_space(8.0);
                if theme::house_button(ui, "Create").clicked() {
                    let Some(project) = state.effective_project() else {
                        toast(toasts, ToastKind::Warning, "create a project first");
                        self.new_agent = false;
                        return;
                    };
                    match state.create_agent_from_seed(&project, &self.agent_seed) {
                        Ok(slug) => {
                            toast(toasts, ToastKind::Success, format!("created agent {project}/{slug}"));
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
        let Some(from) = self.clone_from.clone() else {
            self.show_clone = false;
            return;
        };
        let mut open = self.show_clone;
        let mut done = false;
        egui::Window::new(format!("Clone project: {from}"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, -100.0))
            .show(ui.ctx(), |ui| {
                ui.label("Display name (optional — defaults to the source's)");
                ui.text_edit_singleline(&mut self.clone_name);
                ui.checkbox(&mut self.clone_corpus, "copy the shared corpus");
                ui.add_space(8.0);
                if theme::house_button(ui, "Clone").clicked() {
                    let name = if self.clone_name.trim().is_empty() {
                        None
                    } else {
                        Some(self.clone_name.trim())
                    };
                    match state.clone_project(&from, name, self.clone_corpus) {
                        Ok((to, _)) => {
                            toast(toasts, ToastKind::Success, format!("cloned project {from} -> {to}"));
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
    let (rect, band) =
        ui.allocate_exact_size(egui::vec2(full, ROW_H), egui::Sense::hover());
    let hovered = band.hovered();
    if selected || hovered {
        // Edge-to-edge fill: expand past the panel margin so the band runs to
        // the panel's actual edges. Painted on the PARENT painter (clip
        // widened to the band) before the child exists.
        let fill = if selected { theme::ROW_HL } else { theme::ROW_HOVER };
        let band_rect = rect.expand2(egui::vec2(theme::PANEL_MARGIN, 0.0));
        let mut painter = ui.painter().clone();
        painter.set_clip_rect(band_rect);
        painter.rect_filled(band_rect, egui::CornerRadius::ZERO, fill);
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let click_rect = if has_kebab {
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - KEBAB_STRIP, rect.max.y))
    } else {
        rect
    };
    let click = ui.interact(click_rect, ui.id().with(("row", id_seed)), egui::Sense::click());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    // Clip the row's child so nothing it paints can ever overlap a
    // neighbouring row (defect 1c).
    child.set_clip_rect(rect);
    Row { ui: child, rect, click, hovered }
}

/// A dim hint row (empty-list / placeholder), matching the row padding.
fn row_hint(ui: &mut Ui, text: &str) {
    let Row { ui: mut rui, .. } = row_ui(ui, false, false, "hint");
    rui.add_space(8.0);
    rui.add(
        egui::Label::new(RichText::new(text).size(12.0).color(theme::TEXT_FAINT)).truncate(),
    );
}

/// A section header (spec §4): the title in REGULAR case, 13px TEXT_MUTED
/// (brightens to TEXT on hover — FIX 2c), clickable (routes the screen),
/// with a right-aligned `plus` icon button, and a full-panel-width hairline
/// rule directly beneath the header band (defect 3a). Returns
/// `(header_clicked, plus_clicked)`.
fn section_header(ui: &mut Ui, title: &str) -> (bool, bool) {
    let mut header = false;
    let mut plus = false;
    let Row { ui: mut rui, rect: row, click, hovered } = row_ui(ui, false, false, title);
    let color = if hovered { theme::TEXT } else { theme::TEXT_MUTED };
    rui.add_space(8.0);
    rui.add(egui::Label::new(RichText::new(title).size(13.0).color(color)).truncate());
    rui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if theme::icon_button(ui, ph::PLUS, 16.0).clicked() {
            plus = true;
        }
    });
    if click.clicked() {
        header = true;
    }
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

/// What each seed gives you, for the clone-from-seed picker.
fn seed_note(seed: &str) -> &'static str {
    match seed {
        "researcher" => "read-only research pass (open internet; executes nothing)",
        "blank" => "an empty config — fill it in with the raw JSON editor",
        _ => "the sandbox-executing hunter; verified-finding writer",
    }
}

/// Abort a mission's run (Mission ⋮ -> Abort run): kills the tmux session.
fn abort_mission(state: &mut AppState, toasts: &mut Toasts, slug: &str) {
    let Some(project) = state.effective_project() else {
        return;
    };
    match state.abort_mission(&project, slug) {
        Ok(()) => {
            toast(toasts, ToastKind::Success, format!("aborted mission {slug}"));
            state.refresh_missions(&project);
        }
        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
    }
}

/// Dismiss a mission (Mission ⋮ -> Dismiss): export + close the run,
/// clearing its bookkeeping.
fn dismiss_mission(state: &mut AppState, toasts: &mut Toasts, slug: &str) {
    let Some(project) = state.effective_project() else {
        return;
    };
    match state.dismiss_mission(&project, slug) {
        Ok(()) => {
            toast(
                toasts,
                ToastKind::Success,
                format!("dismissed mission {slug} — transcript exported"),
            );
            state.refresh_missions(&project);
        }
        Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
    }
}

/// Delete a mission record (transcripts stay in the corpus runs/).
fn delete_mission(state: &mut AppState, toasts: &mut Toasts, slug: &str, was_selected: bool) {
    let Some(project) = state.effective_project() else {
        return;
    };
    match state.delete_mission(&project, slug) {
        Ok(()) => {
            toast(toasts, ToastKind::Success, format!("deleted mission {slug}"));
            state.refresh_missions(&project);
            if was_selected {
                state.selected_mission = None; // re-defaults to the next
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