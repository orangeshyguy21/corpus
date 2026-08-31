//! The shared plugin picker: a
//! hand-rolled flat dropdown over the discovered environment plugins with a
//! probe badge per entry — health-green when ready, signal-red when a probe
//! failed, and muted when discovered but not probed (painted with the painter,
//! not a font glyph). The field is
//! a flat PANEL box (1px HAIRLINE, radius 2) with the plugin name in
//! monospace 14px and a `caret_down` arrow; the popup lists each plugin
//! with its own badge. A Re-probe affordance schedules a fresh host-side
//! probe aggregation.

use egui::{Align2, Color32, Ui};
use egui_phosphor::regular as ph;

use corpus_core::PluginStatus;

use crate::theme;

/// The plugin dropdown with a live probe badge per entry (spec §5): a flat
/// field ~360×28 with the current plugin's badge + name + caret; the popup
/// lists every discovered plugin with its badge and the failing plugin's
/// notes inline (pure probe detail on hover).
pub fn plugin_picker(
    ui: &mut Ui,
    current: &mut String,
    plugins: &[PluginStatus],
    needs_probe: &mut bool,
) {
    if plugins.is_empty() {
        ui.horizontal(|ui| {
            ui.weak("no environment plugins installed");
            if theme::house_button(ui, "Re-probe").clicked() {
                *needs_probe = true;
            }
        });
        return;
    }

    if !plugins.iter().any(|plugin| plugin.name == *current) {
        if let Some(plugin) = plugins
            .iter()
            .find(|plugin| plugin.ready)
            .or(plugins.first())
        {
            *current = plugin.name.clone();
            *needs_probe = true;
        }
    }

    let field_size = egui::vec2(ui.available_width(), 34.0);
    let (rect, _) = ui.allocate_exact_size(field_size, egui::Sense::click());
    let id = ui.id().with("plugin_picker_field");
    // The popup gets its OWN id: egui registers the field's interact on the
    // BACKGROUND layer but the popup's content on the FOREGROUND layer, and
    // reusing one id across layers in the same frame trips egui's
    // WidgetRects guard (panics on the project screen, where the field is a
    // background widget; it only escaped notice inside Windows, which are
    // already foreground). The toggle memory must key on the popup id.
    let popup_id = id.with("popup");
    let response = ui.interact(rect, id, egui::Sense::click());
    if response.clicked() {
        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
    }

    let painter = ui.painter();
    painter.rect_filled(rect, theme::CONTROL_RADIUS, theme::PANEL);
    painter.rect_stroke(
        rect,
        theme::CONTROL_RADIUS,
        egui::Stroke::new(1.0_f32, theme::HAIRLINE),
        egui::StrokeKind::Inside,
    );
    // Probe badge + plugin name (monospace 14px) + caret.
    paint_badge(
        painter,
        egui::pos2(rect.left() + 16.0, rect.center().y),
        current,
        plugins,
    );
    painter.text(
        egui::pos2(rect.left() + 30.0, rect.center().y),
        Align2::LEFT_CENTER,
        current.as_str(),
        egui::FontId::monospace(14.0),
        theme::TEXT,
    );
    painter.text(
        egui::pos2(rect.right() - 14.0, rect.center().y),
        Align2::CENTER_CENTER,
        ph::CARET_DOWN,
        egui::FontId::new(13.0, egui::FontFamily::Name("phosphor".into())),
        theme::TEXT_MUTED,
    );

    egui::popup::popup_above_or_below_widget(
        ui,
        popup_id,
        &response,
        egui::AboveOrBelow::Below,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(field_size.x);
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .id_salt("plugin_list")
                .show(ui, |ui| {
                    for status in plugins {
                        if row(ui, status, current) {
                            *needs_probe = true;
                            ui.memory_mut(|memory| memory.close_popup());
                        }
                    }
                });
        },
    );
}

/// A popup row: probe badge + selectable plugin name + failing notes.
fn row(ui: &mut Ui, status: &PluginStatus, current: &mut String) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        let color = badge_color(status);
        ui.painter().circle_filled(dot.center(), 4.0, color);
        let selected = *current == status.name;
        let resp = ui.selectable_label(
            selected,
            egui::RichText::new(&status.name)
                .monospace()
                .color(theme::TEXT),
        );
        if resp.clicked() {
            *current = status.name.clone();
            changed = true;
        }
        if status.probed && !status.ready && !status.notes.is_empty() {
            resp.on_hover_text(&status.notes);
            let short: String = status.notes.chars().take(48).collect();
            ui.weak(short.to_string());
        }
    });
    changed
}

/// The probe badge colour for a plugin: health-green when the live probe is
/// ready, signal-red on a checked failure, muted when not checked.
fn badge_color(status: &PluginStatus) -> Color32 {
    if !status.probed {
        theme::TEXT_FAINT
    } else if status.ready {
        theme::HEALTHY
    } else {
        theme::SIGNAL_RED
    }
}

/// Paint the probe badge for `name` at `center`.
fn paint_badge(painter: &egui::Painter, center: egui::Pos2, name: &str, plugins: &[PluginStatus]) {
    let color = plugins
        .iter()
        .find(|plugin| plugin.name == name)
        .map(badge_color)
        .unwrap_or(theme::TEXT_FAINT);
    painter.circle_filled(center, 4.0, color);
}
