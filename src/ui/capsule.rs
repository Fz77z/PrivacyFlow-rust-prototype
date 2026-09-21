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

    pub fn radius(self) -> f32 {
        match self {
            CapsuleSize::Bead => theme::BEAD_RADIUS,
            CapsuleSize::Active => theme::ACTIVE_RADIUS,
            CapsuleSize::Full => theme::CAPSULE_RADIUS,
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
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    time: f64,
    size: CapsuleSize,
    opacity: f32,
) -> CapsuleResponse {
    let rect = Rect::from_min_size(ui.max_rect().min, size.points());
    let painter = ui.painter_at(rect);
    let failure = failure_for(state);
    // An unread failure keeps the bead tinted after the dictating capsule has
    // retired. Without that, a failure raised while the user was typing
    // elsewhere would have nowhere to show: the bead has no console icon, and
    // so nowhere to put the unread dot. Chosen here, before the one shell
    // paint, rather than in a second paint over the shell: two FILL paints at
    // less than full opacity would composite to a visibly more opaque bead
    // than the fade asks for, and the bead's tint would blend with the
    // ordinary border underneath it instead of replacing it.
    let border = if size == CapsuleSize::Bead && state.unread_failure {
        theme::ERROR
    } else {
        border_for(state, failure.is_some())
    };
    // Everything is painted through this, so the proximity fade is one
    // multiplication rather than an alpha threaded through every call.
    let fade = |color: Color32| color.gamma_multiply(opacity);
    painter.rect(
        rect.shrink(0.5),
        Rounding::same(size.radius()),
        fade(theme::FILL),
        Stroke::new(1.0, fade(border)),
    );

    // The body is everything the icon does not claim, so dragging the widget
    // works anywhere the user naturally grabs it.
    let body = ui.interact(rect, ui.id().with("capsule"), Sense::click_and_drag());
    let settled = drag_window(ui, &body);
    let mut action = settled.map(CapsuleAction::Moved);

    match size {
        // The shell above is the whole bead; there is nothing left to paint.
        CapsuleSize::Bead => {}
        CapsuleSize::Active => paint_active(&painter, rect, state, time, opacity),
        CapsuleSize::Full => paint_full(ui, &painter, rect, state, time, opacity, &mut action),
    }

    body.context_menu(|ui| menu(ui, &mut action));
    CapsuleResponse { action, dragging: body.dragged() || body.drag_started() }
}

/// The full capsule: mark, label, and the console icon with its unread dot.
/// This is today's whole widget, moved here unchanged so `show` can also
/// paint the two smaller sizes.
fn paint_full(
    ui: &mut Ui,
    painter: &egui::Painter,
    rect: Rect,
    state: &AppState,
    time: f64,
    opacity: f32,
    action: &mut Option<CapsuleAction>,
) {
    let failure = failure_for(state);
    let fade = |color: Color32| color.gamma_multiply(opacity);
    let mark_rect = Rect::from_min_size(
        Pos2::new(rect.left() + theme::PAD_LEFT, rect.center().y - theme::MARK_SIZE.y / 2.0),
        theme::MARK_SIZE,
    );
    mark::paint(
        painter,
        mark_rect,
        &Appearance { state: state.hud, failure: failure.map(|f| f.kind) },
        state.mic_level,
        time,
        opacity,
    );

    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.right() - theme::PAD_RIGHT - theme::ICON_SIZE / 2.0, rect.center().y),
        Vec2::splat(theme::ICON_SIZE),
    );

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

    let icon = ui.interact(icon_rect, ui.id().with("console"), Sense::click());
    // Hover text comes from the last failure rather than from the capsule's
    // current state: the red dot outlives the capsule's return to Ready, and
    // an icon that still says something went wrong must still say what.
    let icon = match &state.last_failure {
        Some(failure) => icon.on_hover_text(&failure.detail),
        None => icon.on_hover_text("Open console"),
    };
    if icon.hovered() {
        painter.rect_filled(
            icon_rect,
            Rounding::same(theme::ICON_RADIUS),
            fade(theme::ICON_HOVER),
        );
    }
    let tint = if failure.is_some() || state.unread_failure {
        theme::ICON_ALERT
    } else {
        theme::ICON_TINT
    };
    paint_console_glyph(painter, icon_rect, fade(tint));
    if state.unread_failure {
        // Survives the capsule returning to Ready, so a failure that happened
        // while the user was typing elsewhere is still there to be found.
        let dot = Pos2::new(icon_rect.right() - 8.0, icon_rect.top() + 8.0);
        painter.circle_filled(dot, 4.5, fade(theme::FILL));
        painter.circle_filled(dot, 3.5, fade(theme::UNREAD_DOT));
    }

    if icon.clicked() {
        *action = Some(CapsuleAction::ToggleConsole);
    }
    // The icon wins the overlap with the body, so without its own menu the
    // 36x36 square would be a dead zone for right-clicks. The spec asks for
    // the menu anywhere on the capsule.
    icon.context_menu(|ui| menu(ui, action));
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
fn paint_active(painter: &egui::Painter, rect: Rect, state: &AppState, time: f64, opacity: f32) {
    let failure = failure_for(state);
    let mark_rect = Rect::from_center_size(rect.center(), Vec2::new(22.0, 18.0));
    mark::paint(
        painter,
        mark_rect,
        &Appearance { state: state.hud, failure: failure.map(|f| f.kind) },
        state.mic_level,
        time,
        opacity,
    );
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

    /// Largest claim wins. Pointing at the capsule during a dictation must
    /// show the full capsule, not the smaller dictating one, because the
    /// reason to point at it is to read it.
    #[test]
    fn pointing_at_a_dictating_capsule_shows_the_whole_thing() {
        assert_eq!(size_for(true, true, true), CapsuleSize::Full);
    }
}
