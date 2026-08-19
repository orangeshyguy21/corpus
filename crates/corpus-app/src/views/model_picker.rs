//! Shared model picker for agent configuration and the in-house GDK chat.
//!
//! Both surfaces render the same large, searchable popup, but keep their own
//! catalog source and storage format. The caller hands this widget a grouped
//! [`corpus_core::ModelList`] and receives the selected [`ModelOption`] back.

use std::collections::HashSet;

use egui::{Color32, RichText, Ui};

use corpus_core::{ModelList, ModelOption};

use crate::theme;

const MIN_POPUP_WIDTH: f32 = 640.0;
const MAX_POPUP_HEIGHT: f32 = 520.0;

/// Persistent interaction state. Each picker surface owns one instance.
#[derive(Default)]
pub struct ModelPicker {
    search: String,
    collapsed: HashSet<String>,
    highlight: usize,
    focus_search: bool,
}

/// The catalog state rendered inside the popup.
pub enum Catalog<'a> {
    Ready(&'a ModelList),
    Loading(&'a str),
    Failed { label: &'a str, detail: &'a str },
}

/// Per-surface presentation knobs. Catalog discovery stays with the caller.
pub struct Options<'a> {
    pub id_salt: &'a str,
    /// Full catalog id used to highlight the current option.
    pub current_id: &'a str,
    /// Text shown in the closed field (it may differ from `current_id`).
    pub selected_label: &'a str,
    pub empty_label: &'a str,
    /// When present, an empty-value option is pinned above the groups.
    pub none_label: Option<&'a str>,
    pub field_width: f32,
    pub font_size: f32,
    pub text_color: Color32,
    pub status_dot: Option<Color32>,
    pub refresh_label: &'a str,
}

/// A selection is distinct from no interaction; `None` clears the value.
pub enum Selection {
    None,
    Model(ModelOption),
}

pub struct Output {
    pub response: egui::Response,
    pub selection: Option<Selection>,
    pub refresh_requested: bool,
}

impl ModelPicker {
    pub fn show(&mut self, ui: &mut Ui, catalog: Catalog<'_>, options: Options<'_>) -> Output {
        let Options {
            id_salt,
            current_id,
            selected_label,
            empty_label,
            none_label,
            field_width,
            font_size,
            text_color,
            status_dot,
            refresh_label,
        } = options;

        let field_width = field_width.max(90.0);
        let label = if selected_label.is_empty() {
            empty_label
        } else {
            selected_label
        };
        let label_color = if selected_label.is_empty() {
            theme::TEXT_FAINT
        } else {
            text_color
        };
        let selected = fit_field_label(
            ui,
            label,
            label_color,
            field_width - 38.0,
            status_dot.is_some(),
            font_size,
        );

        let field_id = ui.make_persistent_id(("model_picker", id_salt));
        let popup_id = field_id.with("popup");
        let was_open = ui.memory(|memory| memory.is_popup_open(popup_id));
        let response = theme::combo_field(ui, |ui| {
            ui.add_sized(
                egui::vec2(field_width, 28.0),
                egui::Button::new(selected).truncate(),
            )
        });
        if response.clicked() {
            if !was_open {
                self.search.clear();
                self.highlight = 0;
                self.focus_search = true;
            }
            ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        }

        let rect = response.rect;
        ui.painter().text(
            egui::pos2(rect.right() - 14.0, rect.center().y),
            egui::Align2::CENTER_CENTER,
            egui_phosphor::regular::CARET_DOWN,
            egui::FontId::new(13.0, egui::FontFamily::Name("phosphor".into())),
            theme::TEXT_MUTED,
        );
        if let Some(dot) = status_dot {
            ui.painter()
                .circle_filled(egui::pos2(rect.left() + 15.0, rect.center().y), 3.5, dot);
        }

        let screen = ui.ctx().screen_rect();
        let popup_width = field_width
            .max(MIN_POPUP_WIDTH)
            .min((screen.width() - 32.0).max(280.0));
        let popup_height = MAX_POPUP_HEIGHT.min((screen.height() - 120.0).max(240.0));
        let mut selection = None;
        let mut refresh_requested = false;
        egui::popup::popup_above_or_below_widget(
            ui,
            popup_id,
            &response,
            egui::AboveOrBelow::Below,
            egui::popup::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(popup_width);
                ui.set_max_width(popup_width);

                let (up, down, enter, escape) = nav_keys(ui);
                let search_id = popup_id.with("search");
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .id(search_id)
                        .hint_text("search provider or model…")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_search {
                    ui.memory_mut(|memory| memory.request_focus(search_id));
                    self.focus_search = false;
                }
                if search.changed() {
                    self.highlight = 0;
                }
                if escape {
                    ui.memory_mut(|memory| memory.close_popup());
                }
                ui.separator();

                match catalog {
                    Catalog::Ready(list) => {
                        selection = self.model_rows(
                            ui,
                            list,
                            current_id,
                            none_label,
                            popup_height - 92.0,
                            up,
                            down,
                            enter,
                        );
                    }
                    Catalog::Loading(message) => {
                        ui.add_space(12.0);
                        ui.label(RichText::new(message).size(12.0).color(theme::TEXT_MUTED));
                        ui.add_space(12.0);
                    }
                    Catalog::Failed { label, detail } => {
                        ui.add_space(12.0);
                        ui.label(RichText::new(label).size(12.0).color(theme::SIGNAL_RED))
                            .on_hover_text(detail);
                        ui.add_space(12.0);
                    }
                }

                ui.separator();
                if theme::house_button(ui, refresh_label).clicked() {
                    refresh_requested = true;
                    ui.memory_mut(|memory| memory.close_popup());
                }
            },
        );

        Output {
            response,
            selection,
            refresh_requested,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn model_rows(
        &mut self,
        ui: &mut Ui,
        list: &ModelList,
        current_id: &str,
        none_label: Option<&str>,
        max_height: f32,
        up: bool,
        down: bool,
        enter: bool,
    ) -> Option<Selection> {
        let needle = self.search.trim().to_lowercase();
        let searching = !needle.is_empty();
        let groups = matching_groups(list, &needle);

        let mut flat: Vec<Option<&ModelOption>> = Vec::new();
        if none_label.is_some() {
            flat.push(None);
        }
        for (group, models) in &groups {
            if searching || !self.collapsed.contains(&group.id) {
                flat.extend(models.iter().copied().map(Some));
            }
        }
        self.highlight = self.highlight.min(flat.len().saturating_sub(1));
        if down && !flat.is_empty() {
            self.highlight = (self.highlight + 1).min(flat.len() - 1);
        }
        if up {
            self.highlight = self.highlight.saturating_sub(1);
        }
        if enter {
            if let Some(option) = flat.get(self.highlight) {
                ui.memory_mut(|memory| memory.close_popup());
                return Some(match option {
                    Some(model) => Selection::Model((*model).clone()),
                    None => Selection::None,
                });
            }
        }

        let mut selection = None;
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .auto_shrink([false, false])
            .id_salt(ui.id().with("model_results"))
            .show(ui, |ui| {
                let mut index = 0usize;
                if let Some(label) = none_label {
                    let response = ui.selectable_label(
                        current_id.is_empty() || self.highlight == index,
                        RichText::new(label).size(12.5),
                    );
                    if response.hovered() {
                        self.highlight = index;
                    }
                    if self.highlight == index && (up || down) {
                        response.scroll_to_me(None);
                    }
                    if response.clicked() {
                        selection = Some(Selection::None);
                        ui.memory_mut(|memory| memory.close_popup());
                    }
                    index += 1;
                }

                if groups.is_empty() {
                    ui.add_space(10.0);
                    ui.label(RichText::new("no models match").color(theme::TEXT_MUTED));
                    ui.add_space(10.0);
                }
                for (group, models) in groups {
                    let expanded = searching || !self.collapsed.contains(&group.id);
                    let caret = if expanded {
                        egui_phosphor::regular::CARET_DOWN
                    } else {
                        egui_phosphor::regular::CARET_RIGHT
                    };
                    let header = ui.add(
                        egui::Label::new(
                            RichText::new(format!("{caret}  {}  ({})", group.label, models.len()))
                                .size(11.0)
                                .color(theme::TEXT_MUTED),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if header.clicked() && !searching && !self.collapsed.remove(&group.id) {
                        self.collapsed.insert(group.id.clone());
                    }
                    if !expanded {
                        continue;
                    }
                    for model in models {
                        let highlighted = self.highlight == index;
                        let label = if model.name == model.model {
                            model.id.clone()
                        } else {
                            format!("{}  ·  {}", model.name, model.id)
                        };
                        let response = ui
                            .selectable_label(
                                current_id == model.id || highlighted,
                                RichText::new(label).size(12.5),
                            )
                            .on_hover_text(format!("{}\n{}", model.name, model.id));
                        if response.hovered() {
                            self.highlight = index;
                        }
                        if highlighted && (up || down) {
                            response.scroll_to_me(None);
                        }
                        if response.clicked() {
                            selection = Some(Selection::Model(model.clone()));
                            ui.memory_mut(|memory| memory.close_popup());
                        }
                        index += 1;
                    }
                }
            });
        selection
    }
}

fn matching_groups<'a>(
    list: &'a ModelList,
    needle: &str,
) -> Vec<(&'a corpus_core::ModelProviderGroup, Vec<&'a ModelOption>)> {
    list.groups
        .iter()
        .filter_map(|group| {
            let models: Vec<_> = group
                .models
                .iter()
                .filter(|model| model_matches(&group.id, &group.label, model, needle))
                .collect();
            (!models.is_empty()).then_some((group, models))
        })
        .collect()
}

fn model_matches(
    provider_id: &str,
    provider_label: &str,
    model: &ModelOption,
    needle: &str,
) -> bool {
    needle.is_empty()
        || provider_id.to_lowercase().contains(needle)
        || provider_label.to_lowercase().contains(needle)
        || model.id.to_lowercase().contains(needle)
        || model.model.to_lowercase().contains(needle)
        || model.name.to_lowercase().contains(needle)
}

fn nav_keys(ui: &Ui) -> (bool, bool, bool, bool) {
    ui.input(|input| {
        let pressed = |key| {
            input.events.iter().any(|event| {
                matches!(event, egui::Event::Key { key: event_key, pressed: true, .. } if *event_key == key)
            })
        };
        (
            pressed(egui::Key::ArrowUp),
            pressed(egui::Key::ArrowDown),
            pressed(egui::Key::Enter),
            pressed(egui::Key::Escape),
        )
    })
}

fn fit_field_label(
    ui: &Ui,
    label: &str,
    color: Color32,
    budget: f32,
    has_status_dot: bool,
    font_size: f32,
) -> egui::text::LayoutJob {
    let indent = if has_status_dot { 15.0 } else { 0.0 };
    let job = |text: &str| {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            text,
            indent,
            egui::TextFormat {
                font_id: theme::font(font_size),
                color,
                valign: egui::Align::Center,
                ..Default::default()
            },
        );
        job
    };
    let fits = |candidate: &egui::text::LayoutJob| {
        ui.fonts(|fonts| fonts.layout_job(candidate.clone()).size().x) <= budget
    };
    let full = job(label);
    if fits(&full) {
        return full;
    }
    let (mut low, mut high) = (4usize, label.chars().count());
    while low < high {
        let middle = (low + high).div_ceil(2);
        if fits(&job(&elide_middle(label, middle))) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    job(&elide_middle(label, low))
}

fn elide_middle(value: &str, max: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max || max < 4 {
        return value.to_string();
    }
    let keep = max - 1;
    let head = keep.div_ceil(2);
    let tail = keep - head;
    format!(
        "{}…{}",
        chars[..head].iter().collect::<String>(),
        chars[chars.len() - tail..].iter().collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> ModelList {
        ModelList {
            groups: vec![corpus_core::ModelProviderGroup {
                id: "openrouter".into(),
                label: "OpenRouter".into(),
                models: vec![ModelOption {
                    id: "openrouter/acme/Qwen-Coder".into(),
                    model: "acme/Qwen-Coder".into(),
                    name: "Qwen Coder 32B".into(),
                }],
            }],
        }
    }

    #[test]
    fn search_matches_provider_name_full_id_and_display_name_case_insensitively() {
        let list = catalog();
        for query in ["router", "OPENROUTER", "acme/qwen", "coder 32b"] {
            assert_eq!(matching_groups(&list, &query.to_lowercase()).len(), 1);
        }
        assert!(matching_groups(&list, "missing").is_empty());
    }

    #[test]
    fn empty_search_keeps_every_model() {
        assert_eq!(matching_groups(&catalog(), "")[0].1.len(), 1);
    }

    #[test]
    fn middle_elision_preserves_both_ends() {
        assert_eq!(elide_middle("qwen3:8b", 20), "qwen3:8b");
        let elided = elide_middle("hf.co/unsloth/Qwen3-30B:Q4_K_M", 20);
        assert!(elided.starts_with("hf.co/"));
        assert!(elided.ends_with("Q4_K_M"));
        assert_eq!(elided.chars().count(), 20);
    }
}
