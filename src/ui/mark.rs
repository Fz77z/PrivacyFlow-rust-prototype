use crate::state::{FailureKind, HudState};
use crate::ui::theme;
use egui::{Color32, Painter, Pos2, Rect, Rounding, Stroke, Vec2};

/// The four bars never move. These are their positions and resting heights as
/// fractions of the mark box, so every state paints the same silhouette and
/// only colour and fill distinguish them.
const BARS: [(f32, f32); 4] = [(0.00, 0.27), (0.26, 0.67), (0.52, 0.37), (0.78, 0.57)];
const BAR_WIDTH: f32 = 4.0;

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
}

/// Paints the mark. `level` is the current microphone level and `time` drives
/// the shimmer; both only ever change bar heights inside `rect`, never the
/// box itself.
pub fn paint(painter: &Painter, rect: Rect, appearance: &Appearance, level: f32, time: f64) {
    let bars = bars_for(appearance);
    for (index, (x, resting)) in BARS.iter().enumerate() {
        let height = rect.height() * animated_height(appearance, index, *resting, level, time);
        let left = rect.left() + rect.width() * x;
        let bar = Rect::from_center_size(
            Pos2::new(left + BAR_WIDTH / 2.0, rect.center().y),
            Vec2::new(BAR_WIDTH, height),
        );
        match bars[index] {
            Bar::Filled(color) => painter.rect_filled(bar, Rounding::same(BAR_WIDTH / 2.0), color),
            Bar::Hollow(color) => painter.rect_stroke(
                bar,
                Rounding::same(BAR_WIDTH / 2.0),
                Stroke::new(1.4, color),
            ),
        };
    }
    if appearance.failure == Some(FailureKind::InputUnavailable) {
        // The one mark that adds a stroke rather than recolouring. It never
        // displaces a bar; it crosses them, which is how a muted input reads
        // everywhere else on the system.
        painter.line_segment(
            [
                Pos2::new(rect.left() + 2.0, rect.bottom() - 2.0),
                Pos2::new(rect.right() - 2.0, rect.top() + 2.0),
            ],
            Stroke::new(2.4, theme::ERROR),
        );
    }
}

fn bars_for(appearance: &Appearance) -> [Bar; 4] {
    let Some(kind) = appearance.failure else {
        let color = match appearance.state {
            HudState::Listening => theme::LISTENING,
            HudState::Processing => theme::TRANSCRIBING,
            HudState::Done => theme::INSERTED,
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

/// Only listening and transcribing move, and only within the fixed box.
fn animated_height(
    appearance: &Appearance,
    index: usize,
    resting: f32,
    level: f32,
    time: f64,
) -> f32 {
    if appearance.failure.is_some() {
        return resting;
    }
    match appearance.state {
        HudState::Listening => (resting + level * 0.9).clamp(0.18, 1.0),
        HudState::Processing => {
            let shimmer = (time as f32 * 2.0 + index as f32 * 0.7).sin() * 0.5 + 0.5;
            (resting * 0.7 + shimmer * 0.3).clamp(0.18, 1.0)
        }
        _ => resting,
    }
}
