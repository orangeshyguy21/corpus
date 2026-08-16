//! GFM table rendering for the management-chat log.
//!
//! Why our own renderer instead of egui_commonmark 0.20's built-in one: that
//! crate draws a table as a bare striped `egui::Grid` — borderless, with
//! unwrapped cells (long cells blow the row out of the bubble), and every
//! assistant message's first table shares ONE Grid id
//! (`ui.id().with("_table").with(0)`), which makes egui's debug id-clash
//! check paint red "🔥 ID clash" errors into the log whenever two bubbles
//! each carry a table.
//!
//! So the assistant text is segmented first: markdown between tables goes to
//! egui_commonmark verbatim, tables are parsed by pulldown-cmark into plain
//! cells and drawn here (bordered frame, content-aware column widths, wrapped
//! cells, and ZERO widget ids — nothing can clash).

use std::sync::Arc;

use eframe::egui;
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};

/// Horizontal alignment of a table column (pulldown's type mapped at parse
/// time so pulldown stays quarantined to this module).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellAlign {
    Left,
    Center,
    Right,
}

/// A parsed GFM table: header row, column alignments, body rows. Cell
/// contents are PLAIN text (inline markup resolved away during parse).
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub header: Vec<String>,
    pub alignments: Vec<CellAlign>,
    pub rows: Vec<Vec<String>>,
}

/// One piece of an assistant message, in source order.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// Markdown handed to egui_commonmark verbatim.
    Markdown(String),
    /// A table rendered by [`show_table`].
    Table(Table),
}

/// Split markdown into text/table segments. The parser is pulldown-cmark with
/// the SAME option set egui_commonmark parses with, so a segment boundary is
/// exactly where that renderer would have drawn its table.
pub fn split(text: &str) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut cursor = 0usize;
    let mut events = Parser::new_ext(text, parser_options()).into_offset_iter();

    while let Some((event, range)) = events.next() {
        let Event::Start(Tag::Table(alignments)) = event else {
            continue;
        };
        // Markdown before the table.
        push_markdown(&mut out, &text[cursor..range.start]);

        let mut table = Table {
            header: Vec::new(),
            alignments: alignments
                .iter()
                .map(|a| match a {
                    Alignment::None | Alignment::Left => CellAlign::Left,
                    Alignment::Center => CellAlign::Center,
                    Alignment::Right => CellAlign::Right,
                })
                .collect(),
            rows: Vec::new(),
        };

        // Consume inner events until End(Table). Cells collect only Text/Code
        // events so inline markup (**bold**, `code`) becomes plain text.
        let mut current_row: Vec<String> = Vec::new();
        let mut cell = String::new();
        let mut cell_open = false;
        let mut closed = false;

        loop {
            match events.next() {
                None => break,
                Some((ev, _)) => match ev {
                    Event::End(TagEnd::Table) => {
                        closed = true;
                        break;
                    }
                    Event::Start(Tag::TableHead) => {
                        current_row.clear();
                    }
                    Event::End(TagEnd::TableHead) => {
                        table.header = std::mem::take(&mut current_row);
                    }
                    Event::Start(Tag::TableRow) => {
                        current_row.clear();
                    }
                    Event::End(TagEnd::TableRow) => {
                        table.rows.push(std::mem::take(&mut current_row));
                    }
                    Event::Start(Tag::TableCell) => {
                        cell.clear();
                        cell_open = true;
                    }
                    Event::End(TagEnd::TableCell) => {
                        current_row.push(cell.trim().to_string());
                        cell_open = false;
                    }
                    Event::Text(t) | Event::Code(t) if cell_open => {
                        cell.push_str(&t);
                    }
                    Event::SoftBreak if cell_open => {
                        cell.push(' ');
                    }
                    _ => {}
                },
            }
        }

        cursor = range.end;
        if closed && (!table.header.is_empty() || !table.rows.is_empty()) {
            out.push(Segment::Table(table));
        } else {
            // Degenerate / malformed: fall back to raw markdown so nothing is
            // silently lost.
            push_markdown(&mut out, &text[range.start..cursor]);
        }
    }

    push_markdown(&mut out, &text[cursor..]);
    out
}

fn push_markdown(out: &mut Vec<Segment>, text: &str) {
    if !text.trim().is_empty() {
        out.push(Segment::Markdown(text.to_string()));
    }
}

fn parser_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_DEFINITION_LIST
}

// --- rendering ---

const CELL_PAD_X: f32 = 8.0;
const CELL_PAD_Y: f32 = 4.0;
const MIN_COL_W: f32 = 40.0;
const CORNER: egui::CornerRadius = egui::CornerRadius::same(2);

/// Draw a parsed table directly onto the painter (no widget ids → no clash).
pub fn show_table(ui: &mut egui::Ui, table: &Table) {
    let ncols = table
        .header
        .len()
        .max(table.rows.iter().map(|r| r.len()).max().unwrap_or(0))
        .max(1);
    if table.header.is_empty() && table.rows.is_empty() {
        return;
    }

    let avail = ui.available_width().max(MIN_COL_W * 2.0);
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text_color = crate::theme::TEXT;
    let header_color = crate::theme::TEXT;

    // --- content-aware column widths ---
    let mut widths = vec![MIN_COL_W; ncols];
    for (ci, cell) in table.header.iter().enumerate() {
        if ci < ncols {
            widths[ci] = widths[ci].max(natural_w(ui, &font, cell) + 2.0 * CELL_PAD_X);
        }
    }
    for row in &table.rows {
        for (ci, cell) in row.iter().enumerate() {
            if ci < ncols {
                widths[ci] = widths[ci].max(natural_w(ui, &font, cell) + 2.0 * CELL_PAD_X);
            }
        }
    }
    for w in widths.iter_mut() {
        *w = w.min(avail);
    }
    fit_widths(&mut widths, avail);

    // --- lay out every cell (wrapped to its final width) and measure rows ---
    let wrap_widths: Vec<f32> = widths.iter().map(|w| (w - 2.0 * CELL_PAD_X).max(8.0)).collect();
    let aligns = column_aligns(table, ncols);

    let header_galleys: Vec<Arc<egui::Galley>> = table
        .header
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            galley_job(ui, &font, c, wrap_widths[ci], aligns[ci], header_color)
        })
        .collect();
    let header_h = header_galleys
        .iter()
        .map(|g| g.size().y)
        .fold(0.0_f32, f32::max)
        + 2.0 * CELL_PAD_Y;

    let mut row_galleys: Vec<(Vec<Arc<egui::Galley>>, f32)> = Vec::new();
    for row in &table.rows {
        let galleys: Vec<Arc<egui::Galley>> = (0..ncols)
            .map(|ci| {
                let text = row.get(ci).map(String::as_str).unwrap_or("");
                galley_job(ui, &font, text, wrap_widths[ci], aligns[ci], text_color)
            })
            .collect();
        let h = galleys.iter().map(|g| g.size().y).fold(0.0, f32::max) + 2.0 * CELL_PAD_Y;
        row_galleys.push((galleys, h));
    }

    let total_w: f32 = widths.iter().sum();
    let total_h = header_h + row_galleys.iter().map(|(_, h)| h).sum::<f32>();

    // --- allocate + paint ---
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total_w, total_h), egui::Sense::hover());
    let p = ui.painter();

    // Body + border.
    p.rect_filled(rect, CORNER, crate::theme::EDITOR_BG);
    p.rect_stroke(
        rect,
        CORNER,
        egui::Stroke::new(1.0_f32, crate::theme::HAIRLINE),
        egui::StrokeKind::Outside,
    );

    // Header band.
    let band = egui::Rect::from_min_size(rect.min, egui::vec2(total_w, header_h));
    p.rect_filled(
        band,
        egui::CornerRadius {
            nw: 2,
            ne: 2,
            sw: 0,
            se: 0,
        },
        crate::theme::PANEL,
    );
    p.hline(rect.left()..=rect.right(), band.bottom(), egui::Stroke::new(1.0_f32, crate::theme::HAIRLINE));

    // Row separators.
    let mut y = band.bottom();
    for (i, (_, rh)) in row_galleys.iter().enumerate() {
        y += rh;
        if i + 1 < row_galleys.len() {
            p.hline(rect.left()..=rect.right(), y, egui::Stroke::new(0.5_f32, crate::theme::HAIRLINE));
        }
    }

    // Paint cells.
    let mut y = rect.min.y;
    paint_row(&p, rect.min.x, y, &widths, &header_galleys, header_h);
    y += header_h;
    for (galleys, rh) in &row_galleys {
        paint_row(&p, rect.min.x, y, &widths, galleys, *rh);
        y += rh;
    }
}

fn paint_row(
    p: &egui::Painter,
    x0: f32,
    y_top: f32,
    widths: &[f32],
    galleys: &[Arc<egui::Galley>],
    row_h: f32,
) {
    let mut x = x0;
    for (ci, g) in galleys.iter().enumerate() {
        let w = widths.get(ci).copied().unwrap_or(0.0);
        let gx = x + CELL_PAD_X;
        let gy = y_top + (row_h - g.size().y) / 2.0;
        p.galley(egui::pos2(gx, gy), g.clone(), crate::theme::TEXT);
        x += w;
    }
}

fn column_aligns(table: &Table, ncols: usize) -> Vec<CellAlign> {
    let mut out = vec![CellAlign::Left; ncols];
    for (i, a) in table.alignments.iter().enumerate() {
        if i < ncols {
            out[i] = *a;
        }
    }
    out
}

fn natural_w(ui: &egui::Ui, font: &egui::FontId, text: &str) -> f32 {
    ui.fonts(|f| f.layout_no_wrap(text.to_string(), font.clone(), crate::theme::TEXT).size().x)
}

fn galley_job(
    ui: &egui::Ui,
    font: &egui::FontId,
    text: &str,
    wrap_w: f32,
    align: CellAlign,
    color: egui::Color32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_w.max(4.0);
    job.wrap.max_rows = usize::MAX;
    job.halign = match align {
        CellAlign::Left => egui::Align::LEFT,
        CellAlign::Center => egui::Align::Center,
        CellAlign::Right => egui::Align::RIGHT,
    };
    job.append(
        text,
        0.0,
        egui::text::TextFormat {
            font_id: font.clone(),
            color,
            ..Default::default()
        },
    );
    ui.fonts(|f| f.layout_job(job))
}

/// Shrink column widths to fit `avail` with a floor, distributing the cut
/// proportionally over columns that can still shrink above the minimum.
fn fit_widths(widths: &mut [f32], avail: f32) {
    for _ in 0..widths.len() {
        let total: f32 = widths.iter().sum();
        if total <= avail {
            break;
        }
        let deficit = total - avail;
        let flex: f32 = widths.iter().map(|x| (x - MIN_COL_W).max(0.0)).sum();
        if flex <= f32::EPSILON {
            break;
        }
        for w in widths.iter_mut() {
            let share = (*w - MIN_COL_W).max(0.0) / flex;
            *w = (*w - deficit * share).max(MIN_COL_W);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_table_is_one_markdown_segment() {
        let segs = split("just text\n\nwith paragraphs");
        assert_eq!(segs.len(), 1);
        match &segs[0] {
            Segment::Markdown(md) => assert!(md.contains("just text")),
            _ => panic!("expected markdown"),
        }
    }

    #[test]
    fn table_is_segmented_with_header_rows_and_alignments() {
        let md = "intro\n\n| A | B |\n|---|--:|\n| 1 | 2 |\n\noutro\n";
        let segs = split(md);
        assert_eq!(segs.len(), 3, "{segs:?}");
        assert!(matches!(&segs[0], Segment::Markdown(s) if s.contains("intro")));
        match &segs[1] {
            Segment::Table(t) => {
                assert_eq!(t.header, vec!["A", "B"]);
                assert_eq!(t.rows, vec![vec!["1", "2"]]);
                assert_eq!(t.alignments, vec![CellAlign::Left, CellAlign::Right]);
            }
            _ => panic!("expected table"),
        }
        assert!(matches!(&segs[2], Segment::Markdown(s) if s.contains("outro")));
    }

    #[test]
    fn inline_markup_in_cells_becomes_plain_text() {
        let md = "| **a** | `b` |\n|---|---|\n| c | d |";
        let segs = split(md);
        match &segs[0] {
            Segment::Table(t) => {
                assert_eq!(t.header, vec!["a", "b"]);
                assert_eq!(t.rows, vec![vec!["c", "d"]]);
            }
            _ => panic!("expected table"),
        }
    }

    #[test]
    fn multiple_tables_split_in_order() {
        let md = "| T1 |\n|---|\n| x |\n\nbetween\n\n| T2 |\n|---|\n| y |\n\nafter";
        let segs = split(md);
        assert_eq!(segs.len(), 4, "{segs:?}");
        assert!(matches!(&segs[0], Segment::Table(_)));
        assert!(matches!(&segs[1], Segment::Markdown(s) if s.contains("between")));
        assert!(matches!(&segs[2], Segment::Table(_)));
        assert!(matches!(&segs[3], Segment::Markdown(s) if s.contains("after")));
    }

    #[test]
    fn segments_cover_the_source_verbatim() {
        let md = "before\n\n| H |\n|---|\n| r |\n\nafter\n";
        let segs = split(md);
        let mut md_text = String::new();
        for s in &segs {
            match s {
                Segment::Markdown(t) => md_text.push_str(t),
                Segment::Table(_) => {}
            }
        }
        assert!(md_text.contains("before"));
        assert!(md_text.contains("after"));
    }

    #[test]
    fn fit_widths_respects_available_and_floor() {
        let mut w = vec![200.0, 200.0, 200.0];
        fit_widths(&mut w, 300.0);
        let total: f32 = w.iter().sum();
        assert!(total <= 300.0 + 1.0, "total {total}");
        for x in &w {
            assert!(*x >= MIN_COL_W, "column below floor");
        }
    }

    #[test]
    fn fit_widths_leaves_widths_unchanged_when_they_fit() {
        let mut w = vec![50.0, 60.0];
        fit_widths(&mut w, 200.0);
        assert_eq!(w, vec![50.0, 60.0]);
    }
}
