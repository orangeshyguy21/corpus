//! The shared model picker (app-flow chunk 8): search +
//! provider-grouped, the ONE widget behind every model-selection
//! surface (launch dialog, agent template composer, team instance
//! overrides, add-agent-to-team). The widget knows NOTHING about the
//! store: the parent hands it the grouped list (corpus-core
//! `model_list()`) plus the current value, and reads the value back
//! from the same string. With no list (opencode missing/empty) it
//! degrades to a free-text field with a warning — the explicit-model
//! rule is untouched: an empty value still refuses to launch.
//!
//! Interaction: click the button to open the popup; the search box is
//! focused immediately and filters case-insensitively across provider
//! + model (searching force-expands all groups); group headers
//!   collapse/expand; arrows move the highlight, enter selects, esc
//!   closes; registry-known models carry a ★ badge.

use std::collections::HashSet;

use egui::{RichText, Ui};

use corpus_core::ModelList;

/// The top entry on optional surfaces (empty = decide at launch).
const NO_MODEL: &str = "no model — decide at launch";

/// Persistent per-field picker state. One instance per call-site field
/// (per ROW for the team editor's agent rows).
#[derive(Default)]
pub struct ModelPicker {
    search: String,
    collapsed: HashSet<String>,
    highlight: usize,
    was_open: bool,
    focus_search: bool,
}

/// Everything the parent hands the picker besides the value buffer:
/// the grouped list, the badge set, and the field's behavior knobs.
pub struct ModelField<'a> {
    /// The grouped model list (corpus-core `model_list()`); None (or
    /// empty) degrades the field to free text with a warning.
    pub models: Option<&'a ModelList>,
    /// Registry-known ids get a ★ badge.
    pub badges: Option<&'a HashSet<String>>,
    /// Why the list is unavailable, shown in the degrade warning.
    pub degrade_note: Option<&'a str>,
    /// Optional surfaces (template composer, instance override) get a
    /// "no model" top entry; required surfaces (launch) do not.
    pub allow_none: bool,
}

impl ModelPicker {
    /// Render the model field. `value` is the current model ref
    /// ("" = none); the parent reads the selection back from it.
    pub fn field(&mut self, ui: &mut Ui, id_salt: &str, value: &mut String, field: ModelField<'_>) {
        let ModelField {
            models,
            badges,
            degrade_note,
            allow_none,
        } = field;
        match models {
            Some(list) if !list.groups.is_empty() => {
                self.popup_picker(ui, id_salt, list, value, allow_none, badges);
            }
            _ => {
                // Degrade (opencode missing/empty): the free-text field
                // this picker replaced, plus the warning.
                ui.text_edit_singleline(value);
                ui.colored_label(
                    egui::Color32::from_rgb(255, 180, 90),
                    format!(
                        "model list unavailable ({}) — free text; a launch still \
                         refuses without a valid provider/model",
                        degrade_note.unwrap_or("opencode missing or empty")
                    ),
                );
            }
        }
    }

    /// The button showing the current value + the popup below it.
    fn popup_picker(
        &mut self,
        ui: &mut Ui,
        id_salt: &str,
        list: &ModelList,
        value: &mut String,
        allow_none: bool,
        badges: Option<&HashSet<String>>,
    ) {
        let popup_id = ui.make_persistent_id(format!("model_picker::{id_salt}"));
        let label = if value.is_empty() {
            // Not .weak(): a dimmed button reads as DISABLED, which made
            // the empty state look broken.
            RichText::new(if allow_none { NO_MODEL } else { "— pick a model —" })
        } else {
            match list.display_name(value) {
                Some(name) => RichText::new(format!("{name} · {value}")),
                None => RichText::new(value.as_str()),
            }
        };
        let response = ui.add(egui::Button::new(label).min_size(egui::vec2(160.0, 0.0)));
        // egui 0.31's popup_below_widget does NOT open on click (see its
        // docs: "You must open the popup with Memory::open_popup or
        // Memory::toggle_popup") — without this the button is dead.
        if response.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }
        // Opening transition: fresh search, highlight reset, focus the box.
        let open = ui.memory(|m| m.is_popup_open(popup_id));
        if open && !self.was_open {
            self.search.clear();
            self.highlight = 0;
            self.focus_search = true;
        }
        self.was_open = open;
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &response,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| self.popup_contents(ui, list, value, allow_none, badges),
        );
    }

    /// The popup: search box, then provider groups with their models.
    fn popup_contents(
        &mut self,
        ui: &mut Ui,
        list: &ModelList,
        value: &mut String,
        allow_none: bool,
        badges: Option<&HashSet<String>>,
    ) {
        ui.set_min_width(360.0);
        // Read nav keys BEFORE the search edit can consume them.
        let (up, down, enter, esc) = nav_keys(ui);
        let search_id = ui.id().with("search");
        let search = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .id(search_id)
                .hint_text("search provider / model…")
                .desired_width(f32::INFINITY),
        );
        if self.focus_search {
            ui.memory_mut(|m| m.request_focus(search_id));
            self.focus_search = false;
        }
        if search.changed() {
            self.highlight = 0;
        }

        let needle = self.search.trim().to_lowercase();
        let searching = !needle.is_empty();
        // Groups with at least one match, each with its matched models.
        let mut rows: Vec<(&str, &str, Vec<&corpus_core::ModelOption>, bool)> = Vec::new();
        for group in &list.groups {
            let matched: Vec<&corpus_core::ModelOption> = group
                .models
                .iter()
                .filter(|m| model_matches(&group.id, &group.label, m, &needle))
                .collect();
            if matched.is_empty() {
                continue;
            }
            let expanded = searching || !self.collapsed.contains(&group.id);
            rows.push((&group.id, &group.label, matched, expanded));
        }
        // The flat nav order: [no model?] then every VISIBLE row.
        let mut flat: Vec<Option<String>> = Vec::new();
        if allow_none {
            flat.push(None);
        }
        for (_, _, matched, expanded) in &rows {
            if *expanded {
                flat.extend(matched.iter().map(|m| Some(m.id.clone())));
            }
        }
        self.highlight = self.highlight.min(flat.len().saturating_sub(1));
        if down {
            self.highlight = (self.highlight + 1).min(flat.len().saturating_sub(1));
        }
        if up {
            self.highlight = self.highlight.saturating_sub(1);
        }
        if esc {
            ui.memory_mut(|m| m.close_popup());
        }
        if enter {
            if let Some(pick) = flat.get(self.highlight) {
                *value = pick.clone().unwrap_or_default();
                ui.memory_mut(|m| m.close_popup());
            }
        }
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                let mut index = 0usize;
                if allow_none {
                    let response = ui.selectable_label(
                        value.is_empty() || index == self.highlight,
                        RichText::new(NO_MODEL).weak(),
                    );
                    if response.hovered() {
                        self.highlight = index;
                    }
                    if index == self.highlight && (up || down) {
                        response.scroll_to_me(None);
                    }
                    if response.clicked() {
                        value.clear();
                        ui.memory_mut(|m| m.close_popup());
                    }
                    index += 1;
                }
                if rows.is_empty() {
                    ui.weak("no models match");
                }
                for (id, label, matched, expanded) in &rows {
                    let arrow = if *expanded { "▼" } else { "▶" };
                    let header = ui.add(
                        egui::Label::new(
                            RichText::new(format!("{arrow} {label} ({})", matched.len())).strong(),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if header.clicked() && !searching && !self.collapsed.remove(*id) {
                        self.collapsed.insert((*id).to_string());
                    }
                    if !expanded {
                        continue;
                    }
                    for model in matched {
                        let highlighted = index == self.highlight;
                        let starred = badges.map(|b| b.contains(&model.id)).unwrap_or(false);
                        let star = if starred { "★ " } else { "" };
                        let text = format!("{star}{} · {}", model.name, model.model);
                        let response =
                            ui.selectable_label(highlighted || value == &model.id, text);
                        if starred {
                            response
                                .clone()
                                .on_hover_text("in the model registry (benchmarks/models.yaml)");
                        }
                        if response.hovered() {
                            self.highlight = index;
                        }
                        if highlighted && (up || down) {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            *value = model.id.clone();
                            ui.memory_mut(|m| m.close_popup());
                        }
                        index += 1;
                    }
                }
            });
    }
}

/// Does a model match the (lowercase) needle? Case-insensitive
/// substring across provider id + label and model id + name.
fn model_matches(
    provider_id: &str,
    provider_label: &str,
    model: &corpus_core::ModelOption,
    needle: &str,
) -> bool {
    needle.is_empty()
        || provider_id.to_lowercase().contains(needle)
        || provider_label.to_lowercase().contains(needle)
        || model.model.to_lowercase().contains(needle)
        || model.name.to_lowercase().contains(needle)
}

/// Arrow/enter/esc presses this frame, read before any widget in the
/// popup can consume the events. (up, down, enter, esc)
fn nav_keys(ui: &Ui) -> (bool, bool, bool, bool) {
    ui.input(|i| {
        let mut keys = (false, false, false, false);
        for event in &i.events {
            if let egui::Event::Key {
                key, pressed: true, ..
            } = event
            {
                match key {
                    egui::Key::ArrowUp => keys.0 = true,
                    egui::Key::ArrowDown => keys.1 = true,
                    egui::Key::Enter => keys.2 = true,
                    egui::Key::Escape => keys.3 = true,
                    _ => {}
                }
            }
        }
        keys
    })
}
