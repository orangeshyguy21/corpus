//! Editable JSON and Markdown syntax highlighting for egui `TextEdit`s.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::str::FromStr;
use std::sync::{Arc, Mutex, OnceLock};

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Color, ScopeSelectors, Style, StyleModifier, Theme, ThemeItem};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use crate::theme;

const CACHE_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum SyntaxKind {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct CacheKey {
    syntax: SyntaxKind,
    source_hash: u64,
}

struct CachedJob {
    source: String,
    job: LayoutJob,
}

pub fn json_layouter(ui: &egui::Ui, text: &str, wrap_width: f32) -> Arc<egui::text::Galley> {
    layouter(ui, text, wrap_width, SyntaxKind::Json)
}

pub fn markdown_layouter(ui: &egui::Ui, text: &str, wrap_width: f32) -> Arc<egui::text::Galley> {
    layouter(ui, text, wrap_width, SyntaxKind::Markdown)
}

fn layouter(
    ui: &egui::Ui,
    text: &str,
    wrap_width: f32,
    syntax: SyntaxKind,
) -> Arc<egui::text::Galley> {
    let mut job = cached_job(text, syntax);
    if wrap_width.is_finite() && wrap_width > 0.0 {
        job.wrap.max_width = wrap_width;
    }
    ui.fonts(|fonts| fonts.layout_job(job))
}

fn cached_job(text: &str, syntax: SyntaxKind) -> LayoutJob {
    let key = CacheKey {
        syntax,
        source_hash: source_hash(text),
    };
    let cache = highlight_cache();
    if let Some(job) = cache
        .lock()
        .expect("syntax highlight cache poisoned")
        .get(&key)
        .filter(|entry| entry.source == text)
        .map(|entry| entry.job.clone())
    {
        return job;
    }

    let job = highlight(text, syntax);
    let mut cache = cache.lock().expect("syntax highlight cache poisoned");
    if cache.len() >= CACHE_CAPACITY {
        cache.clear();
    }
    cache.insert(
        key,
        CachedJob {
            source: text.to_owned(),
            job: job.clone(),
        },
    );
    job
}

fn source_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn highlight_cache() -> &'static Mutex<HashMap<CacheKey, CachedJob>> {
    static CACHE: OnceLock<Mutex<HashMap<CacheKey, CachedJob>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn highlight(text: &str, syntax: SyntaxKind) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut highlighter = HighlightLines::new(syntax_ref(syntax), syntax_theme(syntax));

    for line in LinesWithEndings::from(text) {
        match highlighter.highlight_line(line, syntax_set()) {
            Ok(ranges) => {
                for (style, piece) in ranges {
                    job.append(piece, 0.0, text_format(color_of(style)));
                }
            }
            Err(_) => job.append(line, 0.0, text_format(theme::TEXT)),
        }
    }
    job
}

fn text_format(color: Color32) -> TextFormat {
    TextFormat {
        font_id: FontId::monospace(13.0),
        color,
        ..Default::default()
    }
}

fn color_of(style: Style) -> Color32 {
    let foreground = style.foreground;
    theme::rgb(foreground.r, foreground.g, foreground.b)
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn syntax_ref(syntax: SyntaxKind) -> &'static SyntaxReference {
    let extension = match syntax {
        SyntaxKind::Json => "json",
        SyntaxKind::Markdown => "md",
    };
    syntax_set()
        .find_syntax_by_extension(extension)
        .unwrap_or_else(|| syntax_set().find_syntax_plain_text())
}

fn syntax_theme(syntax: SyntaxKind) -> &'static Theme {
    match syntax {
        SyntaxKind::Json => json_theme(),
        SyntaxKind::Markdown => markdown_theme(),
    }
}

fn json_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme {
        name: Some("corpus-json".into()),
        author: None,
        settings: Default::default(),
        scopes: vec![
            theme_item("source.json", theme::TEXT),
            theme_item("support.type.property-name", theme::rgb(0xaa, 0xb2, 0xbf)),
            theme_item("string", theme::rgb(0x98, 0xc3, 0x79)),
            theme_item("constant.numeric", theme::rgb(0xd1, 0x9a, 0x66)),
            theme_item("constant.language", theme::rgb(0xd1, 0x9a, 0x66)),
            theme_item("comment", theme::TEXT_MUTED),
            theme_item("punctuation", theme::TEXT_MUTED),
        ],
    })
}

fn markdown_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| Theme {
        name: Some("corpus-markdown".into()),
        author: None,
        settings: Default::default(),
        scopes: vec![
            theme_item("text.html.markdown", theme::PROSE),
            theme_item("markup.heading", theme::SIGNAL_RED),
            theme_item("punctuation.definition.heading.markdown", theme::HEALTHY),
            theme_item("punctuation.definition.list_item.markdown", theme::HEALTHY),
            theme_item("markup.bold.markdown", theme::TEXT),
            theme_item("markup.italic.markdown", theme::TEXT_MUTED),
            theme_item("markup.raw.inline.markdown", theme::INTERACTION),
            theme_item("markup.raw.block.markdown", theme::TEXT_MUTED),
            theme_item("punctuation.definition.raw.markdown", theme::TEXT_FAINT),
            theme_item("markup.underline.link.markdown", theme::CORPUS_PALETTE[0]),
        ],
    })
}

fn theme_item(selector: &str, color: Color32) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(selector).unwrap_or_default(),
        style: StyleModifier {
            foreground: Some(Color {
                r: color.r(),
                g: color.g(),
                b: color.b(),
                a: 0xff,
            }),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_syntax_set_contains_markdown() {
        assert_eq!(syntax_ref(SyntaxKind::Markdown).name, "Markdown");
    }

    #[test]
    fn markdown_highlighting_preserves_source_and_uses_palette() {
        let source = "# Heading\n\n- first\n\nPlain `code`.";
        let job = highlight(source, SyntaxKind::Markdown);
        let colors: Vec<_> = job
            .sections
            .iter()
            .map(|section| section.format.color)
            .collect();

        assert_eq!(job.text, source);
        assert!(colors.contains(&theme::HEALTHY));
        assert!(colors.contains(&theme::SIGNAL_RED), "colors: {colors:?}");
        assert!(colors.contains(&theme::PROSE));
    }

    #[test]
    fn cache_key_is_syntax_specific() {
        let source = "# heading";
        let markdown = cached_job(source, SyntaxKind::Markdown);
        let json = cached_job(source, SyntaxKind::Json);
        let markdown_colors: Vec<_> = markdown
            .sections
            .iter()
            .map(|section| section.format.color)
            .collect();
        let json_colors: Vec<_> = json
            .sections
            .iter()
            .map(|section| section.format.color)
            .collect();

        assert_ne!(markdown_colors, json_colors);
    }
}
