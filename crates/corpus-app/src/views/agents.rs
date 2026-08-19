//! Agent command editor for the selected agent. Its fixed header contains the
//! Forms/JSON mode, dominant New Mission action, Save, and overflow actions.
//! Forms use responsive cards plus an ASCII delegation map; JSON remains the
//! raw escape hatch with syntect highlighting. Both modes validate and save
//! through `AppState`; invalid documents never reach the store.
//!
//! No business logic here: corpus-core calls go through `AppState`.

use std::time::Duration;

use egui::{RichText, Ui};
use egui_phosphor::regular as ph;
use egui_toast::{Toast, ToastKind, ToastOptions, Toasts};

use crate::state::AppState;
use crate::theme;
use crate::views::{components, json_editor};

const AGENT_TWO_COLUMN_AT: f32 = 940.0;

/// Which editor the screen is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// Field-by-field forms: each control writes ONE field through a
    /// granular core call, so nothing else in the document is touched.
    Forms,
    /// The raw document, for anything the forms don't cover.
    Json,
}

/// Pure label rule for the fixed Save action. JSON edits keep the existing
/// unmarked label because that editor does not yet track a baseline; Forms
/// marks only a draft that actually differs from disk.
fn save_action_label(tab: Tab, forms_dirty: bool) -> &'static str {
    if tab == Tab::Forms && forms_dirty {
        "Save •"
    } else {
        "Save"
    }
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
    /// The in-progress edits for the current entry. NOTHING here reaches the
    /// store until Save — the Forms tab is Save-gated like the JSON tab.
    /// Keyed by the entry it belongs to; switching entries re-seeds from
    /// disk (unsaved edits are discarded, as in any Save-gated form).
    draft: Option<FormDraft>,
    /// The new-subagent form, when open.
    new_subagent: Option<NewSubagent>,
    /// opencode's model catalog, fetched on demand (a subprocess).
    models: crate::state::ModelDiscovery,
    /// Delete is never dispatched from the page action itself; the action
    /// opens this confirmation ritual first.
    confirm_delete: bool,
}

/// The Forms tab's editable state for ONE entry: the current (edited) values
/// alongside the on-disk baseline they were seeded from. Save writes only
/// the fields that differ from their baseline; `dirty` compares the two so
/// the button (and its unsaved marker) can reflect pending work.
struct FormDraft {
    entry_key: String,
    /// True when this draft edits the primary (name + sidecar role live on
    /// the primary only).
    is_primary: bool,
    /// The agent's display name (sidecar `name`). Primary only.
    name: String,
    role: corpus_core::AgentRole,
    /// Model id, or empty for "inherit launch default".
    model: String,
    description: String,
    prompt: String,
    // On-disk baseline captured at seed time.
    base_name: String,
    base_role: corpus_core::AgentRole,
    base_model: String,
    base_description: String,
    base_prompt: String,
}

impl FormDraft {
    /// Any field diverges from its on-disk baseline — there is work to Save.
    fn dirty(&self) -> bool {
        (self.is_primary && self.name.trim() != self.base_name)
            || self.role != self.base_role
            || self.model != self.base_model
            || self.description != self.base_description
            || self.prompt != self.base_prompt
    }
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
            draft: None,
            new_subagent: None,
            models: crate::state::ModelDiscovery::Loading,
            confirm_delete: false,
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
        let Some((_, agent)) = state.agents.iter().find(|(a, _)| a == &slug).cloned() else {
            return;
        };

        // (Re)load the editor buffer when the viewed agent changes or a Save
        // rewrote it — the buffer is always the on-disk (pretty) JSON.
        if self.viewed_agent.as_deref() != Some(slug.as_str()) {
            self.viewed_agent = Some(slug.clone());
            self.editor_text = serde_json::to_string_pretty(&agent.doc).unwrap_or_default();
            self.error = None;
            self.confirm_delete = false;
        }

        // The header shows the display NAME, never the opaque slug (the
        // slug is still in the sidebar hover and the JSON tab for identity).
        let name = crate::state::agent_label(&agent.meta.name, &slug);

        self.header(
            ui,
            state,
            toasts,
            &project,
            &slug,
            &name,
            agent.meta.created,
        );

        // The page action rail and mode switch stay fixed; only editor
        // content scrolls. This is the structural seam the retheme builds on.
        egui::ScrollArea::vertical()
            .id_salt("agent_body")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.body(ui, state, toasts, &project, &slug, &agent);
            });

        self.delete_confirm_window(ui, state, toasts, &project, &slug, &name);
    }

    /// Fixed command rail: the launch action leads, Save remains visible, and
    /// record-level secondary/destructive actions live in the overflow menu.
    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        name: &str,
        created: u64,
    ) {
        let current_tab = self.tab;
        let mut requested_tab = None;
        components::page_header_with_context(
            ui,
            "Agent",
            name,
            &format!("created: {}", fmt_epoch(created)),
            |ui| {
                for (tab, label) in [(Tab::Forms, "Forms"), (Tab::Json, "JSON")] {
                    let selected = current_tab == tab;
                    if theme::segment_button(ui, selected, label).clicked() && !selected {
                        requested_tab = Some(tab);
                    }
                }
            },
            |ui| {
                components::action_menu(ui, |ui| {
                    if ui.button("Clone…").clicked() {
                        self.clone_agent(state, toasts, project, slug);
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
                // Save lives here (not at the foot of the editor, where it
                // sat below the fold). Both tabs are Save-gated: Forms holds
                // its edits in a draft and JSON in the text buffer, and
                // neither reaches the store until this click. A trailing dot
                // marks a Forms draft with unsaved changes.
                let label = save_action_label(
                    self.tab,
                    self.draft.as_ref().is_some_and(|draft| draft.dirty()),
                );
                if theme::house_button(ui, label).clicked() {
                    self.save(state, toasts, project, slug);
                }
                if theme::primary_button(ui, format!("{}  New Mission", ph::PLUS)).clicked() {
                    self.new_mission(state, toasts, project, slug);
                }
            },
        );
        if let Some(tab) = requested_tab {
            self.select_tab(tab);
        }
        ui.add_space(8.0);
    }

    /// Changing editor modes intentionally discards the other mode's unsaved
    /// buffer, preserving the existing contract.
    fn select_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.error = None;
        self.viewed_agent = None;
        self.draft = None;
    }

    /// Scrollable editor body shared by the Forms and JSON tabs.
    fn body(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        agent: &corpus_core::AgentConfig,
    ) {
        if self.tab == Tab::Forms {
            self.forms(ui, state, toasts, project, slug, agent);
            return;
        }

        // --- JSON editor (spec §6): monospace 13.5px, fills the width,
        // min height 480, in a Frame (EDITOR_BG fill, 1px HAIRLINE, radius 2).
        components::panel_card(ui, "Raw configuration", "原始配置", |ui| {
            egui::Frame::default()
                .fill(theme::EDITOR_BG)
                .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE))
                .corner_radius(egui::CornerRadius::same(2))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_min_height(480.0);
                    let mut layouter = json_editor::layouter;
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

        // --- inline validation banner (under the editor, signal red 12px).
        // Save itself is the top-bar button; an invalid document sets this
        // and never writes.
        if let Some(error) = &self.error {
            ui.add_space(6.0);
            ui.label(
                RichText::new(error.clone())
                    .size(12.0)
                    .color(theme::SIGNAL_RED),
            );
        }
    }

    /// The Forms tab. Controls mutate a per-entry `FormDraft`; nothing
    /// reaches the store until the top-bar Save commits the draft. Save
    /// writes only the fields that changed, so it can never clobber a field
    /// the form doesn't display.
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
            ui.label(
                RichText::new("this agent has no `agent` map — use the JSON tab")
                    .color(theme::SIGNAL_RED),
            );
            return;
        };
        // A rejected Save (core validation) surfaces here, above the fields.
        if let Some(error) = &self.error {
            ui.label(
                RichText::new(error.clone())
                    .size(12.0)
                    .color(theme::SIGNAL_RED),
            );
            ui.add_space(8.0);
        }
        // Entry picker: the primary plus each subagent. Subagents are
        // edited with the SAME controls as the primary.
        let mut subagents: Vec<String> = entries
            .iter()
            .filter(|(name, cfg)| {
                **name != slug && cfg.get("mode").and_then(|m| m.as_str()) == Some("subagent")
            })
            .map(|(name, _)| name.clone())
            .collect();
        subagents.sort();
        // A stale selection (subagent just removed) falls back to primary.
        if self.entry.as_ref().is_some_and(|e| !subagents.contains(e)) {
            self.entry = None;
            self.draft = None;
        }
        if !subagents.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(field_label("Editing"));
                if theme::segment_button(ui, self.entry.is_none(), slug).clicked() {
                    self.entry = None;
                    self.draft = None;
                }
                for sub in &subagents {
                    let selected = self.entry.as_deref() == Some(sub.as_str());
                    if theme::segment_button(ui, selected, sub).clicked() {
                        self.entry = Some(sub.clone());
                        self.draft = None;
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
        // Re-seed the draft (current == baseline) when the target changes.
        // The RAW stored role is the baseline: a subagent's is capped by the
        // primary only at render, so seeding the capped value would make an
        // unchanged super-subagent read as dirty.
        let stale = self.draft.as_ref().is_none_or(|d| d.entry_key != entry_key);
        if stale {
            let role = if is_primary {
                agent.meta.role()
            } else {
                agent
                    .meta
                    .subagent_roles
                    .get(&entry_key)
                    .copied()
                    .unwrap_or_else(|| agent.meta.role())
            };
            let model = cfg
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let description = cfg
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let prompt = cfg
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // The sidecar name is the primary's; a subagent has none.
            let name = agent.meta.name.clone();
            self.draft = Some(FormDraft {
                entry_key: entry_key.clone(),
                is_primary,
                name: name.clone(),
                role,
                model: model.clone(),
                description: description.clone(),
                prompt: prompt.clone(),
                base_name: name,
                base_role: role,
                base_model: model,
                base_description: description,
                base_prompt: prompt,
            });
        }

        if agent_form_columns(ui.available_width()) == 2 {
            ui.columns(2, |columns| {
                let (left, right) = columns.split_at_mut(1);
                self.identity_card(&mut left[0], state, is_primary);
                left[0].add_space(12.0);
                self.role_card(
                    &mut left[0],
                    state,
                    toasts,
                    project,
                    slug,
                    agent,
                    cfg,
                    is_primary,
                );
                left[0].add_space(12.0);
                self.subagents_card(
                    &mut left[0],
                    state,
                    toasts,
                    project,
                    slug,
                    agent,
                    &subagents,
                );

                self.description_card(&mut right[0]);
                right[0].add_space(12.0);
                self.prompt_card(&mut right[0]);
            });
        } else {
            self.identity_card(ui, state, is_primary);
            ui.add_space(12.0);
            self.role_card(ui, state, toasts, project, slug, agent, cfg, is_primary);
            ui.add_space(12.0);
            self.subagents_card(ui, state, toasts, project, slug, agent, &subagents);
            ui.add_space(12.0);
            self.description_card(ui);
            ui.add_space(12.0);
            self.prompt_card(ui);
        }
    }

    fn identity_card(&mut self, ui: &mut Ui, state: &mut AppState, is_primary: bool) {
        components::panel_card(ui, "Identity", "身份", |ui| {
            if is_primary {
                self.name_section(ui);
                ui.add_space(14.0);
            }
            self.model_section(ui, state);
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn role_card(
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
        components::panel_card(ui, "Role & access", "角色与权限", |ui| {
            self.role_section(ui, state, toasts, project, slug, agent, cfg, is_primary);
        });
    }

    fn description_card(&mut self, ui: &mut Ui) {
        components::panel_card(ui, "Description", "描述", |ui| self.description_section(ui));
    }

    fn prompt_card(&mut self, ui: &mut Ui) {
        components::panel_card(ui, "Prompt", "提示词", |ui| self.prompt_section(ui));
    }

    #[allow(clippy::too_many_arguments)]
    fn subagents_card(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        agent: &corpus_core::AgentConfig,
        subagents: &[String],
    ) {
        components::panel_card(ui, "Subagents", "子代理", |ui| {
            self.subagents_section(ui, state, toasts, project, slug, agent, subagents);
        });
    }

    /// Name: the agent's display label (sidecar `name`). Edits the draft
    /// only — committed on Save. The slug — its identity in every path — is
    /// never touched. Primary only; a subagent has no name of its own.
    fn name_section(&mut self, ui: &mut Ui) {
        let Some(draft) = &mut self.draft else { return };
        ui.label(field_label("Display name"));
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut draft.name)
                .hint_text("new agent")
                .desired_width(f32::INFINITY),
        );
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
        // The draft holds the pending role; edits stay local until Save.
        let Some((current, entry_key)) = self
            .draft
            .as_ref()
            .map(|draft| (draft.role, draft.entry_key.clone()))
        else {
            return;
        };
        ui.label(field_label(if is_primary {
            "Primary role ceiling"
        } else {
            "Requested subagent role"
        }));
        ui.add_space(6.0);
        if let Some(role) = role_picker(ui, &entry_key, current) {
            if let Some(draft) = &mut self.draft {
                draft.role = role;
            }
        }
        let current = self.draft.as_ref().map(|d| d.role).unwrap_or(current);
        let effective =
            crate::views::policy::effective_role(current, agent.meta.role(), is_primary);
        ui.add_space(12.0);
        policy_preview(ui, effective);

        // Divergence: stored corpus_* allows the role will overrule.
        let diverging: Vec<&str> = corpus_core::CORPUS_TOOLS
            .into_iter()
            .filter(|tool| {
                let stored = cfg
                    .get("permission")
                    .and_then(|p| p.get(*tool))
                    .and_then(|v| v.as_str());
                stored == Some("allow") && !effective.allows(tool)
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
                            "Stored permissions conflict with {}",
                            effective.as_str(),
                        ))
                        .size(11.5)
                        .color(theme::WARN),
                    );
                    if theme::house_button(ui, "Repair").clicked() {
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
    /// actually launch with. Edits the draft only — committed on Save.
    fn model_section(&mut self, ui: &mut Ui, state: &mut AppState) {
        let Some(current) = self.draft.as_ref().map(|d| d.model.clone()) else {
            return;
        };
        ui.label(field_label("Model"));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            theme::combo_field(ui, |ui| {
                egui::ComboBox::from_id_salt("agent_model")
                    .icon(theme::combo_caret)
                    .width((ui.available_width() - 92.0).max(220.0))
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
                        if matches!(self.models, crate::state::ModelDiscovery::Loading) {
                            self.models = state.opencode_models(false);
                        }
                        let list = match &self.models {
                            crate::state::ModelDiscovery::Ready(list) => list,
                            crate::state::ModelDiscovery::Loading => {
                                ui.label(RichText::new("loading opencode catalog…").size(12.0));
                                return;
                            }
                            crate::state::ModelDiscovery::Failed(error) => {
                                ui.label(
                                    RichText::new("opencode catalog unavailable")
                                        .size(12.0)
                                        .color(theme::SIGNAL_RED),
                                )
                                .on_hover_text(error);
                                return;
                            }
                        };
                        let mut picked: Option<String> = None;
                        if ui
                            .selectable_label(current.is_empty(), "(inherit launch default)")
                            .clicked()
                        {
                            picked = Some(String::new());
                        }
                        for group in &list.groups {
                            ui.label(RichText::new(&group.label).weak().size(11.0));
                            for m in &group.models {
                                if ui
                                    .selectable_label(
                                        current == m.id,
                                        RichText::new(&m.id).size(12.5),
                                    )
                                    .on_hover_text(&m.name)
                                    .clicked()
                                {
                                    picked = Some(m.id.clone());
                                }
                            }
                        }
                        if let (Some(id), Some(draft)) = (picked, &mut self.draft) {
                            draft.model = id;
                        }
                    });
            });
            if theme::house_button(ui, "Refresh")
                .on_hover_text("re-pull opencode's catalog")
                .clicked()
            {
                self.models = state.opencode_models(true);
            }
        });
    }

    /// Description edits remain draft-only until the fixed Save action.
    fn description_section(&mut self, ui: &mut Ui) {
        let Some(draft) = &mut self.draft else { return };
        ui.add(
            egui::TextEdit::multiline(&mut draft.description)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }

    /// The prompt gets the largest editor surface and keeps monospace type.
    fn prompt_section(&mut self, ui: &mut Ui) {
        let Some(draft) = &mut self.draft else { return };
        ui.add(
            egui::TextEdit::multiline(&mut draft.prompt)
                .font(egui::TextStyle::Monospace)
                .desired_rows(22)
                .desired_width(f32::INFINITY),
        );
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
        agent: &corpus_core::AgentConfig,
        subagents: &[String],
    ) {
        if let Some(selected) =
            agent_structure_diagram(ui, slug, agent, subagents, self.entry.as_deref())
        {
            self.entry = match selected {
                AgentNodeSelection::Primary => None,
                AgentNodeSelection::Subagent(name) => Some(name),
            };
            self.draft = None;
        }
        ui.add_space(8.0);
        if let Some(sub) = self.entry.clone().filter(|entry| subagents.contains(entry)) {
            ui.horizontal(|ui| {
                ui.label(field_label("Selected"));
                ui.label(
                    RichText::new(crate::state::agent_label(&sub, &sub))
                        .size(12.0)
                        .monospace()
                        .color(theme::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::destructive_button(ui, "Remove subagent")
                        .on_hover_text("also drops its delegation rule and role")
                        .clicked()
                    {
                        match state.remove_subagent(project, slug, &sub) {
                            Ok(()) => {
                                toast(toasts, ToastKind::Success, format!("removed {sub}"));
                                self.entry = None;
                                self.draft = None;
                                state.refresh_agents(project);
                            }
                            Err(e) => toast(toasts, ToastKind::Error, e.to_string()),
                        }
                    }
                });
            });
        }
        ui.add_space(8.0);
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
                        ui.label(
                            RichText::new("name (unique across the project)")
                                .size(11.5)
                                .color(theme::TEXT_MUTED),
                        );
                        ui.text_edit_singleline(&mut form.name);
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("description")
                                .size(11.5)
                                .color(theme::TEXT_MUTED),
                        );
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
                            toast(
                                toasts,
                                ToastKind::Success,
                                format!("added {}", form.name.trim()),
                            );
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

    /// The top-bar Save. Dispatches to the tab's own commit: the Forms draft
    /// or the raw JSON document.
    fn save(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
        match self.tab {
            Tab::Forms => self.save_forms(state, toasts, project, slug),
            Tab::Json => self.save_json(state, toasts, project, slug),
        }
    }

    /// Commit the Forms draft: write only the fields that diverge from their
    /// on-disk baseline, so an unchanged field is never rewritten and a field
    /// the form doesn't model is never touched. Each write is validated
    /// core-side; a rejected one leaves an error and keeps the draft so the
    /// operator can fix and re-Save.
    fn save_forms(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
        let Some(mut draft) = self.draft.take() else {
            return;
        };
        let entry = if draft.is_primary {
            None
        } else {
            Some(draft.entry_key.clone())
        };
        let mut errors: Vec<String> = Vec::new();

        // Name (sidecar, primary only).
        if draft.is_primary && draft.name.trim() != draft.base_name {
            match state.set_agent_name(project, slug, draft.name.trim()) {
                Ok(()) => draft.base_name = draft.name.trim().to_string(),
                Err(e) => errors.push(e.to_string()),
            }
        }
        // Role (sidecar).
        if draft.role != draft.base_role {
            match state.set_agent_role(project, slug, entry.as_deref(), draft.role) {
                Ok(()) => draft.base_role = draft.role,
                Err(e) => errors.push(e.to_string()),
            }
        }
        // Model (`null` clears the field → inherit launch default).
        if draft.model != draft.base_model {
            let value = if draft.model.is_empty() {
                serde_json::Value::Null
            } else {
                draft.model.clone().into()
            };
            match state.set_agent_field(project, slug, entry.as_deref(), "model", value) {
                Ok(()) => draft.base_model = draft.model.clone(),
                Err(e) => errors.push(e.to_string()),
            }
        }
        // Description.
        if draft.description != draft.base_description {
            let value = draft.description.clone().into();
            match state.set_agent_field(project, slug, entry.as_deref(), "description", value) {
                Ok(()) => draft.base_description = draft.description.clone(),
                Err(e) => errors.push(e.to_string()),
            }
        }
        // Prompt.
        if draft.prompt != draft.base_prompt {
            let value = draft.prompt.clone().into();
            match state.set_agent_field(project, slug, entry.as_deref(), "prompt", value) {
                Ok(()) => draft.base_prompt = draft.prompt.clone(),
                Err(e) => errors.push(e.to_string()),
            }
        }

        state.refresh_agents(project);
        // Keep the draft (with baselines advanced for whatever landed) so a
        // failed field stays pending and editable.
        self.draft = Some(draft);
        if errors.is_empty() {
            self.error = None;
            toast(toasts, ToastKind::Success, "agent saved");
        } else {
            let msg = errors.join("; ");
            self.error = Some(msg.clone());
            toast(toasts, ToastKind::Error, msg);
        }
    }

    /// Save the raw JSON document via the core validator: it must parse and
    /// satisfy the structural rules (agent map, one primary, valid
    /// permissions, resolvable `{file:}` refs). Invalid → red banner, no write.
    fn save_json(&mut self, state: &mut AppState, toasts: &mut Toasts, project: &str, slug: &str) {
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
                toast(toasts, ToastKind::Success, "agent saved");
            }
            Err(error) => {
                self.error = Some(error.to_string());
            }
        }
    }

    fn new_mission(
        &mut self,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
    ) {
        match state.create_mission(project, slug, "") {
            Ok(mission) => {
                state.refresh_missions(project);
                crate::views::mission_actions::launch(state, toasts, project, &mission);
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    fn clone_agent(
        &mut self,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
    ) {
        match state.clone_agent(project, slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, "agent cloned");
                state.refresh_agents(project);
            }
            Err(error) => toast(toasts, ToastKind::Error, error.to_string()),
        }
    }

    /// Confirm deletion before dispatch. The old page deleted immediately
    /// from its top-row button; the overflow-menu retheme needs a reusable
    /// ritual whose first click is always non-mutating.
    fn delete_confirm_window(
        &mut self,
        ui: &mut Ui,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
        name: &str,
    ) {
        if !self.confirm_delete {
            return;
        }
        let mut open = self.confirm_delete;
        let mut deleted = false;
        let mut cancel = false;
        egui::Window::new("Delete agent")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
            .show(ui.ctx(), |ui| {
                ui.label(format!("Delete agent “{name}”?"));
                ui.weak("Its configuration is removed from this project. There is no undo.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if theme::destructive_button(ui, "Delete agent").clicked() {
                        deleted = self.delete_agent(state, toasts, project, slug);
                    }
                    if theme::house_button(ui, "Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        self.confirm_delete = open && !deleted && !cancel;
    }

    fn delete_agent(
        &mut self,
        state: &mut AppState,
        toasts: &mut Toasts,
        project: &str,
        slug: &str,
    ) -> bool {
        match state.delete_agent(project, slug) {
            Ok(()) => {
                toast(toasts, ToastKind::Success, "agent deleted");
                state.refresh_agents(project);
                // The view re-defaults to the first remaining agent.
                state.selected_agent = None;
                true
            }
            Err(error) => {
                toast(toasts, ToastKind::Error, error.to_string());
                false
            }
        }
    }
}

enum AgentNodeSelection {
    Primary,
    Subagent(String),
}

fn role_picker(
    ui: &mut Ui,
    entry_key: &str,
    current: corpus_core::AgentRole,
) -> Option<corpus_core::AgentRole> {
    let summary = crate::views::policy::short_description(current);
    let mut picked = None;
    theme::combo_field(ui, |ui| {
        egui::ComboBox::from_id_salt(("agent_role", entry_key))
            .icon(theme::combo_caret)
            .width(ui.available_width())
            .selected_text(
                RichText::new(format!("{}  —  {summary}", current.as_str()))
                    .size(12.5)
                    .color(theme::TEXT),
            )
            .show_ui(ui, |ui| {
                for role in corpus_core::AgentRole::ALL {
                    let text = format!(
                        "{:<10}  {}",
                        role.as_str().to_uppercase(),
                        crate::views::policy::short_description(role)
                    );
                    let response = ui
                        .selectable_label(
                            role == current,
                            RichText::new(text)
                                .size(11.0)
                                .monospace()
                                .color(theme::TEXT),
                        )
                        .on_hover_text(role.hint());
                    if response.clicked() {
                        picked = Some(role);
                        ui.close_menu();
                    }
                }
            });
    });
    picked
}

fn policy_preview(ui: &mut Ui, effective: corpus_core::AgentRole) {
    use crate::views::policy::{Capability, RolePolicy};

    components::soft_rule(ui);
    ui.add_space(8.0);
    let policy = RolePolicy::new(effective);
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("GRANTS")
                .size(9.5)
                .monospace()
                .color(theme::HEALTHY),
        );
        for capability in Capability::ALL
            .into_iter()
            .filter(|capability| policy.allows(*capability))
        {
            capability_chip(ui, capability, true);
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("DENIALS")
                .size(9.5)
                .monospace()
                .color(theme::SIGNAL_RED),
        );
        for capability in Capability::ALL
            .into_iter()
            .filter(|capability| !policy.allows(*capability))
        {
            capability_chip(ui, capability, false);
        }
    });
}

fn capability_chip(ui: &mut Ui, capability: crate::views::policy::Capability, allowed: bool) {
    let color = if allowed {
        theme::HEALTHY
    } else {
        theme::SIGNAL_RED
    };
    let icon = if allowed {
        ph::CHECK_CIRCLE
    } else {
        ph::X_CIRCLE
    };
    ui.label(theme::icon_label(
        icon,
        10.0,
        color,
        capability.label(),
        theme::font(9.5),
        theme::TEXT_MUTED,
    ))
    .on_hover_text(capability.detail());
}

/// Direct delegation topology rendered as a scalable terminal diagram. The
/// store models one primary entry with direct subagent entries, so the tree
/// deliberately does not imply deeper nesting that the config cannot express.
fn agent_structure_diagram(
    ui: &mut Ui,
    slug: &str,
    agent: &corpus_core::AgentConfig,
    subagents: &[String],
    selected: Option<&str>,
) -> Option<AgentNodeSelection> {
    components::ascii_banner(ui, "Agent structure");
    ui.add_space(2.0);
    let primary_response = ascii_agent_row(
        ui,
        ("agent-structure-primary", slug),
        "|  ",
        &crate::state::agent_label(&agent.meta.name, slug),
        agent.meta.role().as_str(),
        "PRIMARY",
        selected.is_none(),
    );
    let mut picked = primary_response
        .clicked()
        .then_some(AgentNodeSelection::Primary);

    if subagents.is_empty() {
        ui.label(
            RichText::new("|  \\-- no subagents")
                .font(theme::mono(10.5))
                .color(theme::TEXT_FAINT),
        );
    }
    for (index, name) in subagents.iter().enumerate() {
        let role = subagent_effective_role(
            agent.meta.role(),
            agent.meta.subagent_roles.get(name).copied(),
        );
        let branch = if index + 1 == subagents.len() {
            "|  \\--"
        } else {
            "|  +--"
        };
        let response = ascii_agent_row(
            ui,
            ("agent-structure-subagent", slug, name),
            branch,
            &crate::state::agent_label(name, name),
            role.as_str(),
            "SUBAGENT",
            selected == Some(name.as_str()),
        );
        if response.clicked() {
            picked = Some(AgentNodeSelection::Subagent(name.clone()));
        }
    }
    ui.add_space(2.0);
    components::ascii_rule(ui);
    picked
}

fn ascii_agent_row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    branch: &str,
    name: &str,
    role: &str,
    kind: &str,
    selected: bool,
) -> egui::Response {
    use egui::text::{LayoutJob, TextFormat};

    let font = theme::mono(10.5);
    let format = |color| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.append(branch, 0.0, format(theme::TEXT_FAINT));
    job.append(&format!("[{}] ", kind), 0.0, format(theme::INTERACTION));
    job.append(name, 0.0, format(theme::TEXT));
    job.append("  <", 0.0, format(theme::TEXT_FAINT));
    job.append(&role.to_uppercase(), 0.0, format(theme::INTERACTION));
    job.append(">", 0.0, format(theme::TEXT_FAINT));
    components::ascii_row(ui, id, job, selected)
}

fn field_label(text: &str) -> RichText {
    RichText::new(text.to_uppercase())
        .size(10.5)
        .monospace()
        .color(theme::TEXT_FAINT)
}

fn agent_form_columns(available_width: f32) -> usize {
    if available_width >= AGENT_TWO_COLUMN_AT {
        2
    } else {
        1
    }
}

fn subagent_effective_role(
    primary: corpus_core::AgentRole,
    assigned: Option<corpus_core::AgentRole>,
) -> corpus_core::AgentRole {
    crate::views::policy::effective_role(assigned.unwrap_or(primary), primary, false)
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_action_marks_only_a_dirty_forms_draft() {
        assert_eq!(save_action_label(Tab::Forms, false), "Save");
        assert_eq!(save_action_label(Tab::Forms, true), "Save •");
        assert_eq!(save_action_label(Tab::Json, false), "Save");
        assert_eq!(save_action_label(Tab::Json, true), "Save");
    }

    #[test]
    fn agent_cards_stack_when_the_chat_panel_reduces_the_canvas() {
        assert_eq!(agent_form_columns(AGENT_TWO_COLUMN_AT - 1.0), 1);
        assert_eq!(agent_form_columns(AGENT_TWO_COLUMN_AT), 2);
        assert_eq!(agent_form_columns(1_440.0), 2);
    }

    #[test]
    fn structure_diagram_labels_the_effective_capped_role() {
        use corpus_core::AgentRole;

        assert_eq!(
            subagent_effective_role(AgentRole::Tester, Some(AgentRole::Super)),
            AgentRole::Tester
        );
        assert_eq!(
            subagent_effective_role(AgentRole::Super, Some(AgentRole::Researcher)),
            AgentRole::Researcher
        );
        assert_eq!(
            subagent_effective_role(AgentRole::Curator, None),
            AgentRole::Curator
        );
    }
}
