use egui::{Color32, FontFamily, FontId, Vec2};

/// The three shapes the capsule is painted as. All of them are pills, so
/// none of them carries a radius: the radius is half the painted height at
/// every point of the animation between them, and a stored radius would be a
/// second thing that has to agree with the first.
pub const CAPSULE_SIZE: Vec2 = Vec2::new(216.0, 56.0);
pub const PAD_LEFT: f32 = 17.0;
pub const PAD_RIGHT: f32 = 10.0;
/// The mark at full size. Every smaller capsule scales this by its own
/// height, so the mark keeps its proportions and its position within the
/// shape at every point of the animation. The bar width and the hollow
/// stroke are fractions of this box, so they scale with it too.
pub const MARK_SIZE: Vec2 = Vec2::new(36.0, 30.0);
pub const MARK_GAP: f32 = 11.0;
pub const ICON_SIZE: f32 = 36.0;
pub const ICON_RADIUS: f32 = 11.0;
/// The bead: the capsule at rest in minimal mode.
///
/// Only slightly smaller than the dictating size, and the same 2:1 shape, so
/// growing from one to the other is a pure scale. Anything else changes the
/// widget's proportions mid-animation, which reads as the shape deforming
/// rather than as one object getting bigger.
pub const BEAD_SIZE: Vec2 = Vec2::new(72.0, 36.0);
/// The dictating size. The bead at a slightly larger scale, same shape, so
/// beginning to speak reads as the capsule growing rather than as a new thing
/// arriving.
pub const ACTIVE_SIZE: Vec2 = Vec2::new(88.0, 44.0);

/// The window itself, which never changes size.
///
/// Larger than the full capsule, so the capsule can grow in place with room
/// for its shadow. Expanding on approach does not depend on it: the event tap
/// watches the full capsule's footprint on screen, so the capsule notices the
/// pointer coming whether or not the window is listening to the mouse.
///
/// Clicks outside the expanded capsule's footprint are let through to
/// whatever is underneath, so the ring this leaves around the capsule costs
/// nothing but the room the capsule and its shadow need to be drawn in.
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
/// Close to the fill on purpose. At bead size the capsule is mostly edge, and
/// a border with real contrast against the fill stops reading as an edge and
/// starts reading as a ring drawn around something.
pub const BORDER: Color32 = Color32::from_rgb(26, 26, 26);

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
/// Amber rather than red. The quiet warning arrives while the dictation is
/// still running and can still be saved, so it must not read as a failure
/// that has already happened.
pub const WARNING_TEXT: Color32 = Color32::from_rgb(238, 205, 156);

// Each state tints the capsule's one point border. They are deliberately
// dark: the border says which state this is to someone already looking,
// and the mark says it to someone glancing.
pub const BORDER_LISTENING: Color32 = Color32::from_rgb(42, 95, 72);
pub const BORDER_TRANSCRIBING: Color32 = Color32::from_rgb(51, 64, 110);
pub const BORDER_INSERTED: Color32 = Color32::from_rgb(58, 95, 63);
pub const ERROR_BORDER: Color32 = Color32::from_rgb(92, 46, 46);
pub const WARNING_BORDER: Color32 = Color32::from_rgb(99, 76, 38);

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
    FontId::new(12.0, FontFamily::Name("semibold".into()))
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
