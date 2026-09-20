use egui::{Color32, FontFamily, FontId, Vec2};

/// The capsule is a fixed shape. Every state paints into these same metrics,
/// so nothing in the widget can move or resize between states.
pub const CAPSULE_SIZE: Vec2 = Vec2::new(240.0, 56.0);
pub const CAPSULE_RADIUS: f32 = 28.0;
pub const PAD_LEFT: f32 = 17.0;
pub const PAD_RIGHT: f32 = 10.0;
pub const MARK_SIZE: Vec2 = Vec2::new(36.0, 30.0);
pub const MARK_GAP: f32 = 11.0;
pub const ICON_SIZE: f32 = 36.0;
pub const ICON_RADIUS: f32 = 11.0;

pub const FILL: Color32 = Color32::from_rgb(25, 28, 36);
/// History cards sit on FILL and must read as raised from it, not as a
/// different surface, so this is a single step lighter and nothing more.
pub const CARD_FILL: Color32 = Color32::from_rgb(29, 33, 43);
pub const BORDER: Color32 = Color32::from_rgb(60, 66, 80);
pub const IDLE: Color32 = Color32::from_rgb(116, 124, 140);
pub const LISTENING: Color32 = Color32::from_rgb(113, 218, 178);
pub const TRANSCRIBING: Color32 = Color32::from_rgb(132, 164, 255);
pub const INSERTED: Color32 = Color32::from_rgb(151, 224, 157);
pub const ERROR: Color32 = Color32::from_rgb(224, 112, 112);
pub const ERROR_TEXT: Color32 = Color32::from_rgb(240, 185, 185);
pub const ERROR_BORDER: Color32 = Color32::from_rgb(109, 54, 54);
pub const LABEL: Color32 = Color32::from_rgb(243, 245, 249);
pub const MUTED: Color32 = Color32::from_rgb(157, 165, 181);
pub const ICON_TINT: Color32 = Color32::from_rgb(177, 185, 201);

/// Named so call sites read as intent rather than as a magic family string.
pub fn label_font() -> FontId {
    FontId::new(13.5, FontFamily::Name("semibold".into()))
}

/// Errors are deliberately quieter than the healthy states, so the failure
/// headline is smaller than the label it replaces.
pub fn error_font() -> FontId {
    FontId::new(11.5, FontFamily::Proportional)
}

/// egui bundles only Ubuntu-Light, which has no bold face, so the app's
/// typography is supplied rather than assumed. Inter Regular becomes the
/// proportional default and SemiBold is registered as its own family, because
/// egui selects weight by family rather than by a weight attribute.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".to_owned(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        "Inter-SemiBold".to_owned(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.ttf")),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Inter".to_owned());
    fonts
        .families
        .insert(FontFamily::Name("semibold".into()), vec!["Inter-SemiBold".to_owned()]);
    ctx.set_fonts(fonts);
}
