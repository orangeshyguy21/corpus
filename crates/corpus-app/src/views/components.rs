//! Shared command-centre presentation primitives.
//!
//! These own visual hierarchy only. Views keep navigation, store calls, and
//! confirmation state; components receive plain text/state and emit responses.

// Several primitives land here before their consuming Project/Agent chunks so
// the foundation can be assessed as one visual system.
#![allow(dead_code)]

use egui::{Color32, RichText, Ui};
use egui_phosphor::regular as ph;

use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusTone {
    Neutral,
    Interaction,
    Healthy,
    HealthyMuted,
    Warning,
    Danger,
}

impl StatusTone {
    pub fn color(self) -> Color32 {
        match self {
            Self::Neutral => theme::TEXT_MUTED,
            Self::Interaction => theme::INTERACTION,
            Self::Healthy => theme::HEALTHY,
            Self::HealthyMuted => theme::HEALTHY.gamma_multiply(0.55),
            Self::Warning => theme::WARN,
            Self::Danger => theme::SIGNAL_RED,
        }
    }
}

/// Fixed page-title band. The caller owns action order/state; this component
/// only makes the title, metadata, action rail, and amber keyline consistent.
pub fn page_header<R>(
    ui: &mut Ui,
    kind: &str,
    name: &str,
    metadata: &str,
    actions: impl FnOnce(&mut Ui) -> R,
) -> R {
    page_header_with_context(ui, kind, name, metadata, |_| {}, actions).1
}

/// Fixed page-title band with a compact, view-owned control placed directly
/// after the title. This keeps editor modes in the command rail instead of
/// spending a second row on navigation.
pub fn page_header_with_context<C, R>(
    ui: &mut Ui,
    kind: &str,
    name: &str,
    metadata: &str,
    context: impl FnOnce(&mut Ui) -> C,
    actions: impl FnOnce(&mut Ui) -> R,
) -> (C, R) {
    let result = ui
        .horizontal(|ui| {
            let context_result = ui
                .horizontal(|ui| {
                    ui.label(theme::display_text(kind, 24.0, true, theme::INTERACTION));
                    ui.label(RichText::new("/").size(24.0).color(theme::TEXT_FAINT));
                    ui.add(
                        egui::Label::new(theme::display_text(name, 24.0, true, theme::TEXT))
                            .truncate(),
                    );
                    ui.add_space(10.0);
                    context(ui)
                })
                .inner;
            let action_result = ui
                .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let result = actions(ui);
                    if !metadata.is_empty() {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(metadata)
                                .size(11.0)
                                .monospace()
                                .color(theme::TEXT_FAINT),
                        );
                    }
                    result
                })
                .inner;
            (context_result, action_result)
        })
        .inner;
    ui.add_space(8.0);
    amber_rule(ui);
    result
}

/// A translucent, square-edged card with a bracketed command title, a muted
/// right-aligned Simplified Chinese annotation, and clipped content.
pub fn panel_card<R>(
    ui: &mut Ui,
    title: &str,
    annotation: &str,
    body: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    egui::Frame::default()
        .fill(theme::card_fill())
        .stroke(egui::Stroke::new(1.0_f32, theme::CARD_BORDER))
        .corner_radius(egui::CornerRadius::ZERO)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(theme::section_heading(title));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(theme::sc_annotation(annotation));
                    });
                });
                ui.add_space(8.0);
                soft_rule(ui);
                ui.add_space(10.0);
            }
            body(ui)
        })
}

/// Compact state badge. Icon + text means status never depends on color alone.
pub fn status_badge(ui: &mut Ui, text: &str, tone: StatusTone) -> egui::Response {
    let color = tone.color();
    let icon = match tone {
        StatusTone::Healthy | StatusTone::HealthyMuted => ph::CHECK_CIRCLE,
        StatusTone::Warning => ph::WARNING,
        StatusTone::Danger => ph::X_CIRCLE,
        StatusTone::Neutral | StatusTone::Interaction => ph::DOT_OUTLINE,
    };
    let job = theme::icon_label(
        icon,
        13.0,
        color,
        &format!("  {}", text.to_uppercase()),
        theme::mono(10.5),
        color,
    );
    ui.add(
        egui::Button::new(job)
            .fill(color.gamma_multiply(0.10))
            .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.70)))
            .corner_radius(egui::CornerRadius::same(2))
            .sense(egui::Sense::hover()),
    )
}

/// A small metric block used by Project status bands and corpus summaries.
pub fn metric_cell(ui: &mut Ui, label: &str, value: impl Into<String>, tone: StatusTone) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label.to_uppercase())
                .size(10.5)
                .monospace()
                .color(theme::TEXT_FAINT),
        );
        ui.add_space(3.0);
        ui.label(
            RichText::new(value.into())
                .size(20.0)
                .monospace()
                .strong()
                .color(tone.color()),
        );
    });
}

/// Equal-width score cells used by the System Status card. At narrow widths
/// the strip becomes a 2×2 grid without changing the visual grammar.
pub fn score_strip(ui: &mut Ui, metrics: &[(&str, String)]) {
    if metrics.is_empty() {
        return;
    }
    let columns = score_strip_columns(ui.available_width(), metrics.len());
    let rows = metrics.len().div_ceil(columns);
    let row_height = 78.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_height * rows as f32),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        egui::CornerRadius::ZERO,
        egui::Stroke::new(1.0_f32, theme::CARD_BORDER),
        egui::StrokeKind::Inside,
    );

    let cell_width = rect.width() / columns as f32;
    for row in 1..rows {
        let y = rect.top() + row_height * row as f32;
        painter.hline(
            rect.left()..=rect.right(),
            y,
            egui::Stroke::new(1.0_f32, theme::CARD_BORDER),
        );
    }
    for column in 1..columns {
        let x = rect.left() + cell_width * column as f32;
        painter.vline(
            x,
            rect.top()..=rect.bottom(),
            egui::Stroke::new(1.0_f32, theme::CARD_BORDER),
        );
    }

    for (index, (label, value)) in metrics.iter().enumerate() {
        let row = index / columns;
        let column = index % columns;
        let cell = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + column as f32 * cell_width,
                rect.top() + row as f32 * row_height,
            ),
            egui::vec2(cell_width, row_height),
        )
        .shrink2(egui::vec2(14.0, 10.0));
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("score_cell", index))
                .max_rect(cell)
                .layout(egui::Layout::top_down(egui::Align::LEFT)),
        );
        child.label(
            RichText::new(label.to_uppercase())
                .size(10.5)
                .monospace()
                .color(theme::TEXT_FAINT),
        );
        child.add_space(4.0);
        child.label(
            RichText::new(value)
                .font(theme::display(29.0, true))
                .color(theme::INTERACTION),
        );
    }
}

fn score_strip_columns(available_width: f32, metric_count: usize) -> usize {
    if available_width >= 520.0 {
        metric_count
    } else {
        metric_count.min(2)
    }
}

/// Framed overflow menu button shared by Project and Agent action rails.
pub fn action_menu<R>(
    ui: &mut Ui,
    contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    ui.scope(|ui| {
        let style = ui.style_mut();
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.bg_fill = theme::PANEL;
            widget.weak_bg_fill = theme::PANEL;
            widget.bg_stroke = egui::Stroke::new(1.0_f32, theme::KEYLINE);
            widget.corner_radius = theme::CONTROL_RADIUS;
        }
        ui.menu_button(
            theme::icon_text(ph::DOTS_THREE_VERTICAL, 17.0, theme::INTERACTION),
            contents,
        )
    })
    .inner
}

pub fn amber_rule(ui: &mut Ui) {
    rule(ui, theme::KEYLINE_STRONG);
}

pub fn soft_rule(ui: &mut Ui) {
    rule(ui, theme::KEYLINE_SOFT);
}

/// Terminal-style centered banner used by ASCII diagrams and data blocks.
/// It derives the dash count from the live width, so the same grammar scales
/// from a narrow card to a full-width report without bitmap assets.
pub fn ascii_banner(ui: &mut Ui, title: &str) -> egui::Response {
    use egui::text::{LayoutJob, TextFormat};

    let font = theme::mono(10.5);
    let glyph_w = ui
        .painter()
        .layout_no_wrap("M".to_string(), font.clone(), theme::TEXT_FAINT)
        .size()
        .x
        .max(1.0);
    let title = format!("[ {} ]", title.to_uppercase());
    let columns = (ui.available_width() / glyph_w).floor() as usize;
    let rail = columns.saturating_sub(title.chars().count() + 2);
    let left = rail / 2;
    let right = rail - left;
    let mut job = LayoutJob::default();
    let dim = TextFormat {
        font_id: font.clone(),
        color: theme::TEXT_FAINT,
        ..Default::default()
    };
    let signal = TextFormat {
        font_id: font,
        color: theme::INTERACTION,
        ..Default::default()
    };
    job.append(&format!("+{}", "-".repeat(left)), 0.0, dim.clone());
    job.append(&title, 0.0, signal);
    job.append(&format!("{}+", "-".repeat(right)), 0.0, dim);
    ui.add(egui::Label::new(job).selectable(false).truncate())
}

/// Closing rail for an ASCII block, sized with the same monospace metric as
/// [`ascii_banner`].
pub fn ascii_rule(ui: &mut Ui) -> egui::Response {
    let font = theme::mono(10.5);
    let glyph_w = ui
        .painter()
        .layout_no_wrap("M".to_string(), font.clone(), theme::TEXT_FAINT)
        .size()
        .x
        .max(1.0);
    let columns = (ui.available_width() / glyph_w).floor() as usize;
    let rail = format!("+{}+", "-".repeat(columns.saturating_sub(2)));
    ui.add(
        egui::Label::new(RichText::new(rail).font(font).color(theme::TEXT_FAINT))
            .selectable(false)
            .truncate(),
    )
}

/// Reusable interactive row inside an ASCII diagram. Callers own the
/// `LayoutJob` content (tree branches, tables, topology labels); this owns the
/// shared selection/hover surface, clipping, and monospace block sizing.
pub fn ascii_row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    mut job: egui::text::LayoutJob,
    selected: bool,
) -> egui::Response {
    job.wrap.max_width = (ui.available_width() - 8.0).max(0.0);
    let galley = ui.fonts(|fonts| fonts.layout_job(job));
    let height = (galley.size().y + 6.0).max(24.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let response = ui.interact(rect, ui.make_persistent_id(id), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 1.0, theme::ROW_HOVER);
    }
    if selected {
        ui.painter().line_segment(
            [rect.left_top(), rect.left_bottom()],
            egui::Stroke::new(2.0_f32, theme::INTERACTION),
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter().galley(
        egui::pos2(rect.left() + 4.0, rect.top() + 3.0),
        galley,
        theme::TEXT,
    );
    response
}

fn rule(ui: &mut Ui, color: Color32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        egui::Stroke::new(1.0_f32, color),
    );
}

/// Static technical canvas. It paints only; no widget allocation, state, timer,
/// or repaint request. Major grid intervals and corner chevrons create the
/// command-centre texture without putting scanlines through text.
pub fn paint_command_canvas(ui: &Ui) {
    // This is atmosphere, not layout. A relatively large module keeps the
    // texture legible on wide displays without drawing a line through every
    // control or paragraph.
    const GRID: f32 = 96.0;
    let rect = ui.max_rect();
    let painter = ui.painter().with_clip_rect(rect);
    let first_x = (rect.left() / GRID).floor() * GRID;
    let first_y = (rect.top() / GRID).floor() * GRID;

    let mut x = first_x;
    let mut column = 0_u32;
    while x <= rect.right() {
        let color = if column.is_multiple_of(3) {
            theme::GRID_MARK
        } else {
            theme::GRID_LINE
        };
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0_f32, color),
        );
        x += GRID;
        column += 1;
    }

    let mut y = first_y;
    let mut row = 0_u32;
    while y <= rect.bottom() {
        let color = if row.is_multiple_of(3) {
            theme::GRID_MARK
        } else {
            theme::GRID_LINE
        };
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0_f32, color),
        );
        y += GRID;
        row += 1;
    }

    let corner = egui::Rect::from_min_size(
        egui::pos2((rect.right() - 180.0).max(rect.left()), rect.top()),
        egui::vec2(180.0_f32.min(rect.width()), 72.0_f32.min(rect.height())),
    );
    for offset in [24.0, 72.0, 120.0] {
        painter.line_segment(
            [
                egui::pos2(corner.left() + offset, corner.top()),
                egui::pos2(corner.left() + offset + 72.0, corner.bottom()),
            ],
            egui::Stroke::new(1.0_f32, theme::GRID_MARK),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_tones_do_not_alias_danger_and_interaction() {
        assert_ne!(StatusTone::Interaction.color(), StatusTone::Danger.color());
        assert_ne!(theme::INTERACTION, theme::SIGNAL_RED);
        assert_eq!(StatusTone::Healthy.color(), theme::HEALTHY);
        assert_eq!(
            StatusTone::HealthyMuted.color(),
            theme::HEALTHY.gamma_multiply(0.55)
        );
    }

    #[test]
    fn score_strip_collapses_to_two_columns_on_narrow_cards() {
        assert_eq!(score_strip_columns(900.0, 4), 4);
        assert_eq!(score_strip_columns(519.0, 4), 2);
        assert_eq!(score_strip_columns(300.0, 1), 1);
    }
}
