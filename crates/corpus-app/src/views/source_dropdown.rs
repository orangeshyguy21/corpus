//! The shared source-rev dropdown (app-wide): a hand-rolled flat field in
//! the plugin picker's style — PANEL fill, 1px HAIRLINE, radius 2, the
//! `repo: rev` text in monospace, a caret at the right edge, and a popup
//! listing the available revs. Used by the top bar (one per source) and
//! the project page's Sources section (the pins persist on project.yaml).
//! A branch rev (`main`/`master`) served from an absent or expired rev
//! cache renders amber with a provenance tooltip — it resolves to the
//! recorded snapshot, not today's head.

use egui::{Align2, Ui};
use egui_phosphor::regular as ph;

use corpus_core::SourceRevs;

use crate::theme;

/// The `repo: rev` dropdown for one source. `id_salt` must be unique per
/// render location (top bar vs project page) — the popup keys off it.
/// Returns `Some(rev)` when the user picks a DIFFERENT rev; the caller
/// persists the pin (top bar: `AppState::set_source_pin`).
pub fn source_dropdown(
    ui: &mut Ui,
    id_salt: &str,
    source: &SourceRevs,
    selected: &str,
) -> Option<String> {
    let stale = stale_branch(source, selected);
    let tooltip = rev_tooltip(source, selected);
    let label = format!("{}: {}", source.name, selected);
    let text_color = if stale { theme::WARN } else { theme::TEXT };

    // Field: size to the label (min 120px), 28px tall — the plugin
    // picker's chrome at the top bar's density.
    let galley = ui
        .painter()
        .layout_no_wrap(label.clone(), egui::FontId::monospace(13.0), text_color);
    let width = (galley.size().x + 10.0 + 28.0).max(120.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::click());
    // The popup gets its OWN id (plugin_picker.rs): reusing one id across
    // the background field layer and the foreground popup layer trips
    // egui's WidgetRects guard.
    let id = ui.id().with(id_salt);
    let popup_id = id.with("popup");
    let response = ui.interact(rect, id, egui::Sense::click());
    if response.clicked() {
        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
    }
    response.clone().on_hover_text(tooltip);

    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(2), theme::PANEL);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(2),
        egui::Stroke::new(1.0_f32, theme::HAIRLINE),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        &label,
        egui::FontId::monospace(13.0),
        text_color,
    );
    painter.text(
        egui::pos2(rect.right() - 14.0, rect.center().y),
        Align2::CENTER_CENTER,
        ph::CARET_DOWN,
        egui::FontId::new(13.0, egui::FontFamily::Name("phosphor".into())),
        theme::TEXT_MUTED,
    );

    let mut picked = None;
    egui::popup::popup_above_or_below_widget(
        ui,
        popup_id,
        &response,
        egui::AboveOrBelow::Below,
        egui::popup::PopupCloseBehavior::CloseOnClickOutside,
        |ui| {
            ui.set_min_width(rect.width());
            for rev in &source.revs {
                if ui
                    .selectable_label(
                        rev == selected,
                        egui::RichText::new(rev.clone()).monospace().color(theme::TEXT),
                    )
                    .clicked()
                    && rev != selected
                {
                    picked = Some(rev.clone());
                }
            }
        },
    );
    picked
}

/// Whether the selected rev is a branch (`main`/`master`) served from an
/// absent or expired rev cache: the pick will resolve to the recorded
/// snapshot, NOT today's head. The dropdown turns amber for these.
pub fn stale_branch(source: &SourceRevs, selected: &str) -> bool {
    let is_branch = matches!(selected, "main" | "master");
    is_branch
        && match source.refs_fetched {
            None => true,
            Some(fetched) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now.saturating_sub(fetched) >= corpus_core::REV_CACHE_TTL_SECS
            }
        }
}

/// The persist-on-hover rev-list provenance for a source dropdown: when
/// the list was fetched, and what a branch pick resolves to.
pub fn rev_tooltip(source: &SourceRevs, selected: &str) -> String {
    let mut text = String::from("rev list provenance");
    match source.refs_fetched {
        None => text.push_str(": no rev cache — the remote never answered; revs are the manifest pin + main placeholders"),
        Some(fetched) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let age = now.saturating_sub(fetched);
            let age_txt = if age >= 3600 {
                format!("{}h{}m", age / 3600, (age % 3600) / 60)
            } else {
                format!("{}m", age / 60)
            };
            text.push_str(&format!(": cached {age_txt} ago"));
            if age >= corpus_core::REV_CACHE_TTL_SECS {
                text.push_str(" (STALE — past the refresh; reused because the network was unavailable)");
            } else if selected == "main" || selected == "master" {
                text.push_str(&format!(
                    " — {selected} resolves to that snapshot; the head may have moved since"
                ));
            }
        }
    }
    text
}
