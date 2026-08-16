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

/// Which editor the screen is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// Field-by-field forms: each control writes ONE field through a
    /// granular core call, so nothing else in the document is touched.
    Forms,
    /// The raw document, for anything the forms don't cover.
    Json,
}

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
    tab: Tab,
    /// Which entry the forms are editing: None = the primary, Some = a
    /// subagent by entry name.
    entry: Option<String>,
    /// Buffered text fields, flushed to the store on focus loss so every
    /// keystroke isn't a disk write. Keyed by the entry they belong to so
    /// switching entries can never write one's text onto another.
    buffers: Option<FieldBuffers>,
    /// The new-subagent form, when open.
    new_subagent: Option<NewSubagent>,
    /// opencode's model catalog, fetched on demand (a subprocess).
    models: Option<corpus_core::ModelList>,
}

/// Text fields being edited, with the entry they belong to.
struct FieldBuffers {
    entry_key: String,
    description: String,
    prompt: String,
}

#[derive(Default)]
struct NewSubagent {
    name: String,
    description: String,
    prompt: String,
}

impl Default for AgentsView {
    fn default() -> Self {
        Self {
            viewed_agent: None,
            editor_text: String::new(),
            error: None,
            dirty: true,
            tab: Tab::Forms,
            entry: None,
            buffers: None,
            new_subagent: None,
            models: None,
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

        // --- Forms | JSON toggle. Forms edits one field at a time through
        // the granular core calls; JSON is the escape hatch for anything
        // the forms don't model.
        ui.horizontal(|ui| {
            for (tab, label) in [(Tab::Forms, "Forms"), (Tab::Json, "JSON")] {
                let selected = self.tab == tab;
                if ui.selectable_label(selected, RichText::new(label).size(13.0)).clicked()
                    && !selected
                {
                    self.tab = tab;
                    self.error = None;
                    // Re-read on the way into JSON so it shows what the
                    // forms just wrote, and drop stale field buffers.
                    self.viewed_agent = None;
                    self.buffers = None;
                }
            }
        });
        ui.add_space(10.0);

        if self.tab == Tab::Forms {
            self.forms(ui, state, toasts, &project, &slug, &agent);
            return;
        }

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

    /// The Forms tab. Every control writes ONE field through a granular
    /// core call and then refreshes — there is no "form state" to save, so
    /// there is no way to lose an edit by navigating away, and no way for
    /// the form to clobber a field it doesn't display.
    fn forms(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        agent: &corpus_core::AgentConfig,
    ) {
        let Some(entries) = agent.doc.get("agent").and_then(|a| a.as_object()) else {
            ui.label(RichText::new("this agent has no `agent` map — use the JSON tab").color(theme::DANGER));
            return;
        };
        // Entry picker: the primary plus each subagent. Subagents are
        // edited with the SAME controls as the primary.
        let mut subagents: Vec<String> = entries
            .iter()
            .filter(|(name, cfg)| {
                **name != slug
                    && cfg.get("mode").and_then(|m| m.as_str()) == Some("subagent")
            })
            .map(|(name, _)| name.clone())
            .collect();
        subagents.sort();
        // A stale selection (subagent just removed) falls back to primary.
        if self.entry.as_ref().is_some_and(|e| !subagents.contains(e)) {
            self.entry = None;
            self.buffers = None;
        }
        if !subagents.is_empty() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("editing").size(12.0).color(theme::TEXT_MUTED));
                if ui.selectable_label(self.entry.is_none(), slug).clicked() {
                    self.entry = None;
                    self.buffers = None;
                }
                for sub in &subagents {
                    let selected = self.entry.as_deref() == Some(sub.as_str());
                    if ui.selectable_label(selected, sub).clicked() {
                        self.entry = Some(sub.clone());
                        self.buffers = None;
                    }
                }
            });
            ui.add_space(12.0);
        }

        let entry_key = self.entry.clone().unwrap_or_else(|| slug.to_string());
        let Some(cfg) = entries.get(&entry_key).and_then(|c| c.as_object()) else {
            return;
        };
        let is_primary = self.entry.is_none();
        // Re-seed the text buffers when the target changes.
        let stale = self
            .buffers
            .as_ref()
            .is_none_or(|b| b.entry_key != entry_key);
        if stale {
            self.buffers = Some(FieldBuffers {
                entry_key: entry_key.clone(),
                description: cfg
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                prompt: cfg
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            });
        }

        egui::ScrollArea::vertical()
            .id_salt("agent_forms")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.role_section(ui, state, toasts, project, slug, agent, cfg, is_primary);
                ui.add_space(20.0);
                self.model_section(ui, state, toasts, project, slug, cfg);
                ui.add_space(20.0);
                self.text_section(ui, state, toasts, project, slug);
                ui.add_space(20.0);
                if is_primary {
                    self.subagents_section(ui, state, toasts, project, slug, &subagents);
                }
            });
    }

    /// Role: the server-enforced ceiling. Shows what it grants and warns
    /// when the stored permission block disagrees — divergence is silent
    /// at launch (the render derives from the role), so it must not be
    /// silent here.
    #[allow(clippy::too_many_arguments)]
    fn role_section(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        agent: &corpus_core::AgentConfig,
        cfg: &serde_json::Map<String, serde_json::Value>,
        is_primary: bool,
    ) {
        use corpus_core::AgentRole;
        ui.label(theme::section_heading("Role"));
        ui.add_space(8.0);
        let current = if is_primary {
            agent.meta.role()
        } else {
            let sub = self.entry.clone().unwrap_or_default();
            agent
                .meta
                .subagent_roles
                .get(&sub)
                .copied()
                .unwrap_or(agent.meta.role())
                .min(agent.meta.role())
        };
        ui.horizontal(|ui| {
            for role in AgentRole::ALL {
                let selected = current == role;
                if ui
                    .selectable_label(selected, RichText::new(role.as_str()).size(13.0))
                    .on_hover_text(role_hint(role))
                    .clicked()
                    && !selected
                {
                    match state.set_agent_role(project, slug, self.entry.as_deref(), role) {
                        Ok(()) => {
                            toast(toasts, ToastKind::Success, format!("role -> {}", role.as_str()));
                            state.refresh_agents(project);
                        }
                        Err(e) => toast(toasts, ToastKind::Error, e.to_string()),
                    }
                }
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new(format!(
                "grants: {}",
                current
                    .tools()
                    .iter()
                    .map(|t| t.trim_start_matches("corpus_"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
            .size(11.5)
            .color(theme::TEXT_FAINT),
        );
        if !is_primary {
            ui.label(
                RichText::new(format!(
                    "capped by the primary's role ({}) — the server cannot tell a subagent \
                     from its parent at runtime",
                    agent.meta.role().as_str()
                ))
                .size(11.0)
                .color(theme::TEXT_FAINT),
            );
        }

        // Divergence: stored corpus_* allows the role will overrule.
        let diverging: Vec<&str> = corpus_core::CORPUS_TOOLS
            .into_iter()
            .filter(|tool| {
                let stored = cfg
                    .get("permission")
                    .and_then(|p| p.get(*tool))
                    .and_then(|v| v.as_str());
                stored == Some("allow") && !current.allows(tool)
            })
            .collect();
        if !diverging.is_empty() {
            ui.add_space(8.0);
            egui::Frame::default()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0_f32, theme::WARN))
                .corner_radius(egui::CornerRadius::same(2))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "this agent's stored permissions grant {} — the {} role denies \
                             them, and the launch will too",
                            diverging.join(", "),
                            current.as_str()
                        ))
                        .size(11.5)
                        .color(theme::WARN),
                    );
                    ui.add_space(6.0);
                    if theme::house_button(ui, "Rewrite permissions from role").clicked() {
                        let patch: serde_json::Map<String, serde_json::Value> = diverging
                            .iter()
                            .map(|t| (t.to_string(), "deny".into()))
                            .collect();
                        match state.patch_agent_permission(
                            project,
                            slug,
                            self.entry.as_deref(),
                            &serde_json::Value::Object(patch),
                        ) {
                            Ok(()) => {
                                toast(toasts, ToastKind::Success, "permissions match the role");
                                state.refresh_agents(project);
                            }
                            Err(e) => toast(toasts, ToastKind::Error, e.to_string()),
                        }
                    }
                });
        }
    }

    /// Model: opencode's own catalog, so an id here is one a mission can
    /// actually launch with.
    fn model_section(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        cfg: &serde_json::Map<String, serde_json::Value>,
    ) {
        ui.label(theme::section_heading("Model"));
        ui.add_space(8.0);
        let current = cfg
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        ui.horizontal(|ui| {
            theme::combo_field(ui, |ui| {
                egui::ComboBox::from_id_salt("agent_model")
                    .icon(theme::combo_caret)
                    .width(340.0)
                    .selected_text(
                        RichText::new(if current.is_empty() {
                            "(inherit launch default)".to_string()
                        } else {
                            current.clone()
                        })
                        .size(13.0),
                    )
                    .show_ui(ui, |ui| {
                        // Fetched lazily: this shells out to opencode.
                        if self.models.is_none() {
                            self.models = state.opencode_models(false);
                        }
                        let Some(list) = &self.models else {
                            ui.label(
                                RichText::new("opencode catalog unavailable")
                                    .size(12.0)
                                    .color(theme::DANGER),
                            );
                            return;
                        };
                        let mut picked: Option<String> = None;
                        if ui.selectable_label(current.is_empty(), "(inherit launch default)").clicked() {
                            picked = Some(String::new());
                        }
                        for group in &list.groups {
                            ui.label(RichText::new(&group.label).weak().size(11.0));
                            for m in &group.models {
                                if ui
                                    .selectable_label(current == m.id, RichText::new(&m.id).size(12.5))
                                    .on_hover_text(&m.name)
                                    .clicked()
                                {
                                    picked = Some(m.id.clone());
                                }
                            }
                        }
                        if let Some(id) = picked {
                            let value = if id.is_empty() {
                                serde_json::Value::Null // clears the field
                            } else {
                                id.clone().into()
                            };
                            match state.set_agent_field(
                                project,
                                slug,
                                self.entry.as_deref(),
                                "model",
                                value,
                            ) {
                                Ok(()) => {
                                    toast(toasts, ToastKind::Success, "model updated");
                                    state.refresh_agents(project);
                                }
                                Err(e) => toast(toasts, ToastKind::Error, e.to_string()),
                            }
                        }
                    });
            });
            if theme::house_button(ui, "Refresh").on_hover_text("re-pull opencode's catalog").clicked() {
                self.models = state.opencode_models(true);
            }
        });
    }

    /// Description + prompt. Buffered locally and written on focus loss so
    /// typing isn't a disk write per keystroke.
    fn text_section(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
    ) {
        let Some(buffers) = &mut self.buffers else { return };
        ui.label(theme::section_heading("Description"));
        ui.add_space(8.0);
        let desc = ui.add(
            egui::TextEdit::multiline(&mut buffers.description)
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
        let desc_value = buffers.description.clone();
        ui.add_space(20.0);
        ui.label(theme::section_heading("Prompt"));
        ui.add_space(8.0);
        let prompt = ui.add(
            egui::TextEdit::multiline(&mut buffers.prompt)
                .font(egui::TextStyle::Monospace)
                .desired_rows(14)
                .desired_width(f32::INFINITY),
        );
        let prompt_value = buffers.prompt.clone();

        let entry = self.entry.clone();
        let mut write = |field: &str, value: String| {
            match state.set_agent_field(project, slug, entry.as_deref(), field, value.into()) {
                Ok(()) => {
                    toast(toasts, ToastKind::Success, format!("{field} saved"));
                    state.refresh_agents(project);
                }
                Err(e) => toast(toasts, ToastKind::Error, e.to_string()),
            }
        };
        if desc.lost_focus() && desc.changed() {
            write("description", desc_value);
        }
        if prompt.lost_focus() && prompt.changed() {
            write("prompt", prompt_value);
        }
    }

    /// Subagents: add and remove, each editable with the same controls via
    /// the entry picker above.
    fn subagents_section(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        subagents: &[String],
    ) {
        ui.label(theme::section_heading("Subagents"));
        ui.add_space(8.0);
        if subagents.is_empty() {
            ui.label(
                RichText::new("none — a subagent is an entry the primary may delegate to")
                    .size(12.0)
                    .color(theme::TEXT_FAINT),
            );
        }
        for sub in subagents {
            ui.horizontal(|ui| {
                ui.label(RichText::new(sub).size(13.0).color(theme::TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::destructive_button(ui, "Remove")
                        .on_hover_text("also drops its delegation rule and role")
                        .clicked()
                    {
                        match state.remove_subagent(project, slug, sub) {
                            Ok(()) => {
                                toast(toasts, ToastKind::Success, format!("removed {sub}"));
                                self.entry = None;
                                self.buffers = None;
                                state.refresh_agents(project);
                            }
                            Err(e) => toast(toasts, ToastKind::Error, e.to_string()),
                        }
                    }
                    if theme::house_button(ui, "Edit").clicked() {
                        self.entry = Some(sub.clone());
                        self.buffers = None;
                    }
                });
            });
        }
        ui.add_space(10.0);
        match &mut self.new_subagent {
            None => {
                if theme::house_button(ui, format!("{}  Add subagent", ph::PLUS)).clicked() {
                    self.new_subagent = Some(NewSubagent {
                        // Default to the conventional `<primary>-scout`.
                        name: format!("{slug}-scout"),
                        ..Default::default()
                    });
                }
            }
            Some(form) => {
                let mut submit = false;
                let mut cancel = false;
                egui::Frame::default()
                    .fill(theme::PANEL)
                    .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE))
                    .corner_radius(egui::CornerRadius::same(2))
                    .inner_margin(egui::Margin::same(10))
                    .show(ui, |ui| {
                        ui.label(RichText::new("name (unique across the project)").size(11.5).color(theme::TEXT_MUTED));
                        ui.text_edit_singleline(&mut form.name);
                        ui.add_space(6.0);
                        ui.label(RichText::new("description").size(11.5).color(theme::TEXT_MUTED));
                        ui.text_edit_singleline(&mut form.description);
                        ui.add_space(6.0);
                        ui.label(RichText::new("prompt").size(11.5).color(theme::TEXT_MUTED));
                        ui.add(
                            egui::TextEdit::multiline(&mut form.prompt)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(5)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            submit = theme::house_button(ui, "Add").clicked();
                            cancel = theme::house_button(ui, "Cancel").clicked();
                        });
                    });
                if submit {
                    let form = self.new_subagent.take().unwrap_or_default();
                    match state.add_subagent(
                        project,
                        slug,
                        form.name.trim(),
                        form.description.trim(),
                        form.prompt.trim(),
                        None,
                        None,
                    ) {
                        Ok(()) => {
                            toast(toasts, ToastKind::Success, format!("added {}", form.name.trim()));
                            state.refresh_agents(project);
                        }
                        Err(e) => {
                            toast(toasts, ToastKind::Error, e.to_string());
                            // Keep the form open so the input isn't lost.
                            self.new_subagent = Some(form);
                        }
                    }
                } else if cancel {
                    self.new_subagent = None;
                }
            }
        }
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

/// One line on what a role means, for the picker's tooltip.
fn role_hint(role: corpus_core::AgentRole) -> &'static str {
    match role {
        corpus_core::AgentRole::Researcher => {
            "reads and curates: target_info + technique_save, plus the open internet. \
             No execution — enforced by the corpus server, not just by config."
        }
        corpus_core::AgentRole::Tester => {
            "acts in the regtest arena: sandbox, oracles, faucet, findings, attacks. \
             No open internet, so an execution turn cannot pull in untrusted text."
        }
        corpus_core::AgentRole::Super => "everything: research and penetration both.",
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