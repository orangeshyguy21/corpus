//! Design tokens for corpus's command-centre visual language: a near-black
//! technical canvas, smoked surfaces, amber interaction/focus, acid green
//! health, and signal red reserved for danger and denial. Everything else in
//! the app reads from these tokens; views do not invent colour literals.
//!
//! Oswald display faces, Inter reading text, JetBrains Mono data text, and
//! Phosphor icons are registered here. View-specific composition lives in
//! `views/components`; views consume named tokens instead of inventing color
//! literals. Noto Serif SC supplies the card-header annotations.

use egui::{Color32, FontId, TextStyle, Ui, Visuals};

// --- palette ---
/// Near-black window / canvas background.
pub const BG: Color32 = Color32::from_rgb(0x09, 0x0b, 0x0d);
/// Smoked widget/card surface. Cards may apply alpha at paint time; the shared
/// widget default stays opaque so text-field compositing is deterministic.
pub const PANEL: Color32 = Color32::from_rgb(0x17, 0x18, 0x18);
/// Translucent fill shared by the primary Project/Agent cards. Keeping the
/// alpha here makes the amount of canvas showing through consistent.
pub fn card_fill() -> Color32 {
    Color32::from_rgba_unmultiplied(PANEL.r(), PANEL.g(), PANEL.b(), 0x70)
}
/// Quiet neutral outline for primary cards. Interaction keylines stay amber.
pub const CARD_BORDER: Color32 = Color32::from_rgb(0x3a, 0x3c, 0x3e);
/// A slightly stronger smoked surface for raised cards and menus.
#[allow(dead_code)] // consumed by the Project/Agent card migration in chunks 2-3
pub const PANEL_RAISED: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x1b);
/// Amber owns selection, focus, navigation, and primary action emphasis.
pub const INTERACTION: Color32 = Color32::from_rgb(0xe3, 0x9a, 0x3b);
pub const INTERACTION_HOVER: Color32 = Color32::from_rgb(0xf0, 0xaf, 0x55);
/// Signal red is reserved for destructive actions, failures, and denials.
pub const SIGNAL_RED: Color32 = Color32::from_rgb(0xe5, 0x44, 0x2c);
/// Acid green is reserved for healthy/live/allowed state.
pub const HEALTHY: Color32 = Color32::from_rgb(0x91, 0xcf, 0x58);
/// Status: warn — pickable but degraded (a branch rev served from a
/// stale rev cache).
pub const WARN: Color32 = Color32::from_rgb(0xf0, 0xb3, 0x46);

/// Finding urgency ramp. These deliberately read louder than ordinary status
/// chrome: the corpus summary is a compact alarm panel, ordered most urgent to
/// least urgent. Text labels keep color from carrying meaning alone.
pub const FINDING_CRITICAL: Color32 = SIGNAL_RED;
pub const FINDING_HIGH: Color32 = Color32::from_rgb(0xf0, 0x6a, 0x32);
pub const FINDING_MEDIUM: Color32 = WARN;
pub const FINDING_LOW: Color32 = HEALTHY;
pub const FINDING_UNRATED: Color32 = TEXT_MUTED;

// --- type greys ---
/// Primary labels, data, and IDs.
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe2, 0xda);
/// Reading prose — a touch quieter than labels and display text.
pub const PROSE: Color32 = Color32::from_rgb(0xd8, 0xd4, 0xcb);
/// Secondary / body text, icons.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x9b, 0x98, 0x90);
/// De-emphasised / captions / created stamps / hints.
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x7d, 0x7a, 0x72);
/// Reserved for the later 10–11px Noto Serif SC annotation layer.
pub const SC_ANNOTATION: Color32 = Color32::from_rgb(0x56, 0x53, 0x49);

/// Keyline hierarchy: soft neutral structure and amber-emphasis borders.
pub const KEYLINE_SOFT: Color32 = Color32::from_rgb(0x32, 0x30, 0x2b);
pub const KEYLINE: Color32 = Color32::from_rgb(0x6a, 0x47, 0x22);
pub const KEYLINE_STRONG: Color32 = Color32::from_rgb(0xa8, 0x6d, 0x2d);
pub const HAIRLINE: Color32 = KEYLINE_SOFT;
/// Static canvas-grid paint; deliberately quiet enough to sit behind text.
pub const GRID_LINE: Color32 = Color32::from_rgb(0x10, 0x10, 0x0f);
pub const GRID_MARK: Color32 = Color32::from_rgb(0x17, 0x15, 0x11);

// --- helper shades (added in the parity pass, spec §0) ---
/// Selected sidebar row fill (`#1e1f24`).
pub const ROW_HL: Color32 = Color32::from_rgb(0x25, 0x1e, 0x13);
/// Hovered sidebar row fill (`#16171b`) — the pointer feedback band.
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x18, 0x15, 0x10);
/// JSON editor frame fill (`#101114` — a hair lighter than BG).
pub const EDITOR_BG: Color32 = Color32::from_rgb(0x0d, 0x0f, 0x10);
/// The corpus stack graphic's front-plate fill (`#191a1f`).
pub const PLATE_FRONT: Color32 = Color32::from_rgb(0x1b, 0x1b, 0x19);
/// Category segment colors for the corpus visual (hypotheses, techniques,
/// findings, probes, then any extra bucket) — muted, distinct on the dark
/// panels. Mission logs are not a segment here; they carry `MISSION_LOG`
/// in their own section.
pub const CORPUS_PALETTE: [Color32; 5] = [
    Color32::from_rgb(0x4a, 0x6e, 0x8f), // slate blue
    Color32::from_rgb(0x6f, 0x8f, 0x5a), // moss
    SIGNAL_RED,                          // findings / signal
    INTERACTION,                         // probes / amber
    Color32::from_rgb(0x3a, 0x3b, 0x44), // plate grey (other)
];
/// The mission-log accent (`#5c5d66`) — deliberately the quietest tone in
/// the set: transcripts are bulk, not signal.
pub const MISSION_LOG: Color32 = Color32::from_rgb(0x5c, 0x5d, 0x66);
/// The house-button resting fill (`#1c1d22`).
const HOUSE_FILL: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1a);
/// The house-button hover fill (`#232429`).
const HOUSE_HOVER: Color32 = Color32::from_rgb(0x28, 0x25, 0x20);
/// Dark labels on bright amber/red button fills.
pub const ON_INTERACTION: Color32 = Color32::from_rgb(0x19, 0x12, 0x08);
pub const ON_DANGER: Color32 = Color32::from_rgb(0x1c, 0x0b, 0x06);

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
/// Equal horizontal and vertical gap between peer cards in responsive grids.
pub const CARD_GUTTER: f32 = 12.0;
/// Form controls use hard square corners throughout the app. Cards and
/// conversational surfaces own their shape separately.
pub const CONTROL_RADIUS: egui::CornerRadius = egui::CornerRadius::ZERO;
const DIALOG_SCRIM: Color32 = Color32::from_black_alpha(96);

const DISPLAY_MEDIUM_FAMILY: &str = "display-medium";
const DISPLAY_BOLD_FAMILY: &str = "display-bold";
const SC_ANNOTATION_FAMILY: &str = "sc-annotation";

/// A proportional font id at `size` (family index 0 = Inter Regular).
pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

/// JetBrains Mono at `size` — labels, data, IDs, config, and log content.
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Oswald at `size`, selecting either the 500 or 700 face.
pub fn display(size: f32, bold: bool) -> FontId {
    FontId::new(
        size,
        egui::FontFamily::Name(
            if bold {
                DISPLAY_BOLD_FAMILY
            } else {
                DISPLAY_MEDIUM_FAMILY
            }
            .into(),
        ),
    )
}

/// Uppercase Oswald with deliberate display tracking.
pub fn display_text(
    text: impl Into<String>,
    size: f32,
    bold: bool,
    color: Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        &text.into().to_uppercase(),
        0.0,
        egui::TextFormat {
            font_id: display(size, bold),
            color,
            extra_letter_spacing: 1.25,
            ..Default::default()
        },
    );
    job
}

// --- text helpers (spec §1c) ---
/// Large tracked Oswald screen header.
#[allow(dead_code)] // compatibility until remaining screens adopt page_header
pub fn screen_header(text: impl Into<String>) -> egui::text::LayoutJob {
    display_text(text, 28.0, true, TEXT)
}

/// Compact tracked Oswald Medium card title with quiet-white text held by
/// amber square brackets.
pub fn section_heading(text: impl Into<String>) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let format = |color| egui::TextFormat {
        font_id: display(17.0, false),
        color,
        extra_letter_spacing: 1.25,
        ..Default::default()
    };
    job.append("[", 0.0, format(INTERACTION));
    job.append(
        &format!("  {}  ", text.into().to_uppercase()),
        0.0,
        format(TEXT),
    );
    job.append("]", 0.0, format(INTERACTION));
    job
}

/// Muted Simplified Chinese annotation used at the right edge of card headers.
pub fn sc_annotation(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .font(FontId::new(
            10.5,
            egui::FontFamily::Name(SC_ANNOTATION_FAMILY.into()),
        ))
        .color(SC_ANNOTATION)
}

// --- control helpers (spec §1c) ---
/// A square house button: fill #1c1d22, 1px HAIRLINE stroke, padding
/// (12,7), 14px TEXT. Hover fill #232429.
pub fn house_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, HOUSE_FILL, ButtonKind::House);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        ui.button(egui::RichText::new(text.into()).size(14.0).color(TEXT))
    })
    .inner
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A house button with a Phosphor glyph and a regular text label. Keeping the
/// runs separate prevents the label font from turning private-use icons into
/// tofu after custom typography is installed.
pub fn house_icon_button(ui: &mut Ui, icon: &str, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, HOUSE_FILL, ButtonKind::House);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        ui.add(egui::Button::new(icon_label(
            icon,
            14.0,
            TEXT,
            text,
            FontId::proportional(14.0),
            TEXT,
        )))
    })
    .inner
}

/// Primary amber action. The larger padding is intentional: this is the
/// command the page wants the operator to see first.
pub fn primary_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, INTERACTION, ButtonKind::Primary);
        style.spacing.button_padding = egui::vec2(16.0, 9.0);
        ui.button(
            egui::RichText::new(text.into())
                .size(14.0)
                .strong()
                .color(ON_INTERACTION),
        )
    })
    .inner
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Primary action with a Phosphor icon pinned independently from its label.
pub fn primary_icon_button(ui: &mut Ui, icon: &str, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, INTERACTION, ButtonKind::Primary);
        style.spacing.button_padding = egui::vec2(16.0, 9.0);
        ui.add(egui::Button::new(icon_label(
            icon,
            14.0,
            ON_INTERACTION,
            text,
            FontId::proportional(14.0),
            ON_INTERACTION,
        )))
    })
    .inner
}

/// A destructive button: fill SIGNAL_RED, no stroke, radius 2, dark-on-red
/// 14px text. Hover ~10% lighter red.
pub fn destructive_button(ui: &mut Ui, text: impl Into<String>) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        override_button_visuals(style, SIGNAL_RED, ButtonKind::Destructive);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        ui.button(egui::RichText::new(text.into()).size(14.0).color(ON_DANGER))
    })
    .inner
}

/// One item in a compact segmented control. Selection uses amber text and an
/// amber keyline/fill, never the danger red.
pub fn segment_button(ui: &mut Ui, selected: bool, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let style = ui.style_mut();
        let fill = if selected {
            ROW_HL
        } else {
            Color32::TRANSPARENT
        };
        for ws in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            ws.bg_fill = fill;
            ws.weak_bg_fill = fill;
            ws.bg_stroke = egui::Stroke::new(
                1.0_f32,
                if selected {
                    KEYLINE_STRONG
                } else {
                    KEYLINE_SOFT
                },
            );
            ws.corner_radius = CONTROL_RADIUS;
        }
        let color = if selected { INTERACTION } else { TEXT_MUTED };
        ui.button(egui::RichText::new(text).size(12.5).color(color))
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
            ws.weak_bg_fill = Color32::TRANSPARENT;
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
/// square corners, inner margin (8,4). (Non-ComboBox fields; a ComboBox must be
/// restyled via `combo_field`'s scoped widget-visuals — a wrapping Frame
/// cannot reach its internal button, spec §9.)
#[allow(dead_code)]
pub fn flat_field_frame() -> egui::Frame {
    egui::Frame::default()
        .fill(PANEL)
        .stroke(egui::Stroke::new(1.0_f32, HAIRLINE))
        .corner_radius(CONTROL_RADIUS)
        .inner_margin(egui::Margin::symmetric(8, 4))
}

/// House dialog chrome: the same square smoked card and bracketed title used
/// by the main views, above a paint-only full-screen dimming scrim. The scrim
/// is not an `Area`, so it cannot enter the interaction stack or cover the
/// foreground dialog (the bug in the first modal-backdrop implementation).
pub fn dialog(
    ctx: &egui::Context,
    id_salt: &'static str,
    title: impl Into<String>,
) -> egui::Window<'static> {
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new(("dialog_scrim", id_salt)),
    ))
    .rect_filled(ctx.screen_rect(), egui::CornerRadius::ZERO, DIALOG_SCRIM);

    egui::Window::new(section_heading(title.into()))
        .id(egui::Id::new(("house_dialog", id_salt)))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(dialog_frame())
}

fn dialog_frame() -> egui::Frame {
    egui::Frame::default()
        .fill(PANEL_RAISED)
        .stroke(egui::Stroke::new(1.0_f32, CARD_BORDER))
        .corner_radius(egui::CornerRadius::ZERO)
        .inner_margin(egui::Margin::same(14))
        .shadow(egui::epaint::Shadow::NONE)
}

/// A phosphor glyph followed by text in ONE laid-out line — needed wherever
/// a widget takes a single `WidgetText` (a CollapsingHeader title or a
/// ComboBox's selected text, say) but the glyph must resolve from the
/// phosphor family. A plain RichText can only carry one family, so the icon
/// rendered as tofu; the two colours let a status glyph read differently
/// from its label.
pub fn icon_label(
    icon: &str,
    icon_size: f32,
    icon_color: Color32,
    text: &str,
    font: FontId,
    text_color: Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        icon,
        0.0,
        egui::TextFormat {
            font_id: FontId::new(icon_size, egui::FontFamily::Name("phosphor".into())),
            color: icon_color,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.append(
        text,
        4.0,
        egui::TextFormat {
            font_id: font,
            color: text_color,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job
}

/// A full-width 1px HAIRLINE hairline rule.
pub fn hairline(ui: &mut Ui) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.min.x, rect.center().y),
            egui::pos2(rect.max.x, rect.center().y),
        ],
        egui::Stroke::new(1.0_f32, HAIRLINE),
    );
}

/// Run `inner` inside a scope whose widget visuals restyle the ComboBox's
/// internal button into a flat PANEL field — 1px HAIRLINE, square corners,
/// TEXT text (spec §3). A wrapping Frame cannot reach a ComboBox's button,
/// so every flat dropdown wraps itself in this.
pub fn combo_field<R>(ui: &mut Ui, inner: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope(|ui| {
        let style = ui.style_mut();
        for ws in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            ws.bg_fill = PANEL;
            ws.weak_bg_fill = PANEL; // the ComboBox's button paints THIS one
            ws.bg_stroke = egui::Stroke::new(1.0_f32, HAIRLINE);
            ws.corner_radius = CONTROL_RADIUS;
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
#[derive(Clone, Copy)]
#[allow(dead_code)] // Primary is wired by the Agent action-hierarchy chunk
enum ButtonKind {
    House,
    Primary,
    Destructive,
}

/// Override the button widget visuals (inactive/hovered/active) for a
/// house- or destructive-styled button, using the theme's HAIRLINE stroke.
fn override_button_visuals(style: &mut egui::Style, resting: Color32, kind: ButtonKind) {
    let hover_fill = match kind {
        ButtonKind::House => HOUSE_HOVER,
        ButtonKind::Primary => INTERACTION_HOVER,
        // ~10% lighter red on hover.
        ButtonKind::Destructive => Color32::from_rgb(0xed, 0x59, 0x41),
    };
    for ws in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        ws.bg_stroke = egui::Stroke::new(
            1.0_f32,
            match kind {
                ButtonKind::House => KEYLINE_SOFT,
                ButtonKind::Primary => KEYLINE_STRONG,
                ButtonKind::Destructive => SIGNAL_RED,
            },
        );
        ws.corner_radius = CONTROL_RADIUS;
    }
    // A Button paints `weak_bg_fill`, NOT `bg_fill` (that one is for
    // checkbox/slider tracks). Setting only `bg_fill` left every house and
    // destructive button on egui's default grey — the destructive red never
    // showed up. Both are set, always.
    for (ws, fill) in [
        (&mut style.visuals.widgets.inactive, resting),
        (&mut style.visuals.widgets.hovered, hover_fill),
        (&mut style.visuals.widgets.active, hover_fill),
        (&mut style.visuals.widgets.open, hover_fill),
    ] {
        ws.bg_fill = fill;
        ws.weak_bg_fill = fill;
    }
}

/// Apply the visual language to the egui context. Call once at startup.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter-regular".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "jetbrains-mono".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"))
            .into(),
    );
    fonts.font_data.insert(
        "oswald-medium".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/Oswald-Medium.ttf")).into(),
    );
    fonts.font_data.insert(
        "oswald-bold".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/Oswald-Bold.ttf")).into(),
    );
    fonts.font_data.insert(
        "noto-serif-sc".to_owned(),
        egui::FontData::from_static(include_bytes!("../assets/fonts/NotoSerifSC-Regular.ttf"))
            .into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "inter-regular".to_owned());
    fonts
        .families
        .entry(egui::FontFamily::Monospace)
        .or_default()
        .insert(0, "jetbrains-mono".to_owned());
    fonts.families.insert(
        egui::FontFamily::Name(DISPLAY_MEDIUM_FAMILY.into()),
        vec!["oswald-medium".to_owned(), "inter-regular".to_owned()],
    );
    fonts.families.insert(
        egui::FontFamily::Name(DISPLAY_BOLD_FAMILY.into()),
        vec!["oswald-bold".to_owned(), "inter-regular".to_owned()],
    );
    fonts.families.insert(
        egui::FontFamily::Name(SC_ANNOTATION_FAMILY.into()),
        vec!["noto-serif-sc".to_owned(), "inter-regular".to_owned()],
    );
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    // Pin icon glyphs to an explicit phosphor-first family: egui_phosphor's
    // own injection puts the phosphor font at Proportional index 1, behind
    // Inter at index 0, so Inter claims the icon PUA codepoints first
    // and renders stray glyphs (the mangled `⋮`). A dedicated family whose
    // font list is phosphor-first resolves the glyph deterministically.
    fonts.families.insert(
        egui::FontFamily::Name("phosphor".into()),
        vec!["phosphor".to_owned(), "inter-regular".to_owned()],
    );
    ctx.set_fonts(fonts);

    let mut visuals = Visuals::dark();
    // Near-black shell. Smoked content surfaces are painted by widgets/cards.
    visuals.panel_fill = BG;
    visuals.window_fill = BG;
    visuals.faint_bg_color = BG;
    visuals.widgets.noninteractive.bg_fill = BG;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, KEYLINE_SOFT);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, TEXT_MUTED);
    visuals.widgets.noninteractive.corner_radius = CONTROL_RADIUS;
    // Widget surfaces are one step lighter than the canvas. `weak_bg_fill` is
    // what a Button/ComboBox actually paints, so it tracks `bg_fill` here —
    // set alone, `bg_fill` left every plain button on egui's default grey.
    visuals.widgets.inactive.bg_fill = PANEL;
    visuals.widgets.inactive.weak_bg_fill = PANEL;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, KEYLINE_SOFT);
    visuals.widgets.inactive.corner_radius = CONTROL_RADIUS;
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x28, 0x25, 0x20);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x28, 0x25, 0x20);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, KEYLINE);
    visuals.widgets.hovered.corner_radius = CONTROL_RADIUS;
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x31, 0x29, 0x20);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x31, 0x29, 0x20);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, KEYLINE_STRONG);
    visuals.widgets.active.corner_radius = CONTROL_RADIUS;
    visuals.widgets.open.bg_fill = PANEL;
    visuals.widgets.open.weak_bg_fill = PANEL;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, KEYLINE_STRONG);
    visuals.widgets.open.corner_radius = CONTROL_RADIUS;
    visuals.menu_corner_radius = CONTROL_RADIUS;
    // Selected rows stay subordinate to primary actions: a smoked amber tint
    // with amber type remains legible in every ComboBox and menu, unlike a
    // solid interaction slab. Text-edit selection uses the same quiet state.
    visuals.selection.bg_fill = ROW_HL;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, INTERACTION);
    // Inline `code` in chat markdown: a dark chip, not egui's light grey
    // block (which glared against the near-black log).
    visuals.code_bg_color = PANEL;
    visuals.override_text_color = Some(PROSE);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(SPACING, PANEL_GAP);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.text_styles.insert(TextStyle::Body, font(14.5));
    style.text_styles.insert(TextStyle::Button, mono(13.0));
    style
        .text_styles
        .insert(TextStyle::Heading, display(24.0, true));
    style.text_styles.insert(TextStyle::Monospace, mono(13.0));
    style.text_styles.insert(TextStyle::Small, mono(11.0));
    ctx.set_style(style);
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_controls_and_menus_are_square() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let style = ctx.style();
        for widget in [
            &style.visuals.widgets.noninteractive,
            &style.visuals.widgets.inactive,
            &style.visuals.widgets.hovered,
            &style.visuals.widgets.active,
            &style.visuals.widgets.open,
        ] {
            assert_eq!(widget.corner_radius, CONTROL_RADIUS);
        }
        assert_eq!(style.visuals.menu_corner_radius, CONTROL_RADIUS);
    }

    #[test]
    fn dialogs_use_opaque_square_cards_without_a_local_shadow() {
        let frame = dialog_frame();
        assert_eq!(frame.fill, PANEL_RAISED);
        assert_eq!(frame.stroke.color, CARD_BORDER);
        assert_eq!(frame.corner_radius, egui::CornerRadius::ZERO);
        assert_eq!(frame.shadow, egui::epaint::Shadow::NONE);
        assert!(DIALOG_SCRIM.a() > 0);
    }

    fn linear(channel: u8) -> f32 {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(color: Color32) -> f32 {
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (bright, dark) = if luminance(a) > luminance(b) {
            (luminance(a), luminance(b))
        } else {
            (luminance(b), luminance(a))
        };
        (bright + 0.05) / (dark + 0.05)
    }

    #[test]
    fn command_palette_keeps_text_and_actions_legible() {
        assert!(contrast(TEXT, BG) >= 7.0);
        assert!(contrast(TEXT_MUTED, BG) >= 4.5);
        assert!(contrast(INTERACTION, BG) >= 4.5);
        assert!(contrast(INTERACTION, ROW_HL) >= 4.5);
        assert!(contrast(SIGNAL_RED, BG) >= 4.5);
        assert!(contrast(ON_INTERACTION, INTERACTION) >= 4.5);
        assert!(contrast(ON_DANGER, SIGNAL_RED) >= 4.5);
        assert_ne!(INTERACTION, SIGNAL_RED);
    }

    #[test]
    fn cards_use_a_translucent_surface_and_neutral_border() {
        assert_eq!(card_fill().a(), 0x70);
        assert_eq!(CARD_BORDER.r(), CARD_BORDER.g() - 2);
        assert_eq!(CARD_BORDER.g(), CARD_BORDER.b() - 2);
    }

    #[test]
    fn bundled_typography_roles_parse_and_layout() {
        let ctx = egui::Context::default();
        apply(&ctx);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label(display_text("display", 24.0, true, TEXT));
                ui.label(egui::RichText::new("Reading prose"));
                ui.monospace("data-id");
                ui.label(sc_annotation("系统状态"));
            });
        });
        assert!(!output.shapes.is_empty());
    }

    #[test]
    fn card_titles_use_white_type_inside_amber_brackets() {
        let heading = section_heading("system status");
        assert_eq!(heading.text, "[  SYSTEM STATUS  ]");
        assert_eq!(heading.sections.len(), 3);
        assert_eq!(heading.sections[0].format.color, INTERACTION);
        assert_eq!(heading.sections[1].format.color, TEXT);
        assert_eq!(heading.sections[2].format.color, INTERACTION);
    }

    #[test]
    fn icon_labels_keep_phosphor_separate_from_text_font() {
        let label = icon_label(
            egui_phosphor::regular::PLUS,
            14.0,
            TEXT,
            "New Mission",
            FontId::proportional(14.0),
            TEXT,
        );

        assert_eq!(label.sections.len(), 2);
        assert_eq!(
            label.sections[0].format.font_id.family,
            egui::FontFamily::Name("phosphor".into())
        );
        assert_eq!(
            label.sections[1].format.font_id.family,
            egui::FontFamily::Proportional
        );
    }
}
