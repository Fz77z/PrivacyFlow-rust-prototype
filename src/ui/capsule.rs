use crate::state::{AppState, HudState};
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

/// Draws the whole widget. The capsule is the window, so this paints every
/// pixel the user sees: there is no title bar above it.
pub fn show(ui: &mut Ui, state: &AppState, time: f64) -> Option<CapsuleAction> {
    let rect = Rect::from_min_size(ui.max_rect().min, theme::CAPSULE_SIZE);
    let painter = ui.painter_at(rect);
    let failure = state.last_failure.as_ref().filter(|_| state.hud == HudState::Error);
    let border = match (failure.is_some(), state.hud) {
        (true, _) => theme::ERROR_BORDER,
        (false, HudState::Listening) => theme::BORDER_LISTENING,
        (false, HudState::Processing) => theme::BORDER_TRANSCRIBING,
        (false, HudState::Done) => theme::BORDER_INSERTED,
        (false, HudState::Copied) => theme::BORDER_INSERTED,
        _ => theme::BORDER,
    };
    painter.rect(
        rect.shrink(0.5),
        Rounding::same(theme::CAPSULE_RADIUS),
        theme::FILL,
        Stroke::new(1.0, border),
    );

    let mark_rect = Rect::from_min_size(
        Pos2::new(rect.left() + theme::PAD_LEFT, rect.center().y - theme::MARK_SIZE.y / 2.0),
        theme::MARK_SIZE,
    );
    mark::paint(
        &painter,
        mark_rect,
        &Appearance { state: state.hud, failure: failure.map(|f| f.kind) },
        state.mic_level,
        time,
    );

    let icon_rect = Rect::from_center_size(
        Pos2::new(rect.right() - theme::PAD_RIGHT - theme::ICON_SIZE / 2.0, rect.center().y),
        Vec2::splat(theme::ICON_SIZE),
    );

    // The body is everything the icon does not claim, so dragging the widget
    // works anywhere the user naturally grabs it.
    let body = ui.interact(rect, ui.id().with("capsule"), Sense::click_and_drag());
    let settled = drag_window(ui, &body);

    let text_left = mark_rect.right() + theme::MARK_GAP;
    match failure {
        Some(failure) => painter.text(
            Pos2::new(text_left, rect.center().y),
            Align2::LEFT_CENTER,
            failure.headline,
            theme::error_font(),
            theme::ERROR_TEXT,
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
                color,
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
            theme::ICON_HOVER,
        );
    }
    let tint = if failure.is_some() || state.unread_failure {
        theme::ICON_ALERT
    } else {
        theme::ICON_TINT
    };
    paint_console_glyph(&painter, icon_rect, tint);
    if state.unread_failure {
        // Survives the capsule returning to Ready, so a failure that happened
        // while the user was typing elsewhere is still there to be found.
        let dot = Pos2::new(icon_rect.right() - 8.0, icon_rect.top() + 8.0);
        painter.circle_filled(dot, 4.5, theme::FILL);
        painter.circle_filled(dot, 3.5, theme::UNREAD_DOT);
    }

    let mut action = settled.map(CapsuleAction::Moved);
    if icon.clicked() {
        action = Some(CapsuleAction::ToggleConsole);
    }
    // The icon wins the overlap with the body, so without its own menu the
    // 36x36 square would be a dead zone for right-clicks. The spec asks for
    // the menu anywhere on the capsule.
    body.context_menu(|ui| menu(ui, &mut action));
    icon.context_menu(|ui| menu(ui, &mut action));
    action
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
                let (pointer_x, pointer_y) = crate::platform::pointer_on_screen();
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
    let (pointer_x, pointer_y) = crate::platform::pointer_on_screen();
    // Cocoa measures upwards from the bottom of the screen and egui measures
    // downwards from the top, so the vertical movement is inverted.
    // Both terms are differences from the anchor, so the shared origin cancels
    // and this is correct for displays left of or above the main one, and
    // across a drag between displays of different scale factors.
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
        (anchor.window.y - (pointer_y - anchor.pointer_y) as f32).round(),
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
