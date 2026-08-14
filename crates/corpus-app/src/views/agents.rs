//! Agent view (app-flow chunk 4): the mock-faithful detail screen for the
//! selected agent — header `Agent: <name>` + New Mission / Clone / Delete
//! top-right + a dim `created:` stamp; the raw `opencode.json` editor in
//! monospace with syntect highlighting (views/json_editor.rs); Save
//! validates core-side (parse + agent-structure + permissions + `{file:}`
//! refs) and only writes when valid — an invalid document shows a red
//! inline banner and is never saved. New Mission + creates a mission for
//! this agent with the current top-bar pins and routes to the Missions
//! view (real launch lands at chunk 5).
//!
//! No business logic here: corpus-core calls go through `AppState`.

use std::time::Duration;

use egui::{RichText, Ui};
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};
use egui_phosphor::regular as ph;

use crate::nav::Screen;
use crate::state::AppState;
use crate::theme;
use crate::views::json_editor;

/// Widget state for the Agent view: the editor buffer + validation banner.
/// The selected agent lives on `AppState`.
pub struct AgentsView {
    /// The agent slug whose config is in `editor_text`; re-load on change.
    viewed_agent: Option<String>,
    /// Raw opencode.json being edited (displayed as highlighted code).
    editor_text: String,
    /// Last save attempt from the core validator; None = clean.
    error: Option<String>,
    dirty: bool,
}

impl Default for AgentsView {
    fn default() -> Self {
        Self {
            viewed_agent: None,
            editor_text: String::new(),
            error: None,
            dirty: true,
        }
    }
}

impl AgentsView {
    pub fn show(&mut self, ui: &mut Ui, state: &mut AppState, toasts: &mut Toasts) {
        let Some(project) = state.effective_project() else {
            ui.add_space(24.0);
            ui.weak("no projects yet — create one from the sidebar");
            return;
        };
        if self.dirty {
            state.refresh_agents(&project);
            self.dirty = false;
        }
        // Ensure a concrete selection: the sidebar picks an agent, else the
        // first on the project (a stale pick for another project re-defaults).
        let selection_stale = state
            .selected_agent
            .as_ref()
            .map(|s| !state.agents.iter().any(|(a, _)| a == s))
            .unwrap_or(true);
        if selection_stale {
            state.selected_agent = state.agents.first().map(|(a, _)| a.clone());
        }
        let Some(slug) = state.selected_agent.clone() else {
            ui.add_space(24.0);
            ui.label(
                RichText::new("No agents yet — create one from the sidebar (+ on Agents).")
                    .weak()
                    .size(17.0),
            );
            return;
        };
        let Some((_, agent)) = state
            .agents
            .iter()
            .find(|(a, _)| a == &slug)
            .cloned()
        else {
            return;
        };

        // (Re)load the editor buffer when the viewed agent changes or a Save
        // rewrote it — the buffer is always the on-disk (pretty) JSON.
        if self.viewed_agent.as_deref() != Some(slug.as_str()) {
            self.viewed_agent = Some(slug.clone());
            self.editor_text = serde_json::to_string_pretty(&agent.doc).unwrap_or_default();
            self.error = None;
        }

        let name = if agent.meta.name.is_empty() || agent.meta.name == slug {
            slug.clone()
        } else {
            format!("{}  ·{slug}", agent.meta.name)
        };

        // --- header (spec §6): `Agent: <slug>` + New Mission / Clone /
        // Delete top-right + created stamp, then a hairline.
        ui.horizontal(|ui| {
            ui.label(theme::screen_header(format!("Agent: {name}")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::destructive_button(ui, "Delete").clicked() {
                    self.delete_agent(state, toasts, &project, &slug);
                }
                if theme::house_button(ui, "Clone").clicked() {
                    self.clone_agent(state, toasts, &project, &slug);
                }
                if theme::house_button(ui, format!("{}  New Mission", ph::PLUS))
                    .clicked()
                {
                    self.new_mission(state, toasts, &project, &slug);
                }
                ui.label(
                    RichText::new(format!("created: {}", fmt_epoch(agent.meta.created)))
                        .size(12.0)
                        .color(theme::TEXT_FAINT),
                );
            });
        });
        theme::hairline(ui);
        ui.add_space(8.0);

        // --- JSON editor (spec §6): monospace 13.5px, fills the width,
        // min height 480, in a Frame (EDITOR_BG fill, 1px HAIRLINE, radius 2).
        egui::Frame::default()
            .fill(theme::EDITOR_BG)
            .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE))
            .corner_radius(egui::CornerRadius::same(2))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_height(480.0);
                let mut layouter = json_editor::layouter;
                egui::ScrollArea::vertical()
                    .id_salt("agent_json")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.editor_text)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(24)
                                .desired_width(f32::INFINITY)
                                .code_editor()
                                .lock_focus(true)
                                .layouter(&mut layouter),
                        );
                    });
            });

        // --- inline validation banner (ABOVE the Save row, DANGER 12px) ---
        if let Some(error) = &self.error {
            ui.add_space(6.0);
            ui.label(RichText::new(error.clone()).size(12.0).color(theme::DANGER));
        }

        // --- Save (validate core-side; never writes invalid) ---
        ui.add_space(8.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
            if theme::house_button(ui, "Save").clicked() {
                self.save(state, toasts, &project, &slug);
            }
        });
    }

    /// Save via the core validator: JSON must parse and the agent document
    /// must satisfy the structural rules (agent map, one primary, valid
    /// permissions, resolvable `{file:}` refs). Invalid → red banner, no write.
    fn save(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
        let doc = match serde_json::from_str::<serde_json::Value>(&self.editor_text) {
            Ok(doc) => doc,
            Err(error) => {
                self.error = Some(format!("invalid JSON: {error}"));
                return;
            }
        };
        match state.save_agent(project, slug, &doc) {
            Ok(()) => {
                self.error = None;
                // Mirror the pretty (on-disk) config back into the buffer.
                self.editor_text = serde_json::to_string_pretty(&doc).unwrap_or_default();
                state.refresh_agents(project);
                toast(toasts, ToastKind::Success, format!("saved agent {project}/{slug}"));
            }
            Err(error) => {
                self.error = Some(error.to_string());
            }
        }
    }

    fn new_mission(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
        // One-click create + launch: a BARE opencode TUI at an empty prompt
        // (the operator types the mission into the TUI).
        match state.create_mission(project, slug, "") {
            Ok(mission) => {
                toast(
                    toasts,
                    ToastKind::Success,
                    format!("mission created {project}/{mission}"),
                );
                state.refresh_missions(project);
                // Select + auto-launch it on the mission view.
                state.selected_mission = Some(mission.clone());
                state.pending_launch = Some(mission.clone());
                state.current_screen = Screen::Missions;
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    fn clone_agent(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
        match state.clone_agent(project, slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, "agent cloned");
                state.refresh_agents(project);
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    fn delete_agent(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
        match state.delete_agent(project, slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, format!("deleted agent {project}/{slug}"));
                state.refresh_agents(project);
                // The view re-defaults to the first remaining agent.
                state.selected_agent = None;
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
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

/// Format epoch seconds as `YYYY-MM-DD HH:MMZ` (UTC). Display-only.
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