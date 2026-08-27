//! Curated plugin installation controls shared by first-run and project
//! configuration flows. Distribution policy stays in corpus-core; this module
//! only renders the embedded catalog and dispatches an install request.

use std::collections::BTreeMap;

use egui::{RichText, Ui};

use crate::state::AppState;
use crate::theme;

/// Render the Corpus-curated catalog. Returns the result of an install click,
/// if any; callers own the surrounding dialog and toast presentation.
pub fn curated_plugin_list(ui: &mut Ui, state: &mut AppState) -> Option<Result<bool, String>> {
    let plugins = match corpus_core::curated_plugins() {
        Ok(plugins) => plugins,
        Err(error) => {
            ui.colored_label(theme::SIGNAL_RED, format!("catalog unavailable: {error}"));
            return None;
        }
    };
    let installed: BTreeMap<_, _> = state
        .plugins()
        .iter()
        .filter_map(|plugin| {
            plugin
                .version
                .as_ref()
                .map(|version| (plugin.name.clone(), version.clone()))
        })
        .collect();
    let busy = state
        .plugin_operation()
        .is_some_and(|operation| operation.state == crate::state::PluginOperationState::Running);
    let mut requested = None;

    for plugin in plugins {
        egui::Frame::new()
            .fill(theme::PANEL)
            .stroke(egui::Stroke::new(1.0_f32, theme::HAIRLINE))
            .corner_radius(theme::CONTROL_RADIUS)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&plugin.name).strong());
                        ui.label(
                            RichText::new(&plugin.description)
                                .size(12.0)
                                .color(theme::TEXT_MUTED),
                        );
                        let mut metadata = format!("{} · v{}", plugin.id, plugin.version);
                        if plugin.requirements.iter().any(|requirement| {
                            *requirement == corpus_core::CuratedPluginRequirement::Docker
                        }) {
                            metadata.push_str(" · requires Docker");
                        }
                        ui.label(
                            RichText::new(metadata)
                                .monospace()
                                .size(11.0)
                                .color(theme::TEXT_FAINT),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let installed_version = installed.get(&plugin.id);
                        let current =
                            installed_version.is_some_and(|version| version == &plugin.version);
                        let label = if current {
                            "Installed"
                        } else if installed_version.is_some() {
                            "Update"
                        } else {
                            "Install"
                        };
                        if ui
                            .add_enabled(!busy && !current, egui::Button::new(label))
                            .clicked()
                        {
                            requested = Some(state.start_curated_plugin_install(&plugin.id));
                        }
                    });
                });
            });
        ui.add_space(6.0);
    }

    requested
}
