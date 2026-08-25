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
/// `running_rev` is the rev the ENVIRONMENT is actually running (the top
/// bar passes `Some("v0.18.0-rc.0")`; the project page has no live probe
/// and passes `None`): a match paints a green check, a mismatch an amber
/// warning, and when the running rev is selectable the warning is a
/// one-click "adopt it" affordance.
///
/// Returns `Some(rev)` when the user picks a DIFFERENT rev — from the popup
/// OR by clicking the adopt affordance; the caller persists the pin (top
/// bar: `AppState::set_source_pin`).
pub fn source_dropdown(
    ui: &mut Ui,
    id_salt: &str,
    source: &SourceRevs,
    selected: &str,
    running_rev: Option<&str>,
) -> Option<String> {
    let stale = stale_branch(source, selected);
    let tooltip = rev_tooltip(source, selected);
    let label = format!("{}: {}", source.name, selected);
    let text_color = if stale { theme::WARN } else { theme::TEXT };

    // Match state against the live environment: Some(true) = the pin equals
    // what is running, Some(false) = a mismatch, None = no live probe.
    let running_match = running_rev.map(|r| r == selected);
    // The mismatch is fixable in one click only when the running rev is
    // actually offered in this source's list.
    let adoptable = running_match == Some(false)
        && running_rev.is_some_and(|r| source.revs.iter().any(|x| x == r));

    // Field: size to the label (min 120px), 28px tall — the plugin
    // picker's chrome at the top bar's density. Reserve an indicator slot
    // (left of the caret) when there is a match state to show.
    let galley =
        ui.painter()
            .layout_no_wrap(label.clone(), egui::FontId::monospace(13.0), text_color);
    let indicator = running_match.is_some();
    let extra = if indicator { 18.0 } else { 0.0 };
    let width = (galley.size().x + 10.0 + 28.0 + extra).max(120.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 28.0), egui::Sense::click());
    // The popup gets its OWN id (plugin_picker.rs): reusing one id across
    // the background field layer and the foreground popup layer trips
    // egui's WidgetRects guard.
    let id = ui.id().with(id_salt);
    let popup_id = id.with("popup");

    // The indicator sits just left of the caret. When adoptable it is its
    // own click target, so clicking it adopts the running rev instead of
    // opening the popup — the field's click rect excludes it.
    let ind_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 30.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    let mut picked = None;
    if adoptable {
        let adopt = ui.interact(ind_rect, id.with("adopt"), egui::Sense::click());
        adopt
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(format!(
                "environment runs {} — click to pin it",
                running_rev.unwrap_or_default()
            ));
        if adopt.clicked() {
            picked = Some(running_rev.unwrap_or_default().to_string());
        }
    }
    let field_click_rect = if adoptable {
        egui::Rect::from_min_max(rect.min, egui::pos2(ind_rect.left(), rect.max.y))
    } else {
        rect
    };
    let response = ui.interact(field_click_rect, id, egui::Sense::click());
    if response.clicked() && picked.is_none() {
        ui.memory_mut(|mem| mem.toggle_popup(popup_id));
    }
    response.clone().on_hover_text(tooltip);

    let painter = ui.painter();
    painter.rect_filled(rect, theme::CONTROL_RADIUS, theme::PANEL);
    painter.rect_stroke(
        rect,
        theme::CONTROL_RADIUS,
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
    // Match indicator: green check when the pin matches the running env,
    // amber warning otherwise (the warning doubles as the adopt target).
    if let Some(matches) = running_match {
        let (glyph, color) = if matches {
            (ph::CHECK, theme::HEALTHY)
        } else {
            (ph::WARNING, theme::WARN)
        };
        painter.text(
            ind_rect.center(),
            Align2::CENTER_CENTER,
            glyph,
            egui::FontId::new(13.0, egui::FontFamily::Name("phosphor".into())),
            color,
        );
    }
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
            ui.set_min_width(rect.width());
            for rev in &source.revs {
                if ui
                    .selectable_label(
                        rev == selected,
                        egui::RichText::new(rev.clone())
                            .monospace()
                            .color(theme::TEXT),
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
