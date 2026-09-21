use crate::state::{AppState, Failure, HudState};
use crate::ui::mark::{self, Appearance};
use crate::ui::theme;
use egui::{Align2, Color32, Pos2, Rect, Rounding, Sense, Stroke, Ui, Vec2};

pub enum CapsuleAction {
    ToggleConsole,
    OpenConsole,
    Quit,
    /// The capsule was dragged and let go here. Emitted on release rather
    /// than continuously, so settling it in a new place costs one write
    /// instead of one per frame.
    Moved(Pos2),
}

/// Which of the three shapes the capsule is wearing.
///
/// The names say what claims each one, not how big it is: the user is
/// pointing at it, the user is dictating, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsuleSize {
    Bead,
    Active,
    Full,
}

impl CapsuleSize {
    pub fn points(self) -> Vec2 {
        match self {
            CapsuleSize::Bead => theme::BEAD_SIZE,
            CapsuleSize::Active => theme::ACTIVE_SIZE,
            CapsuleSize::Full => theme::CAPSULE_SIZE,
        }
    }


}

/// What the capsule is entitled to be right now.
///
/// Every input is something the user is doing, which is the governing rule
/// expressed as a signature: the application never changes the capsule's
/// shape on its own, and there is no argument here through which it could.
///
/// `active` is "a dictation has begun and its result has not yet retired",
/// which the hud state already answers.
pub fn size_for(minimal: bool, pointing: bool, active: bool) -> CapsuleSize {
    if !minimal {
        return CapsuleSize::Full;
    }
    if pointing {
        return CapsuleSize::Full;
    }
    if active {
        return CapsuleSize::Active;
    }
    CapsuleSize::Bead
}

/// What happened this frame, and whether the widget is being dragged.
///
/// `dragging` is reported separately from `action` because a drag in
/// progress has no `Moved` action yet (that lands on release), but Task 5
/// still needs to know about it: a fast drag can carry the pointer outside
/// the window for a frame, and the capsule must not shrink out from under
/// the user mid-drag just because `pointing` briefly reads false.
pub struct CapsuleResponse {
    pub action: Option<CapsuleAction>,
    pub dragging: bool,
}

/// The failure to show right now, if any. A failure is only shown while the
/// capsule is actually in the error state; once the hud moves on, the icon's
/// unread dot is what carries the failure forward, not this.
fn failure_for(state: &AppState) -> Option<&Failure> {
    state.last_failure.as_ref().filter(|_| state.hud == HudState::Error)
}

/// The one point border colour for the current state. Shared by the full
/// capsule's shell and the bead, so the two never drift apart on what each
/// state means.
fn border_for(state: &AppState, has_failure: bool) -> Color32 {
    match (has_failure, state.hud) {
        (true, _) => theme::ERROR_BORDER,
        (false, HudState::Listening) => theme::BORDER_LISTENING,
        (false, HudState::Processing) => theme::BORDER_TRANSCRIBING,
        (false, HudState::Done) => theme::BORDER_INSERTED,
        (false, HudState::Copied) => theme::BORDER_INSERTED,
        _ => theme::BORDER,
    }
}

/// Draws the whole widget. The capsule is the window, so this paints every
/// pixel the user sees: there is no title bar above it.
/// Paints the capsule into the middle of the catchment window.
///
/// `painted` is the size to draw at, which is animated and so is usually
/// between the three fixed sizes. `layout` is which of the three arrangements
/// to draw, chosen from the size actually being drawn rather than the one
/// being animated towards, so a half grown capsule never paints a label into
/// a shape too small to hold it.
pub fn show(ui: &mut Ui, state: &AppState, time: f64, painted: Vec2) -> CapsuleResponse {
    // The window is the catchment and never changes size while the capsule
    // animates, so the capsule is centred inside it rather than filling it.
    let rect = Rect::from_center_size(ui.max_rect().center(), painted);
    let painter = ui.painter_at(rect);
    let failure = failure_for(state);
    // An unread failure tints the bead, which is the only way a failure
    // raised while the user was typing elsewhere can still be seen once the
    // capsule has shrunk: there is no console icon at that size, and so
    // nowhere to put the unread dot. Chosen here, before the one shell paint,
    // rather than painted over it: two fills would composite to something
    // neither colour asked for.
    let small = painted.x < theme::ACTIVE_SIZE.x;
    let border = if small && state.unread_failure {
        theme::ERROR
    } else {
        border_for(state, failure.is_some())
    };
    // All three sizes are pills, so the radius is half the height at every
    // point of the animation. Interpolating between three stored radii would
    // be a second thing that has to agree with the first.
    painter.rect(
        rect.shrink(0.5),
        Rounding::same(rect.height() / 2.0),
        theme::FILL,
        Stroke::new(1.0, border),
    );

    // The body is everything the icon does not claim, so dragging the widget
    // works anywhere the user naturally grabs it.
    let body = ui.interact(rect, ui.id().with("capsule"), Sense::click_and_drag());
    let settled = drag_window(ui, &body);
    let mut action = settled.map(CapsuleAction::Moved);

    // How far the capsule has grown towards its full width. The mark slides
    // and scales across this, and the label and icon fade in over it, so the
    // contents flow with the shape instead of switching arrangement the
    // instant it is wide enough. That switch is what made the bars appear to
    // snap to the left as the capsule opened.
    let to_full = progress(painted.x, theme::ACTIVE_SIZE.x, theme::CAPSULE_SIZE.x);
    let mark_rect = mark_rect_for(rect);
    mark::paint(
        &painter,
        mark_rect,
        &Appearance { state: state.hud, failure: failure.map(|f| f.kind) },
        state.mic_level,
        time,
    );
    if to_full > 0.0 {
        paint_label_and_icon(ui, &painter, rect, mark_rect, state, to_full, &mut action);
    }

    body.context_menu(|ui| menu(ui, &mut action));
    CapsuleResponse { action, dragging: body.dragged() || body.drag_started() }
}


/// The dictating capsule. Only the mark: at 84 by 28 there is no room for the
/// label, and the mark is the part that has to stay legible while someone is
/// actually speaking.
///
/// The mark box is 22 by 18, not a uniform scale of the 36 by 30 box the mark
/// was designed against: a uniform scale that fits the 28pt capsule height
/// would also widen the bars past what the height leaves room for. Bar width
/// against the tallest bar's height is about 5:1 at 36x30 and about 4.9:1 at
/// 22x18, which keeps the silhouette; 22 is close to the largest width a
/// uniform scale of the 28pt capsule height allows, leaving 5pt above and
/// below, and the last bar's right edge lands at 19.60 inside 22.0, so
/// nothing clips.
/// How far `value` has travelled from `from` to `to`, clamped to 0 and 1.
fn progress(value: f32, from: f32, to: f32) -> f32 {
    ((value - from) / (to - from)).clamp(0.0, 1.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Where the mark goes, for a capsule painted at any size between the three.
///
/// Two things move at once. The mark grows through the three mark sizes, and
/// it slides from the middle of the capsule to the left as the label makes
/// room for itself. Both are continuous, because the capsule is animated and
/// anything that changes in one step during that animation reads as a snap,
/// which is exactly what the earlier arrangement-swapping version did.
///
/// Split out from the painting so the geometry can be tested at every width
/// the animation passes through, rather than only at the three it rests at.
fn mark_rect_for(rect: Rect) -> Rect {
    let to_active = progress(rect.width(), theme::BEAD_SIZE.x, theme::ACTIVE_SIZE.x);
    let to_full = progress(rect.width(), theme::ACTIVE_SIZE.x, theme::CAPSULE_SIZE.x);
    let size = if to_full > 0.0 {
        Vec2::new(
            lerp(theme::ACTIVE_MARK_SIZE.x, theme::MARK_SIZE.x, to_full),
            lerp(theme::ACTIVE_MARK_SIZE.y, theme::MARK_SIZE.y, to_full),
        )
    } else {
        Vec2::new(
            lerp(theme::BEAD_MARK_SIZE.x, theme::ACTIVE_MARK_SIZE.x, to_active),
            lerp(theme::BEAD_MARK_SIZE.y, theme::ACTIVE_MARK_SIZE.y, to_active),
        )
    };
    let centre_x = lerp(
        rect.center().x,
        rect.left() + theme::PAD_LEFT + size.x / 2.0,
        to_full,
    );
    Rect::from_center_size(Pos2::new(centre_x, rect.center().y), size)
}

/// The label and the console icon, faded in as the capsule reaches full
/// width.
///
/// `appearing` runs from 0 to 1 across the last part of the growth. The icon
/// only becomes clickable once it has fully arrived: a half faded icon that
/// can be clicked is a target the user cannot see well enough to aim at.
fn paint_label_and_icon(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    mark_rect: Rect,
    state: &AppState,
    appearing: f32,
    action: &mut Option<CapsuleAction>,
) {
    let failure = failure_for(state);
    let fade = |color: Color32| color.gamma_multiply(appearing);
    let text_left = mark_rect.right() + theme::MARK_GAP;
    match failure {
        Some(failure) => painter.text(
            Pos2::new(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            failure.headline,
            theme::error_font(),
            fade(theme::ERROR_TEXT),
        ),
        None => {
            let (label, color) = match state.hud {
                HudState::Idle => ("Ready", theme::MUTED),
                HudState::Listening => ("Listening", theme::LABEL),
                HudState::Processing => (state.processing_label(), theme::LABEL),
                HudState::Done => ("Inserted", theme::LABEL),
                HudState::Copied => ("Copied", theme::LABEL),
                // Muted, like Ready: nothing was said, so nothing happened,
                // and the capsule should not announce it as though it had.
                HudState::NoSpeech => ("No speech", theme::MUTED),
                HudState::Error => ("Ready", theme::MUTED),
            };
            painter.text(
                Pos2::new(text_left, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                theme::label_font(),
                fade(color),
            )
        }
    };

    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.right() - theme::PAD_RIGHT - theme::ICON_SIZE / 2.0, rect.center().y),
        Vec2::splat(theme::ICON_SIZE),
    );
    let tint = if failure.is_some() || state.unread_failure {
        theme::ICON_ALERT
    } else {
        theme::ICON_TINT
    };
    if appearing < 1.0 {
        paint_console_glyph(painter, icon_rect, fade(tint));
        return;
    }

    let icon = ui.interact(icon_rect, ui.id().with("console"), Sense::click());
    // Hover text comes from the last failure rather than from the capsule's
    // current state: the red dot outlives the capsule's return to Ready, and
    // an icon that still says something went wrong must still say what.
    let icon = match &state.last_failure {
        Some(failure) => icon.on_hover_text(&failure.detail),
        None => icon.on_hover_text("Open console"),
    };
    if icon.hovered() {
        painter.rect_filled(icon_rect, Rounding::same(theme::ICON_RADIUS), theme::ICON_HOVER);
    }
    paint_console_glyph(painter, icon_rect, tint);
    if state.unread_failure {
        // Survives the capsule returning to Ready, so a failure that happened
        // while the user was typing elsewhere is still there to be found.
        let dot = Pos2::new(icon_rect.right() - 8.0, icon_rect.top() + 8.0);
        painter.circle_filled(dot, 4.5, theme::FILL);
        painter.circle_filled(dot, 3.5, theme::UNREAD_DOT);
    }

    if icon.clicked() {
        *action = Some(CapsuleAction::ToggleConsole);
    }
    // The icon wins the overlap with the body, so without its own menu the
    // 36x36 square would be a dead zone for right-clicks. The spec asks for
    // the menu anywhere on the capsule.
    icon.context_menu(|ui| menu(ui, action));
}


/// Move the window with the pointer, rather than asking macOS to run a drag.
///
/// `ViewportCommand::StartDrag` hands off to AppKit's own window drag, which
/// does not start for a window that refuses to become key. Since the capsule
/// refuses on purpose, so that clicking it never takes focus from whatever is
/// being written in, the drag is done here instead: egui already reports the
/// pointer movement, so the position is ours to set.
///
/// The pointer is measured in screen coordinates rather than through egui's
/// drag delta. egui reports movement relative to the window, so moving the
/// window changes the next reading: the capsule chases a number it is itself
/// perturbing, which shows up as jitter and lag. The screen position owes
/// nothing to any window, so the arithmetic is absolute and the capsule sits
/// exactly where it was grabbed.
fn drag_window(ui: &Ui, body: &egui::Response) -> Option<Pos2> {
    let anchor_id = ui.id().with("drag_anchor");
    if body.dragged() || body.drag_started() {
        // A drag is driven by pointer movement, and egui otherwise only wakes
        // for events. Asking for the next frame keeps the capsule tracking at
        // the display's rate rather than in steps.
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        ui.ctx().request_repaint();
    }
    if body.drag_started() {
        let window = ui.ctx().input(|i| i.viewport().outer_rect.map(|rect| rect.min));
        match window {
            Some(window) => {
                let (pointer_x, pointer_y) = crate::platform::pointer_in_window_space();
                ui.ctx().memory_mut(|memory| {
                    memory.data.insert_temp(
                        anchor_id,
                        DragAnchor { window, pointer_x, pointer_y, settled: window },
                    )
                });
            }
            // Without a window rectangle there is nothing to measure from, so
            // this drag has no anchor. The previous drag's anchor must go with
            // it: left in place, the next frame would read it and jump the
            // capsule to a position computed against an origin that has since
            // moved.
            None => ui
                .ctx()
                .memory_mut(|memory| memory.data.remove::<DragAnchor>(anchor_id)),
        }
    }
    let anchor = ui
        .ctx()
        .memory_mut(|memory| memory.data.get_temp::<DragAnchor>(anchor_id));
    let anchor = anchor?;
    if body.drag_stopped() {
        ui.ctx()
            .memory_mut(|memory| memory.data.remove::<DragAnchor>(anchor_id));
        return Some(anchor.settled);
    }
    if !body.dragged() {
        return None;
    }
    let (pointer_x, pointer_y) = crate::platform::pointer_in_window_space();
    // Both terms are differences from the anchor, so the shared origin cancels
    // and this is correct for displays left of or above the main one, and
    // across a drag between displays of different scale factors. The vertical
    // term is no longer negated here: `pointer_in_window_space` already
    // converts Cocoa's upward-y into the same downward-y egui uses, so both
    // sides of the subtraction share one convention.
    // It does assume egui's zoom factor is 1.0: the anchor is in egui points
    // and the pointer delta is in raw Cocoa points, so a zoom would track at
    // the wrong rate. Left as an assumption rather than handled, because
    // nothing sets zoom and a window that refuses to become key cannot receive
    // the keyboard shortcut that would change it.
    // Rounded to whole points. A window placed on a fraction of a point is
    // resampled by the compositor, which softens the capsule's edge and its
    // one point border while it moves.
    let moved = Pos2::new(
        (anchor.window.x + (pointer_x - anchor.pointer_x) as f32).round(),
        (anchor.window.y + (pointer_y - anchor.pointer_y) as f32).round(),
    );
    ui.ctx().memory_mut(|memory| {
        memory.data.insert_temp(anchor_id, DragAnchor { settled: moved, ..anchor })
    });
    ui.ctx()
        .send_viewport_cmd(egui::ViewportCommand::OuterPosition(moved));
    None
}

/// Where the window was, and where the pointer was, at the moment the drag
/// began. Everything after that is measured from here rather than
/// accumulated, so a long drag cannot drift.
#[derive(Clone, Copy)]
struct DragAnchor {
    window: Pos2,
    pointer_x: f64,
    pointer_y: f64,
    /// Where the capsule was last put, carried so the release can report it
    /// without reading back a window rectangle that lags behind.
    settled: Pos2,
}

fn menu(ui: &mut Ui, action: &mut Option<CapsuleAction>) {
    if ui.button("Open console").clicked() {
        *action = Some(CapsuleAction::OpenConsole);
        ui.close_menu();
    }
    if ui.button("Quit LocalFlow").clicked() {
        *action = Some(CapsuleAction::Quit);
        ui.close_menu();
    }
}

/// A window with a title bar and two lines of text: the icon says "opens the
/// other window" rather than promising settings that do not exist.
fn paint_console_glyph(painter: &egui::Painter, rect: Rect, tint: Color32) {
    // Two shapes, not four. An earlier version drew a title rule and two
    // content lines inside the same box, and at this size two one point
    // lines three points apart do not read as two lines, they read as a
    // grey smudge. One centred rule carries the same meaning legibly.
    let glyph = Rect::from_center_size(rect.center(), Vec2::new(19.0, 15.0));
    let stroke = Stroke::new(1.5, tint);
    painter.rect_stroke(glyph, Rounding::same(4.5), stroke);
    let rule_y = glyph.center().y;
    let inset = 4.5;
    painter.line_segment(
        [
            Pos2::new(glyph.left() + inset, rule_y),
            Pos2::new(glyph.right() - inset, rule_y),
        ],
        stroke,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal mode off is today's behaviour, and today's behaviour is one
    /// size. Nothing the user does may shrink a capsule they did not ask to
    /// shrink.
    #[test]
    fn with_minimal_mode_off_the_capsule_is_always_full_size() {
        for pointing in [false, true] {
            for active in [false, true] {
                assert_eq!(size_for(false, pointing, active), CapsuleSize::Full);
            }
        }
    }

    /// The three sizes, each claimed by one thing the user is doing.
    #[test]
    fn minimal_mode_rests_as_a_bead_and_grows_when_the_user_acts() {
        assert_eq!(size_for(true, false, false), CapsuleSize::Bead);
        assert_eq!(size_for(true, false, true), CapsuleSize::Active);
        assert_eq!(size_for(true, true, false), CapsuleSize::Full);
    }


    /// The capsule is animated, so it is drawn at hundreds of widths between
    /// the three it rests at. The mark has to stay inside it at every one of
    /// them: a mark that overhangs is the snap this geometry exists to
    /// remove, wearing a different shape.
    #[test]
    fn the_mark_stays_inside_the_capsule_at_every_width_of_the_animation() {
        let mut width = theme::BEAD_SIZE.x;
        while width <= theme::CAPSULE_SIZE.x {
            let height = theme::BEAD_SIZE.y
                + (theme::CAPSULE_SIZE.y - theme::BEAD_SIZE.y)
                    * (width - theme::BEAD_SIZE.x)
                    / (theme::CAPSULE_SIZE.x - theme::BEAD_SIZE.x);
            let capsule =
                Rect::from_center_size(Pos2::new(500.0, 500.0), Vec2::new(width, height));
            let mark = mark_rect_for(capsule);
            assert!(
                capsule.contains_rect(mark),
                "at {width} by {height} the mark {mark:?} escaped the capsule {capsule:?}"
            );
            width += 0.5;
        }
    }

    /// And it has to arrive where the full capsule's layout expects it,
    /// or the label it makes room for is spaced against the wrong edge.
    #[test]
    fn the_mark_lands_where_the_full_capsule_wants_it() {
        let capsule = Rect::from_center_size(Pos2::new(500.0, 500.0), theme::CAPSULE_SIZE);
        let mark = mark_rect_for(capsule);
        assert_eq!(mark.left(), capsule.left() + theme::PAD_LEFT);
        assert_eq!(mark.size(), theme::MARK_SIZE);
    }

    /// At rest it is centred, which is what makes the bead look like a bead
    /// rather than like a capsule with its contents pushed to one side.
    #[test]
    fn the_mark_is_centred_in_the_bead() {
        let bead = Rect::from_center_size(Pos2::new(500.0, 500.0), theme::BEAD_SIZE);
        let mark = mark_rect_for(bead);
        assert_eq!(mark.center().x, bead.center().x);
        assert_eq!(mark.size(), theme::BEAD_MARK_SIZE);
    }

    /// Largest claim wins. Pointing at the capsule during a dictation must
    /// show the full capsule, not the smaller dictating one, because the
    /// reason to point at it is to read it.
    #[test]
    fn pointing_at_a_dictating_capsule_shows_the_whole_thing() {
        assert_eq!(size_for(true, true, true), CapsuleSize::Full);
    }
}
