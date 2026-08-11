//! The Corpus view: a grouped tree of the store + a markdown viewer.
//!
//! Left: collapsing sections per category (hypotheses / techniques /
//! findings / attacks / runs), newest-first where timestamps apply.
//! Right: `egui_commonmark` rendering of the selected entry's body.
//! Selection is by PATH, not index — it survives the 1 Hz rescan.

use std::path::PathBuf;

use egui::{Color32, RichText, ScrollArea, Ui};

use crate::store::{format_epoch, strip_ansi, Category, Store};

/// How much of a run log to render (they can be megabytes).
const RUN_TAIL_LINES: usize = 2000;

/// Selection state this view owns.
#[derive(Debug, Default)]
pub struct View {
    /// Selected entry's path (stable across store rescans).
    selected: Option<PathBuf>,
    /// Markdown cache — re-parsing every frame is the classic egui
    /// perf bug; keep one cache and reuse it.
    md_cache: egui_commonmark::CommonMarkCache,
}

impl View {
    /// The tree, rendered into its own left panel (from the app shell).
    /// Open state lives in egui's collapsing-header memory (keyed by
    /// id_salt).
    pub fn show_tree(&mut self, ui: &mut Ui, store: &Store) {
        ui.add_space(8.0);
        ui.weak("store");
        ui.add_space(4.0);
        ScrollArea::vertical()
            .id_salt("corpus-tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for cat in Category::ALL {
                    let count = store.of(cat).count();
                    let header = format!("{}  ·  {count}", cat.label());
                    egui::CollapsingHeader::new(RichText::new(header).strong())
                        .id_salt(cat.dir_name())
                        .default_open(true)
                        .show(ui, |ui| {
                            for entry in store.of(cat) {
                                let selected = self.selected.as_ref() == Some(&entry.path);
                                let mut text = RichText::new(entry.title());
                                if selected {
                                    text = text.strong();
                                }
                                let row = ui
                                    .horizontal(|ui| {
                                        let row = ui.selectable_label(selected, text);
                                        // Findings carry severity in the tree.
                                        if cat == Category::Findings {
                                            if let Some(severity) = entry.meta("severity") {
                                                ui.label(severity_badge(severity));
                                            }
                                        }
                                        row
                                    })
                                    .inner;
                                if row.clicked() {
                                    self.selected = Some(entry.path.clone());
                                }
                            }
                            if count == 0 {
                                ui.weak("(empty)");
                            }
                        });
                }
            });
    }

    /// The preview, rendered into the central panel.
    pub fn show_preview(&mut self, ui: &mut Ui, store: &Store) {
        let Some(path) = self.selected.clone() else {
            ui.centered_and_justified(|ui| {
                ui.weak("select an entry on the left");
            });
            return;
        };
        let Some(entry) = store
            .entries
            .iter()
            .find(|e| e.path == path)
            .cloned()
        else {
            // The entry vanished in a rescan — drop the selection.
            self.selected = None;
            return;
        };

        // Title row: title + category chip + severity + date.
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.heading(entry.title());
            ui.weak(format!("· {}", entry.category.label()));
            if entry.category == Category::Findings {
                if let Some(severity) = entry.meta("severity") {
                    ui.label(severity_badge(severity));
                }
            }
            if let Some(ts) = entry.timestamp() {
                ui.weak(format!("· {}", format_epoch(ts)));
            }
        });
        ui.label(
            RichText::new(entry.path.display().to_string())
                .small()
                .weak(),
        )
        .on_hover_text("store path");
        ui.add_space(2.0);
        ui.separator();

        let body = if entry.category == Category::Runs {
            let clean = strip_ansi(&entry.body);
            let lines: Vec<&str> = clean.lines().collect();
            if lines.len() > RUN_TAIL_LINES {
                format!(
                    "[… {} earlier lines elided …]\n\n{}",
                    lines.len() - RUN_TAIL_LINES,
                    lines[lines.len() - RUN_TAIL_LINES..].join("\n")
                )
            } else {
                clean
            }
        } else {
            entry.body.clone()
        };

        ScrollArea::vertical()
            .id_salt("corpus-preview")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if entry.category == Category::Runs {
                    ui.monospace(body);
                } else {
                    egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.md_cache, &body);
                }
            });
    }
}

/// A colored severity badge for findings.
fn severity_badge(severity: &str) -> RichText {
    let color = match severity {
        "critical" => Color32::from_rgb(255, 70, 70),
        "high" => Color32::from_rgb(255, 140, 60),
        "medium" => Color32::from_rgb(255, 200, 90),
        "low" => Color32::from_rgb(150, 200, 120),
        _ => Color32::GRAY,
    };
    RichText::new(format!("[{severity}]")).color(color).strong()
}
