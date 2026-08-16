//! Design tokens (app-flow-plan chunk 0 + app-parity-spec §1): the visual
//! language the mocks pin down — a FLAT near-black canvas, 1px hairlines
//! everywhere, a single red accent reserved for the wordmark + destructive
//! actions, light sans typography over dim text-greys, and monospace for
//! config/log content. Everything else in the app reads from these tokens;
//! the ONLY place styles are built (app-parity-spec §1c).
//!
//! Foundations (chunk 1 of the parity pass): the Inter-Light proportional
//! font and the Phosphor icon font are registered here; every helper a view
//! needs (headers, house/destructive/icon buttons, fields, hairlines) lives
//! here. No hex literals anywhere else.

use egui::{Color32, FontId, TextStyle, Ui, Visuals};

// --- palette ---
/// Near-black window / canvas background (`#0e0f12`) — the ONLY colour that
/// fills a panel; flat design means panels tile flush on it.
pub const BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x12);
/// One step lighter than BG — widget surfaces (buttons, inputs, dropdown
/// fields, editor frames).
pub const PANEL: Color32 = Color32::from_rgb(0x18, 0x19, 0x1e);
/// The single red accent: wordmark, destructive fills, finding-red.
pub const ACCENT: Color32 = Color32::from_rgb(0xe5, 0x44, 0x2c);
/// Status: live / ready (env dot, ready badges).
pub const OK: Color32 = Color32::from_rgb(0x78, 0xc8, 0x78);
/// Status: down / error (env dot when a probe fails).
pub const DANGER: Color32 = Color32::from_rgb(0xe5, 0x44, 0x2c);
/// Status: warn — pickable but degraded (a branch rev served from a
/// stale rev cache).
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x3c);

// --- type greys ---
/// Primary light text.
pub const TEXT: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xd8);
/// Secondary / body text, icons.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8f, 0x90, 0x99);
/// De-emphasised / captions / created stamps / hints.
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5c, 0x5d, 0x66);

/// The 1px hairline border between chrome and canvas (`#2a2b33`).
pub const HAIRLINE: Color32 = Color32::from_rgb(0x2a, 0x2b, 0x33);

// --- helper shades (added in the parity pass, spec §0) ---
/// Selected sidebar row fill (`#1e1f24`).
pub const ROW_HL: Color32 = Color32::from_rgb(0x1e, 0x1f, 0x24);
/// Hovered sidebar row fill (`#16171b`) — the pointer feedback band.
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x16, 0x17, 0x1b);
/// JSON editor frame fill (`#101114` — a hair lighter than BG).
pub const EDITOR_BG: Color32 = Color32::from_rgb(0x10, 0x11, 0x14);
/// The corpus stack graphic's front-plate fill (`#191a1f`).
pub const PLATE_FRONT: Color32 = Color32::from_rgb(0x19, 0x1a, 0x1f);
/// Category segment colors for the corpus visual (hypotheses, techniques,
/// findings, attacks, runs, other) — muted, distinct on the dark panels.
pub const CORPUS_PALETTE: [Color32; 6] = [
    Color32::from_rgb(0x4a, 0x6e, 0x8f), // slate blue
    Color32::from_rgb(0x6f, 0x8f, 0x5a), // moss
    Color32::from_rgb(0xe5, 0x44, 0x2c), // corpus red (findings)
    Color32::from_rgb(0x8f, 0x6f, 0x4a), // amber-brown
    Color32::from_rgb(0x5c, 0x5d, 0x66), // faint grey (runs)
    Color32::from_rgb(0x3a, 0x3b, 0x44), // plate grey (other)
];
/// The house-button resting fill (`#1c1d22`).
const HOUSE_FILL: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x22);
/// The house-button hover fill (`#232429`).
const HOUSE_HOVER: Color32 = Color32::from_rgb(0x23, 0x24, 0x29);
/// Destructive-button label (`#1c0b06` — dark red-brown, per the mock).
const ON_ACCENT: Color32 = Color32::from_rgb(0x1c, 0x0b, 0x06);

/// Convert a raw (r, g, b) triple into a `Color32` — the sanctioned dynamic
/// colour builder for syntect-adjacent conversions (keeps `from_rgb`
/// literals out of view code).
pub fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

// --- spacing (px) ---
/// Half of the sidebar panel's inner margin: how far the sidebar content
/// sits inset from the panel's edge. Shared by `main.rs`'s sidebar frame
/// (which insets the content) and `sidebar.rs`'s `row_ui`/`section_header`
/// (which expand their fill/hairline back out to the panel edges).
pub const PANEL_MARGIN: f32 = 10.0;
/// Horizontal gap between sibling controls.
pub const SPACING: f32 = 8.0;
/// Vertical gap between stacked controls / rows.
pub const PANEL_GAP: f32 = 6.0;

/// A proportional font id at `size` (family index 0 = Inter-Light).
pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

/// The monospace font id at `size` — config + log content.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

// --- text helpers (spec §1c) ---
/// The mock's large screen header: 28px, Inter-Light, TEXT.
pub fn screen_header(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).size(28.0).color(TEXT)
}

/// The mock's section heading: 20px, Inter-Light, TEXT.
pub fn section_heading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).size(20.0).color(TEXT)
}

// --- control helpers (spec §1c) ---
/// A house button: fill #1c1d22, 1px HAIRLINE stroke, radius 2, padding
/// (12,7), 14px TEXT. Hover fill #232429.
pub fn house_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, HOUSE_FILL, ButtonKind::House);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        ui.button(egui::RichText::new(text.into()).size(14.0).color(TEXT))
    })
    .inner
}

/// A destructive button: fill ACCENT, no stroke, radius 2, dark-on-red
/// 14px text. Hover ~10% lighter red.
pub fn destructive_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, ACCENT, ButtonKind::Destructive);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        ui.button(egui::RichText::new(text.into()).size(14.0).color(ON_ACCENT))
    })
    .inner
}

/// A flat frameless icon button: the glyph in TEXT_MUTED, TEXT on hover.
pub fn icon_button(ui: &mut Ui, icon: &str, size: f32) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        for ws in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            ws.bg_fill = Color32::TRANSPARENT;
            ws.bg_stroke = egui::Stroke::NONE;
        }
        style.visuals.widgets.inactive.fg_stroke.color = TEXT_MUTED;
        style.visuals.widgets.hovered.fg_stroke.color = TEXT;
        style.visuals.widgets.active.fg_stroke.color = TEXT;
        style.spacing.button_padding = egui::vec2(2.0, 2.0);
        ui.add(
            egui::Button::new(
                // Pinned to the phosphor family; no explicit colour so the
                // glyph follows fg_stroke (muted → text on hover).
                egui::WidgetText::from(
                    egui::RichText::new(icon)
                        .family(egui::FontFamily::Name("phosphor".into()))
                        .size(size),
                ),
            )
            .frame(false),
        )
    })
    .inner
}

/// A phosphor icon glyph as a `RichText`, pinned to the dedicated
/// "phosphor" family (phosphor-first, Inter as glyph fallback). egui's
/// font-injection would otherwise let Inter-Light claim the icon's PUA
/// codepoint first and render a stray glyph — the family pin makes the
/// icon win deterministically. Size + colour are explicit here.
pub fn icon_text(icon: &str, size: f32, color: Color32) -> egui::RichText {
    egui::RichText::new(icon)
        .family(egui::FontFamily::Name("phosphor".into()))
        .size(size)
        .color(color)
}

/// A flat field frame for dropdown-like fields: PANEL fill, 1px HAIRLINE,
/// radius 2, inner margin (8,4). (Non-ComboBox fields; a ComboBox must be
/// restyled via `combo_field`'s scoped widget-visuals — a wrapping Frame
/// cannot reach its internal button, spec §9.)
#[allow(dead_code)]
pub fn flat_field_frame() -> egui::Frame {
    egui::Frame::default()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, HAIRLINE))
        .corner_radius(egui::CornerRadius::same(2))
        .inner_margin(egui::Margin::symmetric(8, 4))
}

/// A full-width 1px HAIRLINE hairline rule.
pub fn hairline(ui: &mut Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().line_segment(
        [egui::pos2(rect.min.x, rect.center().y), egui::pos2(rect.max.x, rect.center().y)],
        egui::Stroke::new(1.0_f32, HAIRLINE),
    );
}

/// Run `inner` inside a scope whose widget visuals restyle the ComboBox's
/// internal button into a flat PANEL field — 1px HAIRLINE, radius 2,
/// TEXT text (spec §3). A wrapping Frame cannot reach a ComboBox's button,
/// so every flat dropdown wraps itself in this.
pub fn combo_field<R>(ui: &mut Ui, inner: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let style = ui.style_mut();
        for ws in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            ws.bg_fill = PANEL;
            ws.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
            ws.corner_radius = egui::CornerRadius::same(2);
            ws.fg_stroke.color = TEXT;
        }
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        inner(ui)
    })
    .inner
}

/// Paint the phosphor `caret_down` arrow for a flat ComboBox (spec §1a/§3).
pub fn combo_caret(
    ui: &egui::Ui,
    rect: egui::Rect,
    visuals: &egui::style::WidgetVisuals,
    _open: bool,
    _above: egui::AboveOrBelow,
) {
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::CARET_DOWN,
        egui::FontId::new(13.0, egui::FontFamily::Name("phosphor".into())),
        visuals.fg_stroke.color,
    );
}

/// How a button restyles: hover-state fills (house vs destructive).
enum ButtonKind {
    House,
    Destructive,
}

/// Override the button widget visuals (inactive/hovered/active) for a
/// house- or destructive-styled button, using the theme's HAIRLINE stroke.
fn override_button_visuals(style: &mut egui::Style, resting: Color32, kind: ButtonKind) {
    let hover_fill = match kind {
        ButtonKind::House => HOUSE_HOVER,
        // ~10% lighter red on hover (mix ACCENT toward white).
        ButtonKind::Destructive => Color32::from_rgb(0xed, 0x59, 0x41),
    };
    let radius = egui::CornerRadius::same(2);
    for ws in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        ws.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
        ws.corner_radius = radius;
    }
    style.visuals.widgets.inactive.bg_fill = resting;
    style.visuals.widgets.hovered.bg_fill = hover_fill;
    style.visuals.widgets.active.bg_fill = hover_fill;
}

/// Apply the visual language to the egui context. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    // --- fonts (spec §1a + §1b): Inter-Light first in the proportional
    // family (defaults remain as fallback), Phosphor registered as its own
    // family (egui falls through to it for icon glyphs).
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter-light".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/Inter-Light.otf")).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "inter-light".to_owned());
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Pin icon glyphs to an explicit phosphor-first family: egui_phosphor's
    // own injection puts the phosphor font at Proportional index 1, behind
    // Inter-Light at index 0, so Inter claims the icon.PUA codepoints first
    // and renders stray glyphs (the mangled `⋮`). A dedicated family whose
    // font list is phosphor-first resolves the glyph deterministically.
    fonts.families.insert(
        egui::FontFamily::Name("phosphor".into()),
        vec!["phosphor".to_owned(), "inter-light".to_owned()],
    );
    ctx.set_fonts(fonts);

    let mut visuals = Visuals::dark();
    // Flat: window + every panel fill BG (spec §0, §2).
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.faint_bg_color = BG;
    visuals.widgets.noninteractive.bg_fill = BG;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT_MUTED);
    // Widget surfaces are one step lighter than the canvas.
    visuals.widgets.inactive.bg_fill = PANEL;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(2);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x22, 0x23, 0x2a);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(2);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x26, 0x27, 0x30);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(2);
    visuals.selection.bg_fill = ACCENT;
    visuals.override_text_color = Some(TEXT);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(SPACING, PANEL_GAP);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.text_styles.insert(TextStyle::Body, font(15.0));
    style.text_styles.insert(TextStyle::Button, font(14.0));
    style.text_styles.insert(TextStyle::Heading, font(24.0));
    style.text_styles.insert(TextStyle::Monospace, mono(13.5));
    style.text_styles.insert(TextStyle::Small, font(12.0));
    ctx.set_style(style);
}