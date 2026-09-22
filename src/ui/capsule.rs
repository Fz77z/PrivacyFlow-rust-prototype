use crate::state::{AppState, Failure, HudState};
use crate::ui::mark::{self, Appearance};
use crate::ui::theme;
use egui::{Color32, Pos2, Rect, Rounding, Sense, Stroke, Ui, Vec2};

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
/// `recording` is "the microphone is capturing right now". Transcribing and
/// the result that follows are shown at bead size, in the bars' colour, so
/// the capsule shrinks back the moment the user lets go.
pub fn size_for(minimal: bool, pointing: bool, recording: bool) -> CapsuleSize {
    if !minimal {
        return CapsuleSize::Full;
    }
    if pointing {
        return CapsuleSize::Full;
    }
    if recording {
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

/// Paints the capsule into the middle of its window.
///
/// `painted` is the size to draw at, which is animated and so is usually
/// between the three fixed sizes. The layout is chosen from the size actually
/// being drawn rather than the one being animated towards, so a half grown
/// capsule never paints a label into a shape too small to hold it.
///
/// `presence` is how solid the whole capsule is drawn, from 0 to 1. The bead
/// at rest is drawn faint, so it stays out of the way until the user acts.
pub fn show(
    ui: &mut Ui,
    state: &AppState,
    time: f64,
    painted: Vec2,
    presence: f32,
) -> CapsuleResponse {
    // The capsule is centred in its window rather than filling it, and it
    // never quite fills it: the outline is drawn outside the shape, so the
    // shape has to leave the outline somewhere to go. With minimal mode on
    // there is a whole catchment of room; with it off the window is exactly
    // the capsule, and without this inset the outline would be clipped away
    // and the default configuration would have no border at all.
    //
    // Insetting the painted shape rather than inflating the window is what
    // keeps the window exactly the capsule when minimal mode is off. An
    // inflated window would stop being the thing the drag conversion divides
    // by, and would put a click-swallowing ring around the default capsule.
    let room = ui.max_rect();
    let limit = room.size() - Vec2::splat(EDGE_WIDTH * 2.0);
    let painted = Vec2::new(painted.x.min(limit.x), painted.y.min(limit.y));
    let rect = Rect::from_center_size(room.center(), painted);
    // Clipped a little wider than the capsule, because the outline sits
    // outside it and would otherwise be cut off at the very edge.
    let mut painter = ui.painter_at(rect.expand(EDGE_WIDTH * 2.0));
    painter.set_opacity(presence);
    let failure = failure_for(state);
    // Minimal mode has no outline: the capsule is a plain shape on its
    // shadow. An unread failure is carried by the menu bar icon, which is
    // still there when the capsule has hidden.
    let outline = (!state.settings.minimal_mode).then(|| border_for(state, failure.is_some()));
    // All three sizes are pills, so the radius is half the height at every
    // point of the animation. Interpolating between three stored radii would
    // be a second thing that has to agree with the first.
    let radius = rect.height() / 2.0;
    // A soft shadow lifts the capsule off whatever is behind it. Only drawn where the window has room for
    // it: with minimal mode off the window is exactly the capsule, and a
    // shadow cut off square at the window's edge would outline the very
    // rectangle the transparent window exists to hide.
    let shadow_room = room.shrink(SHADOW.blur / 2.0 + SHADOW.offset.y.abs());
    if shadow_room.contains_rect(rect) {
        let mut shadow_painter = ui.painter_at(room);
        shadow_painter.set_opacity(presence);
        shadow_painter.add(SHADOW.as_shape(rect, Rounding::same(radius)));
    }
    // The outline sits outside the fill rather than inside it. Stroking the
    // shape itself puts the line within the capsule, which at bead size eats
    // a visible share of a small shape and reads as a shrunken inner ring.
    //
    // It is painted as a solid pill with the fill laid on top, not as a
    // stroke beside the fill. A stroke and a fill that meet at an edge each
    // anti-alias their half of it at partial coverage, and the desktop shows
    // through the seam as a light hairline that looks jagged around the
    // curves at 1x. Laid on top, the fill's soft edge blends into the
    // outline instead, and only the outer edge ever meets the desktop.
    //
    // The outline's rounding has to grow by the same amount its rect was
    // expanded by. Inheriting the shape's radius leaves the two curves
    // non-concentric: they stay together along the straight edges and open a
    // gap at the rounded ends, which is exactly where this capsule is all
    // curve.
    if let Some(outline) = outline {
        painter.rect_filled(
            rect.expand(EDGE_WIDTH),
            Rounding::same(radius + EDGE_WIDTH),
            outline,
        );
    }
    painter.rect_filled(rect, Rounding::same(radius), theme::FILL);

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
        &Appearance {
            state: state.hud,
            failure: failure.map(|f| f.kind),
            settled_for: state.done_at.map(|at| at.elapsed().as_secs_f32()),
        },
        state.voice_bars,
        time,
    );
    if to_full > 0.0 {
        paint_label_and_icon(ui, &painter, rect, mark_rect, state, to_full, &mut action);
    }

    body.context_menu(|ui| menu(ui, &mut action));
    CapsuleResponse { action, dragging: body.dragged() || body.drag_started() }
}

/// The cog, which is the console icon. One stroke weight throughout, kept
/// thin: the glyph is small and a heavy line turns it into a drawing of a
/// cog rather than an icon of one.
const COG_STROKE: f32 = 1.1;
const COG_RING: f32 = 5.4;
const COG_HUB: f32 = 1.9;
const COG_TOOTH_TIP: f32 = 8.0;
const COG_TEETH: usize = 8;

/// The capsule's outline, which is drawn outside the shape.
const EDGE_WIDTH: f32 = 1.0;

/// Soft and close, so it reads as the capsule resting just above the desktop
/// rather than floating far off it.
const SHADOW: egui::epaint::Shadow = egui::epaint::Shadow {
    offset: Vec2::new(0.0, 3.0),
    blur: 14.0,
    spread: 0.0,
    color: Color32::from_black_alpha(110),
};

/// The smallest gap kept between the mark and the capsule's edge, so the
/// mark never touches the border even when it has to be clamped to fit.
const MARK_INSET: f32 = 3.0;

/// How far `value` has travelled from `from` to `to`, clamped to 0 and 1.
fn progress(value: f32, from: f32, to: f32) -> f32 {
    ((value - from) / (to - from)).clamp(0.0, 1.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Where the mark goes, for a capsule painted at any size between the three.
///
/// The mark scales with the capsule's height and slides from centred to the
/// left as the capsule opens and the label makes room for itself. Both are
/// continuous,
/// because the capsule is animated and anything that changes in one step
/// during that animation reads as a snap, which is exactly what an earlier
/// arrangement-swapping version did.
///
/// Split out from the painting so the geometry can be tested at every size
/// the animation passes through, rather than only at the three it rests at.
fn mark_rect_for(rect: Rect) -> Rect {
    let to_full = progress(rect.width(), theme::ACTIVE_SIZE.x, theme::CAPSULE_SIZE.x);
    // The mark scales with the capsule's height, so the widget keeps its
    // proportions at every point of the animation rather than holding a fixed
    // mark inside a changing shell.
    let scale = rect.height() / theme::CAPSULE_SIZE.y;
    // Clamped to fit whatever it is being painted into, so the function is
    // total. Nothing clamps it at the three resting sizes, which are all
    // built around this mark, but a geometry function that is only correct
    // for the inputs it happens to be given is a trap for whoever changes the
    // animation next.
    let room = rect.shrink(MARK_INSET);
    let size = Vec2::new(
        (theme::MARK_SIZE.x * scale).min(room.width()),
        (theme::MARK_SIZE.y * scale).min(room.height()),
    );
    let centre_x = lerp(
        rect.center().x,
        rect.left() + theme::PAD_LEFT + size.x / 2.0,
        to_full,
    )
    .clamp(room.left() + size.x / 2.0, room.right() - size.x / 2.0);
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
    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.right() - theme::PAD_RIGHT - theme::ICON_SIZE / 2.0, rect.center().y),
        Vec2::splat(theme::ICON_SIZE),
    );
    let (text, font, color) = match failure {
        Some(failure) => (failure.headline, theme::error_font(), theme::ERROR_TEXT),
        None => {
            let (label, color) = hud_label(state.hud, state.processing_label());
            (label, theme::label_font(), color)
        }
    };
    // Cut short with an ellipsis rather than allowed to run on. Failure
    // headlines are written wherever the failure happens, several are wider
    // than the gap, and a label left to run on is painted straight through
    // the cog. The whole message is on the cog's tooltip and in the console.
    let label = fitted_label(ui, text, font, label_room());
    painter.galley(
        Pos2::new(text_left, rect.center().y - label.size().y / 2.0),
        label,
        fade(color),
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


/// What the capsule says in each healthy state, and how loudly.
fn hud_label(hud: HudState, processing: &'static str) -> (&'static str, Color32) {
    match hud {
        HudState::Idle => ("Ready", theme::MUTED),
        HudState::Listening => ("Listening", theme::LABEL),
        HudState::Processing => (processing, theme::LABEL),
        HudState::Done => ("Inserted", theme::LABEL),
        HudState::Copied => ("Copied", theme::LABEL),
        // Muted, like Ready: nothing was said, so nothing happened, and the
        // capsule should not announce it as though it had.
        HudState::NoSpeech => ("No speech", theme::MUTED),
        // Not muted, unlike silence: the user spoke and their words were
        // discarded, which they are entitled to notice.
        HudState::NotUnderstood => ("Didn't catch that", theme::LABEL),
        HudState::Error => ("Ready", theme::MUTED),
    }
}

/// The width the label has between the mark and the cog, in the full
/// capsule. Measured at full size rather than at whatever size is being
/// painted: while the capsule grows the label is still fading in, and fitting
/// it to the half-grown gap cut every word down to a lone ellipsis.
fn label_room() -> f32 {
    let capsule = Rect::from_center_size(Pos2::ZERO, theme::CAPSULE_SIZE);
    let mark = mark_rect_for(capsule);
    let icon_left = capsule.right() - theme::PAD_RIGHT - theme::ICON_SIZE;
    icon_left - (mark.right() + theme::MARK_GAP)
}

/// Lays `text` out on one line no wider than `room`, ending in an ellipsis if
/// it has to be cut. The colour is left to the painter, so the fade applies.
fn fitted_label(ui: &Ui, text: &str, font: egui::FontId, room: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat { font_id: font, color: Color32::PLACEHOLDER, ..Default::default() },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: room,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|fonts| fonts.layout_job(job))
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
    if ui.button("Quit PrivacyFlow").clicked() {
        *action = Some(CapsuleAction::Quit);
        ui.close_menu();
    }
}

/// A cog, thinly drawn.
///
/// It used to be a window with a rule through it, chosen deliberately so the
/// icon promised "this opens the other window" rather than promising settings
/// that did not exist. They exist now, and the console's middle tab is where
/// they live, so a cog is the honest icon rather than the misleading one.
///
/// Built from three shapes at one stroke weight: a ring, eight teeth standing
/// on it, and a hub. Thin on purpose. At 36 points the heavy 1.5pt outline
/// the old glyph used read as a drawn box rather than as an icon.
fn paint_console_glyph(painter: &egui::Painter, rect: Rect, tint: Color32) {
    let centre = rect.center();
    let stroke = Stroke::new(COG_STROKE, tint);
    painter.circle_stroke(centre, COG_RING, stroke);
    painter.circle_stroke(centre, COG_HUB, stroke);
    for tooth in 0..COG_TEETH {
        // Offset by half a step so no tooth sits on the vertical, which reads
        // as an arrow rather than as part of a ring.
        let angle = std::f32::consts::TAU * (tooth as f32 + 0.5) / COG_TEETH as f32;
        let (sin, cos) = angle.sin_cos();
        let direction = Vec2::new(cos, sin);
        painter.line_segment(
            [
                centre + direction * (COG_RING - COG_STROKE / 2.0),
                centre + direction * COG_TOOTH_TIP,
            ],
            stroke,
        );
    }
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
            for recording in [false, true] {
                assert_eq!(size_for(false, pointing, recording), CapsuleSize::Full);
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


    /// The capsule is animated, so it is drawn at hundreds of sizes between
    /// the three it rests at, and an animation interrupted part way leaves
    /// the width and the height at different points of their travel. The
    /// mark has to stay inside it at every combination: a mark that overhangs
    /// is the snap this geometry exists to remove, wearing a different shape.
    #[test]
    fn the_mark_stays_inside_the_capsule_at_every_size_of_the_animation() {
        let mut width = theme::BEAD_SIZE.x;
        while width <= theme::CAPSULE_SIZE.x {
            let mut height = theme::BEAD_SIZE.y;
            while height <= theme::CAPSULE_SIZE.y {
                let capsule =
                    Rect::from_center_size(Pos2::new(500.0, 500.0), Vec2::new(width, height));
                let mark = mark_rect_for(capsule);
                assert!(
                    capsule.contains_rect(mark),
                    "at {width} by {height} the mark {mark:?} escaped the capsule {capsule:?}"
                );
                height += 2.0;
            }
            width += 2.0;
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

    /// Every label the capsule shows in a healthy state has to fit whole
    /// between the mark and the cog, measured in the real font. Failure
    /// headlines are cut short with an ellipsis if they do not, but these are
    /// the words the user reads on every dictation, and "Didn't catch that"
    /// clipped to "Didn't catch th…" would be a defect. Measured rather than
    /// budgeted, so a larger label font or a longer word fails here.
    #[test]
    fn every_healthy_label_fits_the_full_capsule_whole() {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        // Fonts only exist once a frame has run.
        let _ = ctx.run(Default::default(), |_| {});
        let room = label_room();
        let states = [
            HudState::Idle,
            HudState::Listening,
            HudState::Processing,
            HudState::Done,
            HudState::Copied,
            HudState::NoSpeech,
            HudState::NotUnderstood,
        ];
        for processing in ["Loading models", "Transcribing"] {
            for hud in states {
                let (label, _) = hud_label(hud, processing);
                let width = ctx.fonts(|fonts| {
                    fonts.layout_no_wrap(label.to_owned(), theme::label_font(), Color32::WHITE)
                        .size()
                        .x
                });
                assert!(width <= room, "\"{label}\" is {width} wide, with {room} to fit in");
            }
        }
    }

    /// At rest it is centred, which is what makes the bead look like a bead
    /// rather than like a capsule with its contents pushed to one side.
    #[test]
    fn the_mark_is_centred_in_the_bead() {
        let bead = Rect::from_center_size(Pos2::new(500.0, 500.0), theme::BEAD_SIZE);
        let mark = mark_rect_for(bead);
        assert_eq!(mark.center().x, bead.center().x);
        let expected = theme::MARK_SIZE * (theme::BEAD_SIZE.y / theme::CAPSULE_SIZE.y);
        assert!(
            (mark.size() - expected).length() < 1e-3,
            "the mark scales with the capsule, so the widget keeps its proportions: \
             {:?} against {expected:?}",
            mark.size()
        );
    }

    /// Largest claim wins. Pointing at the capsule during a dictation must
    /// show the full capsule, not the smaller dictating one, because the
    /// reason to point at it is to read it.
    #[test]
    fn pointing_at_a_dictating_capsule_shows_the_whole_thing() {
        assert_eq!(size_for(true, true, true), CapsuleSize::Full);
    }
}
