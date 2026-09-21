use egui::{Color32, FontFamily, FontId, Vec2};

/// The three shapes the capsule is painted as. All of them are pills, so
/// none of them carries a radius: the radius is half the painted height at
/// every point of the animation between them, and a stored radius would be a
/// second thing that has to agree with the first.
pub const CAPSULE_SIZE: Vec2 = Vec2::new(240.0, 56.0);
pub const PAD_LEFT: f32 = 17.0;
pub const PAD_RIGHT: f32 = 10.0;
pub const MARK_SIZE: Vec2 = Vec2::new(36.0, 30.0);
pub const MARK_GAP: f32 = 11.0;
pub const ICON_SIZE: f32 = 36.0;
pub const ICON_RADIUS: f32 = 11.0;
/// The bead: the capsule at rest in minimal mode. Too narrow for the label,
/// but it carries the mark at nearly full height, so the bars read as marks
/// rather than as specks. It shares its height with the dictating size to
/// within a few points on purpose: starting to speak should widen the capsule
/// and lift it very slightly, not appear to replace it with a larger one.
pub const BEAD_SIZE: Vec2 = Vec2::new(48.0, 28.0);
/// The mark inside the bead. The failure kinds are still not distinguishable
/// here, which the ambient spec accepted knowingly; what survives is that
/// something is there, and what colour it is.
pub const BEAD_MARK_SIZE: Vec2 = Vec2::new(24.0, 14.0);
/// The mark inside the dictating capsule. Very nearly a uniform scale of
/// MARK_SIZE, which is what keeps the bars from looking clubbed: width
/// against height for the tallest bar stays near 5:1, as it is at full size.
pub const ACTIVE_MARK_SIZE: Vec2 = Vec2::new(28.0, 17.0);
/// The dictating size. A step up from the bead rather than a different
/// object: four points taller and not quite twice as wide, so beginning to
/// speak reads as the capsule opening up rather than as a new thing arriving.
pub const ACTIVE_SIZE: Vec2 = Vec2::new(88.0, 32.0);

/// The window itself, which never changes size.
///
/// Larger than the full capsule on purpose. The window is the only thing
/// macOS delivers mouse events for, so it is also the area within which the
/// capsule can notice someone approaching, and the user asked for expansion
/// on approach rather than on contact. A catchment the size of the expanded
/// capsule could not do that: the bead is 14 points tall inside it, so
/// vertically "close" and "on" would be the same thing.
///
/// The cost is that clicks anywhere in here are swallowed, including the ring
/// this leaves outside the expanded capsule. Most of the area conceals that
/// cost, because a pointer inside it is exactly what expands the capsule to
/// fill the middle, but the ring is real and is the first number to revisit
/// if it proves annoying.
pub const CATCHMENT_SIZE: Vec2 = Vec2::new(320.0, 120.0);

/// The window size for a given setting.
///
/// Only minimal mode needs a catchment, because only minimal mode has to
/// notice someone approaching. With it off the window is exactly the capsule,
/// as it has always been, so the setting being off costs no screen space that
/// the capsule does not visibly occupy.
pub fn window_size(minimal_mode: bool) -> Vec2 {
    if minimal_mode {
        CATCHMENT_SIZE
    } else {
        CAPSULE_SIZE
    }
}

// The surfaces are a neutral near black. An earlier palette tinted them
// blue, which quietly fought every other colour in the widget: the greens
// read as minty, and a desaturated accent laid over them looked muddy
// rather than subtle. Neutral surfaces let the accents be the only colour
// in the frame.
pub const FILL: Color32 = Color32::from_rgb(10, 10, 10);
/// History cards sit on FILL and must read as raised from it, not as a
/// different surface, so this is a single step lighter and nothing more.
pub const CARD_FILL: Color32 = Color32::from_rgb(20, 20, 20);
pub const BORDER: Color32 = Color32::from_rgb(38, 38, 38);

// The text greys are neutral for the same reason as the surfaces.
pub const LABEL: Color32 = Color32::from_rgb(245, 245, 245);
pub const MUTED: Color32 = Color32::from_rgb(154, 154, 154);
pub const ICON_TINT: Color32 = Color32::from_rgb(176, 176, 176);
/// The icon's hover plate. On a near black fill this only has to be a hint
/// that something is interactive, so it is barely lighter than the capsule.
pub const ICON_HOVER: Color32 = Color32::from_rgb(35, 35, 35);

// The state accents. These are the only saturated colour in the app, which
// is why each one has to earn its place rather than being a default hue.
pub const IDLE: Color32 = Color32::from_rgb(110, 110, 110);
pub const LISTENING: Color32 = Color32::from_rgb(95, 211, 155);
pub const TRANSCRIBING: Color32 = Color32::from_rgb(110, 142, 245);
pub const INSERTED: Color32 = Color32::from_rgb(134, 217, 143);
pub const ERROR: Color32 = Color32::from_rgb(224, 112, 112);
pub const ERROR_TEXT: Color32 = Color32::from_rgb(235, 179, 179);

// Each state tints the capsule's one point border. They are deliberately
// dark: the border says which state this is to someone already looking,
// and the mark says it to someone glancing.
pub const BORDER_LISTENING: Color32 = Color32::from_rgb(42, 95, 72);
pub const BORDER_TRANSCRIBING: Color32 = Color32::from_rgb(51, 64, 110);
pub const BORDER_INSERTED: Color32 = Color32::from_rgb(58, 95, 63);
pub const ERROR_BORDER: Color32 = Color32::from_rgb(92, 46, 46);

// The unread dot and the icon tint that goes with it. The dot is the one
// thing in the widget allowed to be brighter than its surroundings,
// because its whole job is to be noticed after the fact.
pub const ICON_ALERT: Color32 = Color32::from_rgb(224, 138, 138);
pub const UNREAD_DOT: Color32 = Color32::from_rgb(246, 128, 128);

/// A bar that is present but dead, used for a microphone that cannot be
/// opened. Dimmed rather than absent, because the input exists and is shut.
pub const MARK_DEAD: Color32 = Color32::from_rgb(74, 58, 58);
/// A bar that was never lit, used for the half of the mark that dropped out
/// when the pipeline lost an utterance it had already captured.
pub const MARK_UNLIT: Color32 = Color32::from_rgb(107, 71, 71);

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
