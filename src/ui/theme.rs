//! Design tokens (colors, radii, fonts) and egui style setup.
//!
//! The one bold idea: a session's state is a color shared by the sidebar row,
//! the header chip and the chat's "working" line (see [`status_color`]).

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke,
};

use crate::state::{PendingReason, SessionStatus};

// -- backgrounds --------------------------------------------------------------
/// Central area.
pub const BG: Color32 = Color32::from_rgb(0x1C, 0x1B, 0x1A);
/// Side/bottom panels and header.
pub const SURFACE: Color32 = Color32::from_rgb(0x23, 0x22, 0x20);
/// User bubble, cards, inputs.
pub const RAISED: Color32 = Color32::from_rgb(0x2C, 0x2A, 0x27);
/// 1 px borders.
pub const LINE: Color32 = Color32::from_rgb(0x3A, 0x37, 0x33);

// -- text ---------------------------------------------------------------------
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE4, 0xDE);
pub const MUTED: Color32 = Color32::from_rgb(0x9A, 0x94, 0x8C);

// -- brand and states ---------------------------------------------------------
/// Brand color (icon, selection, focus). Also the "working" state.
pub const ACCENT: Color32 = Color32::from_rgb(0xD9, 0x77, 0x57);
/// Needs permission.
pub const AMBER: Color32 = Color32::from_rgb(0xE3, 0xA6, 0x4A);
/// Waiting for your message.
pub const BLUE: Color32 = Color32::from_rgb(0x6F, 0xA3, 0xD8);
pub const RED: Color32 = Color32::from_rgb(0xE0, 0x64, 0x64);
/// Tool call OK.
pub const OK: Color32 = Color32::from_rgb(0x7D, 0xB3, 0x86);

// -- radii --------------------------------------------------------------------
pub const R_CARD: u8 = 8;
pub const R_CHIP: u8 = 255;
pub const R_BUBBLE: u8 = 10;
pub const R_BUTTON: u8 = 6;

/// Family used for semibold text.
pub fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Semibold text (titles, group names).
pub fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(semibold_family())
}

/// Section title (18 px semibold).
pub fn title(text: impl Into<String>) -> RichText {
    RichText::new(text)
        .family(semibold_family())
        .size(18.0)
        .color(TEXT)
}

/// Muted small text.
pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).small().color(MUTED)
}

/// Color of a session status.
pub fn status_color(status: &SessionStatus) -> Color32 {
    match status {
        SessionStatus::Starting | SessionStatus::Running => ACCENT,
        SessionStatus::Pending(PendingReason::Permission) => AMBER,
        SessionStatus::Pending(PendingReason::AwaitingInput) => BLUE,
        SessionStatus::Finished { error: Some(_) } => RED,
        SessionStatus::Finished { error: None } => MUTED,
    }
}

/// Spanish label of a session status.
pub fn status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Starting => "Iniciando",
        SessionStatus::Running => "Trabajando",
        SessionStatus::Pending(PendingReason::Permission) => "Necesita permiso",
        SessionStatus::Pending(PendingReason::AwaitingInput) => "Esperando tu mensaje",
        SessionStatus::Finished { error: Some(_) } => "Error",
        SessionStatus::Finished { error: None } => "Terminada",
    }
}

/// Dark tint of a color, for chip/card backgrounds.
pub fn tint(c: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), FontData::from_static(bytes).into());
    };
    add(
        "plex-sans",
        include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf"),
    );
    add(
        "plex-sans-semibold",
        include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    );
    add(
        "plex-mono",
        include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf"),
    );
    // Plex first, egui's bundled fonts stay as fallback for missing glyphs.
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "plex-sans".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "plex-mono".to_owned());
    let mut semi = vec!["plex-sans-semibold".to_owned()];
    semi.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts.families.insert(semibold_family(), semi);
    ctx.set_fonts(fonts);
}

/// Installs fonts, visuals and text styles.
pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = RAISED;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.extreme_bg_color = BG;
    v.faint_bg_color = SURFACE;
    v.widgets.noninteractive.fg_stroke.color = TEXT;
    v.widgets.inactive.fg_stroke.color = TEXT;
    v.selection.bg_fill = tint(ACCENT, 90);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    let r = CornerRadius::same(R_BUTTON);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.noninteractive.corner_radius = r;
    v.widgets.inactive.weak_bg_fill = RAISED;
    v.widgets.inactive.bg_fill = RAISED;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    v.widgets.inactive.corner_radius = r;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x36, 0x33, 0x2F);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x36, 0x33, 0x2F);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, MUTED);
    v.widgets.hovered.corner_radius = r;
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x40, 0x3C, 0x37);
    v.widgets.active.bg_fill = Color32::from_rgb(0x40, 0x3C, 0x37);
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    v.widgets.active.corner_radius = r;
    v.widgets.open.corner_radius = r;
    ctx.set_visuals(v);

    ctx.all_styles_mut(|s| {
        use egui::TextStyle;
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(14.5));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(14.5));
        s.text_styles
            .insert(TextStyle::Heading, FontId::new(18.0, semibold_family()));
        s.text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
    });
}
