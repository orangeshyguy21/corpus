//! JSON syntax highlighting (app-flow chunk 4): a syntect-backed layouter
//! for the agent editor's `TextEdit`, mapping token scopes onto the
//! theme's token colours (strings green, numbers orange, property keys
//! teal, booleans/null purple, comments/punctuation faint). The degrade
//! arm — a plain monospace `TextEdit` with no layouter — is still the view's
//! fallback if syntect were to fight egui; the editor picks it up by not
//! attaching a layouter.

use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};

use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, ScopeSelectors, Style, StyleModifier, Theme, ThemeItem};
use syntect::parsing::{SyntaxReference, SyntaxSet};

use crate::theme;

/// The `TextEdit::layouter` closure body: build the highlighted `Galley`.
pub fn layouter(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
) -> Arc<egui::text::Galley> {
    let mut job = LayoutJob::default();
    if wrap_width.is_finite() && wrap_width > 0.0 {
        job.wrap.max_width = wrap_width;
    }
    let mut highlighter = HighlightLines::new(json_syntax(), json_theme());
    for line in text.split('\n') {
        match highlighter.highlight_line(line, syntax_set()) {
            Ok(ranges) => {
                for (style, piece) in ranges {
                    job.append(piece, 0.0, fmt(color_of(style)));
                }
            }
            Err(_) => {
                job.append(line, 0.0, fmt(theme::TEXT));
            }
        }
        job.append("\n", 0.0, fmt(theme::TEXT));
    }
    ui.fonts(|fonts| fonts.layout_job(job))
}

/// The monospace base text format for a token colour.
fn fmt(color: Color32) -> TextFormat {
    TextFormat {
        font_id: FontId::monospace(13.5),
        color,
        ..Default::default()
    }
}

/// syntect `Style` → theme `Color32` (its foreground is already resolved
/// from the theme's scope rules).
fn color_of(style: Style) -> Color32 {
    let f = style.foreground;
    theme::rgb(f.r, f.g, f.b)
}

/// The bundled syntax set (JSON + everything else; loaded once).
fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// The JSON syntax reference, falling back to plain text if absent.
fn json_syntax() -> &'static SyntaxReference {
    let ss = syntax_set();
    ss.find_syntax_by_extension("json")
        .unwrap_or_else(|| ss.find_syntax_plain_text())
}

/// A theme whose rules map JSON token scopes onto the corpus palette.
fn json_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(build_theme)
}

fn build_theme() -> Theme {
    fn item(sel: &str, rgb: (u8, u8, u8)) -> ThemeItem {
        ThemeItem {
            scope: ScopeSelectors::from_str(sel).unwrap_or_default(),
            style: StyleModifier {
                foreground: Some(Color {
                    r: rgb.0,
                    g: rgb.1,
                    b: rgb.2,
                    a: 0xff,
                }),
                ..Default::default()
            },
        }
    }
    let faint = (
        theme::TEXT_MUTED.r(),
        theme::TEXT_MUTED.g(),
        theme::TEXT_MUTED.b(),
    );
    Theme {
        name: Some("corpus-json".into()),
        author: None,
        settings: Default::default(),
        scopes: vec![
            // One-Dark-ish palette (app-parity-spec §6): keys #aab2bf, string
            // values #98c379, numbers #d19a66, booleans/null #d19a66,
            // punctuation TEXT_MUTED. Base first; specific rules win below.
            item("source.json", (theme::TEXT.r(), theme::TEXT.g(), theme::TEXT.b())),
            item("support.type.property-name", (0xaa, 0xb2, 0xbf)),
            item("string", (0x98, 0xc3, 0x79)),
            item("constant.numeric", (0xd1, 0x9a, 0x66)),
            item("constant.language", (0xd1, 0x9a, 0x66)),
            item("comment", faint),
            item("punctuation", faint),
        ],
    }
}
