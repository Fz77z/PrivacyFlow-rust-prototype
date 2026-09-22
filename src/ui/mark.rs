use crate::state::{FailureKind, HudState};
use crate::ui::theme;
use egui::{Color32, Painter, Pos2, Rect, Rounding, Stroke, Vec2};

/// The four bars never move. These are their positions and resting heights as
/// fractions of the mark box, so every state paints the same silhouette and
/// only colour and fill distinguish them.
const BARS: [(f32, f32); 4] = [(0.00, 0.27), (0.26, 0.67), (0.52, 0.37), (0.78, 0.57)];
/// Bar width as a fraction of the mark box. Keeps the bars proportional when
/// a caller scales the box down uniformly; it is the caller's job to keep the
/// box roughly proportional to the 36 point box this was designed against, so
/// the silhouette this fraction produces stays the intended shape.
const BAR_WIDTH_FRACTION: f32 = 4.0 / 36.0;

/// How a bar is drawn. Filled bars read as live, hollow bars as unlit - the
/// same language as a signal meter that has lost its signal.
#[derive(Clone, Copy, PartialEq)]
enum Bar {
    Filled(Color32),
    Hollow(Color32),
}

pub struct Appearance {
    pub state: HudState,
    pub failure: Option<FailureKind>,
    /// How long ago the dictation settled on its result, in seconds, if it
    /// has. Drives the one-off gestures that mark an ending: the pop of a
    /// success and the shake of a failure.
    pub settled_for: Option<f32>,
}

/// While listening the bars never drop below this, so silence reads as a row
/// of short bars waiting for a voice rather than as the resting silhouette.
const LISTENING_FLOOR: f32 = 0.2;

/// The transcribing wave: how fast it travels, how far apart the bars sit on
/// it, and how much of the box it swings through. Fast and wide enough to be
/// unmistakably moving at bead size, where the old shimmer read as still.
const WAVE_SPEED: f32 = 7.0;
const WAVE_SPACING: f32 = 0.9;
const WAVE_FLOOR: f32 = 0.3;
const WAVE_SWING: f32 = 0.5;

/// The success pop: how long it lasts and how far the bars jump.
const POP_SECONDS: f32 = 0.32;
const POP_HEIGHT: f32 = 0.45;

/// The failure shake: how long it lasts, how fast it swings, and how far,
/// as a fraction of the mark's width.
const SHAKE_SECONDS: f32 = 0.42;
const SHAKE_SPEED: f32 = 42.0;
const SHAKE_WIDTH: f32 = 0.11;

/// Paints the mark. `voice` is how far each bar stands while listening, from
/// the voice meter, and `time` drives the transcribing wave. Neither ever
/// changes the box itself, only the bars inside it and, for the failure
/// shake, where the box sits for a moment.
pub fn paint(
    painter: &Painter,
    rect: Rect,
    appearance: &Appearance,
    voice: [f32; 4],
    time: f64,
) {
    let rect = rect.translate(Vec2::new(shake(appearance) * rect.width(), 0.0));
    let bar_width = rect.width() * BAR_WIDTH_FRACTION;
    // Bars scale with the box, and so does the stroke that draws a hollow
    // one: at the dictating size's narrower bars, a fixed 1.4pt stroke would
    // leave no hollow interior, and hollow-versus-filled is the only state
    // distinction the mark carries at that size.
    let hollow_stroke = 1.4 * rect.width() / 36.0;
    let bars = bars_for(appearance);
    for (index, (x, resting)) in BARS.iter().enumerate() {
        let height = rect.height() * animated_height(appearance, index, *resting, voice, time);
        let left = rect.left() + rect.width() * x;
        let bar = Rect::from_center_size(
            Pos2::new(left + bar_width / 2.0, rect.center().y),
            Vec2::new(bar_width, height),
        );
        match bars[index] {
            Bar::Filled(color) => {
                painter.rect_filled(bar, Rounding::same(bar_width / 2.0), color)
            }
            Bar::Hollow(color) => painter.rect_stroke(
                bar,
                Rounding::same(bar_width / 2.0),
                Stroke::new(hollow_stroke, color),
            ),
        };
    }
    if appearance.failure == Some(FailureKind::InputUnavailable) {
        // The one mark that adds a stroke rather than recolouring. It never
        // displaces a bar; it crosses them, which is how a muted input reads
        // everywhere else on the system. The inset scales with the box for
        // the same reason the stroke width does.
        let inset = 2.0 * rect.width() / 36.0;
        painter.line_segment(
            [
                Pos2::new(rect.left() + inset, rect.bottom() - inset),
                Pos2::new(rect.right() - inset, rect.top() + inset),
            ],
            Stroke::new(2.4 * rect.width() / 36.0, theme::ERROR),
        );
    }
}

fn bars_for(appearance: &Appearance) -> [Bar; 4] {
    let Some(kind) = appearance.failure else {
        let color = match appearance.state {
            HudState::Listening => theme::LISTENING,
            HudState::Processing => theme::TRANSCRIBING,
            HudState::Done | HudState::Copied => theme::INSERTED,
            _ => theme::IDLE,
        };
        return [Bar::Filled(color); 4];
    };
    match kind {
        // Never lit up: the run did not start.
        FailureKind::Blocked => [Bar::Hollow(theme::ERROR); 4],
        // The input itself is shut, so the bars are dead rather than absent.
        FailureKind::InputUnavailable => [Bar::Filled(theme::MARK_DEAD); 4],
        // Lit, then dropped - the shape of a lost connection.
        FailureKind::Dropped => [
            Bar::Filled(theme::ERROR),
            Bar::Filled(theme::ERROR),
            Bar::Hollow(theme::MARK_UNLIT),
            Bar::Hollow(theme::MARK_UNLIT),
        ],
    }
}

/// How tall a bar stands right now, as a fraction of the box.
///
/// Each state that moves has its own motion as well as its own colour. At
/// bead size colour is the only other thing that tells the states apart,
/// and colour alone fails for anyone who cannot tell red from green.
fn animated_height(
    appearance: &Appearance,
    index: usize,
    resting: f32,
    voice: [f32; 4],
    time: f64,
) -> f32 {
    if appearance.failure.is_some() {
        return resting;
    }
    match appearance.state {
        HudState::Listening => LISTENING_FLOOR + (1.0 - LISTENING_FLOOR) * voice[index],
        HudState::Processing => {
            let phase = time as f32 * WAVE_SPEED - index as f32 * WAVE_SPACING;
            WAVE_FLOOR + WAVE_SWING * (phase.sin() * 0.5 + 0.5)
        }
        HudState::Done | HudState::Copied => {
            let Some(settled_for) = appearance.settled_for else {
                return resting;
            };
            let progress = (settled_for / POP_SECONDS).clamp(0.0, 1.0);
            // One smooth hump from rest, up, and back to rest.
            let pop = (progress * std::f32::consts::PI).sin() * POP_HEIGHT;
            (resting * (1.0 + pop)).min(1.0)
        }
        _ => resting,
    }
}

/// How far the mark is pushed sideways by a failure's shake, as a fraction of
/// its width. A few quick swings that die away, like a head shaking no.
fn shake(appearance: &Appearance) -> f32 {
    let (Some(_), Some(settled_for)) = (appearance.failure, appearance.settled_for) else {
        return 0.0;
    };
    if settled_for >= SHAKE_SECONDS {
        return 0.0;
    }
    let dying = 1.0 - settled_for / SHAKE_SECONDS;
    (settled_for * SHAKE_SPEED).sin() * SHAKE_WIDTH * dying * dying
}
