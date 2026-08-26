//! Finding discovery projection and compact severity summary for project views.

use corpus_core::FindingSeverity;
use egui::{RichText, Ui};

use crate::state::FindingDiscovery;
use crate::theme;
use crate::views::components;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FindingCounts {
    critical: usize,
    high: usize,
    medium: usize,
    low: usize,
    unrated: usize,
}

impl FindingCounts {
    fn from_cards(cards: &[corpus_core::FindingCard]) -> Self {
        let mut counts = Self::default();
        for card in cards {
            match card.severity {
                Some(FindingSeverity::Critical) => counts.critical += 1,
                Some(FindingSeverity::High) => counts.high += 1,
                Some(FindingSeverity::Medium) => counts.medium += 1,
                Some(FindingSeverity::Low) => counts.low += 1,
                None => counts.unrated += 1,
            }
        }
        counts
    }

    fn total(self) -> usize {
        self.critical + self.high + self.medium + self.low + self.unrated
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FindingSummaryState {
    Loading,
    Ready(FindingCounts),
    Failed {
        message: String,
        last_good: FindingCounts,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FindingSummary(FindingSummaryState);

impl FindingSummary {
    pub(super) fn from_discovery(discovery: &FindingDiscovery) -> Self {
        Self(match discovery {
            FindingDiscovery::Loading => FindingSummaryState::Loading,
            FindingDiscovery::Ready(cards) => {
                FindingSummaryState::Ready(FindingCounts::from_cards(cards))
            }
            FindingDiscovery::Failed { message, last_good } => FindingSummaryState::Failed {
                message: message.clone(),
                last_good: FindingCounts::from_cards(last_good),
            },
        })
    }

    pub(super) fn is_visible(&self) -> bool {
        !matches!(&self.0, FindingSummaryState::Ready(counts) if counts.total() == 0)
    }

    pub(super) fn show(&self, ui: &mut Ui) {
        let counts = match &self.0 {
            FindingSummaryState::Loading => {
                empty_hint(ui, "loading findings…");
                return;
            }
            FindingSummaryState::Ready(counts) => counts,
            FindingSummaryState::Failed { message, last_good } => {
                components::status_badge(ui, "refresh failed", components::StatusTone::Danger)
                    .on_hover_text(message);
                ui.add_space(8.0);
                last_good
            }
        };
        let width = tile_width(ui.available_width());
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(theme::CARD_GUTTER, theme::CARD_GUTTER);
            ui.horizontal_wrapped(|ui| {
                for (label, count, color) in count_entries(*counts) {
                    if count > 0 {
                        count_tile(ui, width, label, count, color);
                    }
                }
            });
        });
    }
}

fn count_entries(counts: FindingCounts) -> [(&'static str, usize, egui::Color32); 5] {
    [
        ("CRITICAL", counts.critical, theme::FINDING_CRITICAL),
        ("HIGH", counts.high, theme::FINDING_HIGH),
        ("MEDIUM", counts.medium, theme::FINDING_MEDIUM),
        ("LOW", counts.low, theme::FINDING_LOW),
        ("UNRATED", counts.unrated, theme::FINDING_UNRATED),
    ]
}

fn tile_width(available: f32) -> f32 {
    if available >= 480.0 {
        ((available - 32.0) / 5.0).max(72.0)
    } else if available >= 240.0 {
        ((available - 8.0) / 2.0).max(96.0)
    } else {
        available.max(96.0)
    }
}

fn count_tile(ui: &mut Ui, width: f32, label: &str, count: usize, color: egui::Color32) {
    egui::Frame::default()
        .fill(color.gamma_multiply(0.08))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.90)))
        .corner_radius(egui::CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width((width - 20.0).max(52.0));
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new(label)
                        .size(10.5)
                        .monospace()
                        .strong()
                        .color(color),
                );
                ui.add_space(3.0);
                ui.label(
                    RichText::new(count.to_string())
                        .size(24.0)
                        .monospace()
                        .strong()
                        .color(color),
                );
            });
        });
}

fn empty_hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).italics().color(theme::TEXT_FAINT));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding_card(severity: Option<FindingSeverity>) -> corpus_core::FindingCard {
        corpus_core::FindingCard {
            path: std::path::PathBuf::from("findings/f.md"),
            title: "Finding".into(),
            title_source: corpus_core::FindingTitleSource::Title,
            severity,
            timestamp: None,
            time_source: None,
            reference: "F-1".into(),
            reference_source: corpus_core::FindingReferenceSource::Id,
            status: None,
            oracle_verified: None,
            sensitivity: None,
            warnings: Vec::new(),
        }
    }

    #[test]
    fn preserves_loading_failure_and_empty_counts() {
        assert_eq!(
            FindingSummary::from_discovery(&FindingDiscovery::Loading),
            FindingSummary(FindingSummaryState::Loading)
        );
        assert_eq!(
            FindingSummary::from_discovery(&FindingDiscovery::Ready(Vec::new())),
            FindingSummary(FindingSummaryState::Ready(FindingCounts::default()))
        );
        let failed = FindingDiscovery::Failed {
            message: "watch failed".into(),
            last_good: vec![finding_card(Some(FindingSeverity::High))],
        };
        match FindingSummary::from_discovery(&failed).0 {
            FindingSummaryState::Failed { message, last_good } => {
                assert_eq!(message, "watch failed");
                assert_eq!(last_good.high, 1);
            }
            other => panic!("expected failed model, got {other:?}"),
        }
    }

    #[test]
    fn counts_every_severity_and_keeps_unrated_visible() {
        let cards = vec![
            finding_card(Some(FindingSeverity::Critical)),
            finding_card(Some(FindingSeverity::High)),
            finding_card(Some(FindingSeverity::High)),
            finding_card(Some(FindingSeverity::Medium)),
            finding_card(Some(FindingSeverity::Low)),
            finding_card(None),
            finding_card(None),
        ];
        let FindingSummaryState::Ready(counts) =
            FindingSummary::from_discovery(&FindingDiscovery::Ready(cards)).0
        else {
            panic!("expected ready counts")
        };
        assert_eq!(
            counts,
            FindingCounts {
                critical: 1,
                high: 2,
                medium: 1,
                low: 1,
                unrated: 2,
            }
        );
    }

    #[test]
    fn empty_summary_is_hidden_and_zero_severity_boxes_are_omitted() {
        assert!(!FindingSummary(FindingSummaryState::Ready(FindingCounts::default())).is_visible());
        assert!(FindingSummary(FindingSummaryState::Loading).is_visible());
        assert!(FindingSummary(FindingSummaryState::Failed {
            message: "unknown".into(),
            last_good: FindingCounts::default(),
        })
        .is_visible());

        let counts = FindingCounts {
            critical: 2,
            low: 1,
            ..FindingCounts::default()
        };
        let visible = count_entries(counts)
            .into_iter()
            .filter_map(|(label, count, _)| (count > 0).then_some((label, count)))
            .collect::<Vec<_>>();
        assert_eq!(visible, [("CRITICAL", 2), ("LOW", 1)]);
    }

    #[test]
    fn tiles_wrap_without_becoming_tiny() {
        assert!(tile_width(900.0) >= 160.0);
        assert!(tile_width(479.0) >= 96.0);
        assert_eq!(tile_width(200.0), 200.0);
    }
}
