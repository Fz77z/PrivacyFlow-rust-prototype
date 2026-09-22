//! The toast: what PrivacyFlow says when a dictation ends up on the clipboard
//! instead of at the cursor.
//!
//! Every outcome it reports is already on the capsule, for 1.4 seconds, as a
//! single word. That is enough when the user is watching, and these are
//! exactly the outcomes where they are not: the text did not appear where
//! they were looking, so neither did their attention. The toast carries the
//! two things the capsule has no room for - the words that were kept, and
//! how to place them.
//!
//! The window is inert on purpose. It cannot be clicked, so it can never take
//! focus from whatever the user is typing in, and it swallows nothing that
//! was aimed at the application underneath it.

use crate::platform::WorkArea;
use crate::state::{Toast, ToastKind};
use crate::ui::theme;
use crate::window_position::{self, Centre};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Align2, Color32, FontFamily, FontId, Pos2, Rounding, Vec2};
use std::sync::Arc;
use std::time::Duration;

/// How long a toast stays up.
///
/// Three times the capsule's own dwell, because this asks to be read rather
/// than glanced at: a headline, a sentence of the user's own speech, and what
/// to press.
pub const DWELL: Duration = Duration::from_secs(4);

/// The fixed width. Wide enough for a line of ordinary speech at this size,
/// narrow enough to sit over a document without covering it.
const WIDTH: f32 = 320.0;
const PAD: f32 = 14.0;
/// The gap between the capsule and the toast, whichever side it lands on.
const GAP: f32 = 12.0;
/// The most body text shown. Past this the toast would stop being a notice
/// and start being a window; the whole text is in the console, and on the
/// clipboard.
const BODY_ROWS: usize = 3;
const HEADLINE_HEIGHT: f32 = 15.0;
const FOOTER_HEIGHT: f32 = 13.0;
const LINE_GAP: f32 = 6.0;

/// How far the contents rise as the toast appears. The window itself never
/// moves: asking the window server to reposition a window every frame stutters
/// where animating inside a still window does not.
const RISE: f32 = 8.0;
const FADE_IN: f32 = 0.14;
const FADE_OUT: f32 = 0.22;

/// Where the toast window goes, as a top left corner.
///
/// Placed from the capsule rather than from a corner of the screen, because
/// the capsule is where the user has already chosen to have PrivacyFlow speak
/// to them. Measured against the full capsule at every capsule size, so the
/// gap is honest when the capsule is a bead and the toast never lands on top
/// of it mid-animation.
pub fn place(capsule: Centre, capsule_size: Vec2, toast: Vec2, areas: &[WorkArea]) -> (f32, f32) {
    let above = capsule.y - capsule_size.y / 2.0 - GAP - toast.y / 2.0;
    let below = capsule.y + capsule_size.y / 2.0 + GAP + toast.y / 2.0;
    // A capsule parked at the top of a display has no room above it, and a
    // toast drawn off the screen is a message nobody receives.
    let fits_above = window_position::area_for(capsule, areas)
        .is_none_or(|area| above - toast.y / 2.0 >= area.y as f32);
    let centre = Centre { x: capsule.x, y: if fits_above { above } else { below } };
    window_position::place(centre, (toast.x, toast.y), areas)
}

/// How opaque the toast is this frame: in quickly, out gently, so it does not
/// blink into existence beside whatever the user is reading.
fn opacity(elapsed: f32) -> f32 {
    let remaining = DWELL.as_secs_f32() - elapsed;
    let arriving = ease_out((elapsed / FADE_IN).clamp(0.0, 1.0));
    let leaving = ease_out((remaining / FADE_OUT).clamp(0.0, 1.0));
    arriving.min(leaving)
}

/// Fast at first and slowing into place, the way things come to rest. A
/// linear fade and rise read as mechanical.
fn ease_out(progress: f32) -> f32 {
    1.0 - (1.0 - progress).powi(3)
}

fn body_font() -> FontId {
    FontId::new(13.0, FontFamily::Proportional)
}

fn footer_font() -> FontId {
    FontId::new(11.0, FontFamily::Proportional)
}

/// Lay out one piece of text at the toast's width.
///
/// Laying out before the window exists is what makes the height exact: the
/// window is built around the text rather than the text being fitted into a
/// guessed window, and the truncation of a long dictation falls out of the
/// same measurement.
fn galley(
    ctx: &egui::Context,
    text: &str,
    font: FontId,
    color: Color32,
    rows: usize,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat { font_id: font, color, ..Default::default() },
    );
    job.wrap = TextWrapping {
        max_width: WIDTH - PAD * 2.0,
        max_rows: rows,
        overflow_character: Some('…'),
        ..Default::default()
    };
    ctx.fonts(|fonts| fonts.layout_job(job))
}

/// Paint the toast into its own window.
///
/// The window is created here and destroyed the moment the state stops
/// holding a toast, so nothing has to remember to hide it.
pub fn show(ctx: &egui::Context, toast: &Toast, capsule: Centre, areas: &[WorkArea]) {
    let (headline_colour, border) = match toast.kind {
        ToastKind::Copied => (theme::LABEL, theme::BORDER_INSERTED),
        ToastKind::Failed => (theme::ERROR_TEXT, theme::ERROR_BORDER),
        // Not an error: nothing has gone wrong yet, and saying so in red
        // would read as a failure the user cannot answer. This one they can.
        ToastKind::Quiet => (theme::WARNING_TEXT, theme::WARNING_BORDER),
    };
    let body = galley(ctx, &toast.body, body_font(), theme::LABEL, BODY_ROWS);
    let height = PAD * 2.0 + HEADLINE_HEIGHT + LINE_GAP + body.size().y + LINE_GAP + FOOTER_HEIGHT;
    let size = Vec2::new(WIDTH, height);
    let (x, y) = place(capsule, theme::CAPSULE_SIZE, size, areas);
    let alpha = opacity(toast.raised_at.elapsed().as_secs_f32());
    let rise =
        RISE * (1.0 - ease_out((toast.raised_at.elapsed().as_secs_f32() / FADE_IN).clamp(0.0, 1.0)));
    let builder = egui::ViewportBuilder::default()
        .with_inner_size([size.x, size.y])
        .with_position([x, y])
        .with_decorations(false)
        .with_resizable(false)
        .with_transparent(true)
        .with_always_on_top()
        // Inert: it never becomes key, and clicks aimed at whatever is
        // underneath reach it rather than being eaten by a notice.
        .with_active(false)
        .with_mouse_passthrough(true)
        .with_taskbar(false)
        .with_title("PrivacyFlow notice");
    ctx.show_viewport_immediate(
        egui::ViewportId::from_hash_of("toast"),
        builder,
        |ctx, _class| {
            let frame = egui::Frame::none().fill(Color32::TRANSPARENT);
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                let rect = ui.max_rect().shrink(1.0).translate(Vec2::new(0.0, rise));
                let painter = ui.painter();
                let fade = |colour: Color32| colour.gamma_multiply(alpha);
                // The outline is a filled shape with the fill laid on top,
                // not a stroke beside it, for the same reason as the
                // capsule's: a stroke and a fill meeting at an edge let the
                // desktop through as a light hairline.
                painter.rect_filled(rect, Rounding::same(14.0), fade(border));
                painter.rect_filled(rect.shrink(1.0), Rounding::same(13.0), fade(theme::FILL));
                let left = rect.left() + PAD;
                let mut top = rect.top() + PAD;
                painter.text(
                    Pos2::new(left, top),
                    Align2::LEFT_TOP,
                    &toast.headline,
                    theme::label_font(),
                    fade(headline_colour),
                );
                top += HEADLINE_HEIGHT + LINE_GAP;
                // Laid out once, above, so the window is exactly as tall as
                // the words it holds.
                painter.galley(Pos2::new(left, top), body.clone(), fade(theme::LABEL));
                top += body.size().y + LINE_GAP;
                painter.text(
                    Pos2::new(left, top),
                    Align2::LEFT_TOP,
                    toast.footer,
                    footer_font(),
                    fade(theme::MUTED),
                );
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::WorkArea;
    use crate::window_position::Centre;
    use egui::vec2;

    fn one_display() -> Vec<WorkArea> {
        vec![WorkArea { x: 0.0, y: 0.0, width: 2000.0, height: 1000.0 }]
    }

    /// The toast belongs to the capsule, so it is placed from the capsule
    /// rather than from a fixed corner of the screen: wherever the user has
    /// parked the capsule is where they will look when it speaks.
    #[test]
    fn the_toast_sits_centred_above_the_capsule() {
        let (x, y) = place(
            Centre { x: 1000.0, y: 500.0 },
            vec2(216.0, 56.0),
            vec2(320.0, 90.0),
            &one_display(),
        );
        assert_eq!(x, 840.0, "centred on the capsule");
        assert_eq!(y, 370.0, "clear of the capsule's top edge by the gap");
    }

    /// A capsule parked at the top of the screen has no room above it, and a
    /// toast placed there would be drawn off the display where nobody can
    /// read it.
    #[test]
    fn a_capsule_at_the_top_of_the_display_gets_its_toast_below_it() {
        let (_, y) = place(
            Centre { x: 1000.0, y: 40.0 },
            vec2(216.0, 56.0),
            vec2(320.0, 90.0),
            &one_display(),
        );
        assert_eq!(y, 80.0, "below the capsule's bottom edge by the gap");
    }
}
