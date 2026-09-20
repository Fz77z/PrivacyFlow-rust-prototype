use crate::state::{AppState, HudState};
use crate::ui::mark::{self, Appearance};
use crate::ui::theme;
use egui::{Align2, Color32, Pos2, Rect, Rounding, Sense, Stroke, Ui, Vec2};

pub enum CapsuleAction {
    ToggleConsole,
    OpenConsole,
    Quit,
}

/// Draws the whole widget. The capsule is the window, so this paints every
/// pixel the user sees: there is no title bar above it.
pub fn show(ui: &mut Ui, state: &AppState, time: f64) -> Option<CapsuleAction> {
    let rect = Rect::from_min_size(ui.max_rect().min, theme::CAPSULE_SIZE);
    let painter = ui.painter_at(rect);
    let failure = state.last_failure.as_ref().filter(|_| state.hud == HudState::Error);
    let border = match (failure.is_some(), state.hud) {
        (true, _) => theme::ERROR_BORDER,
        (false, HudState::Listening) => Color32::from_rgb(47, 107, 88),
        (false, HudState::Processing) => Color32::from_rgb(61, 74, 128),
        (false, HudState::Done) => Color32::from_rgb(63, 107, 69),
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
    if body.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

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
                HudState::Processing => ("Transcribing", theme::LABEL),
                HudState::Done => ("Inserted", theme::LABEL),
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
            Color32::from_rgb(57, 63, 78),
        );
    }
    let tint = if failure.is_some() || state.unread_failure {
        Color32::from_rgb(224, 138, 138)
    } else {
        theme::ICON_TINT
    };
    paint_console_glyph(&painter, icon_rect, tint);
    if state.unread_failure {
        // Survives the capsule returning to Ready, so a failure that happened
        // while the user was typing elsewhere is still there to be found.
        let dot = Pos2::new(icon_rect.right() - 8.0, icon_rect.top() + 8.0);
        painter.circle_filled(dot, 4.5, theme::FILL);
        painter.circle_filled(dot, 3.5, Color32::from_rgb(246, 128, 128));
    }

    let mut action = None;
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
    let glyph = Rect::from_center_size(rect.center(), Vec2::new(19.0, 16.0));
    let stroke = Stroke::new(1.6, tint);
    painter.rect_stroke(glyph, Rounding::same(3.4), stroke);
    let title_y = glyph.top() + 4.6;
    painter.line_segment(
        [Pos2::new(glyph.left(), title_y), Pos2::new(glyph.right(), title_y)],
        stroke,
    );
    let inset = 3.4;
    painter.line_segment(
        [
            Pos2::new(glyph.left() + inset, glyph.top() + 8.8),
            Pos2::new(glyph.left() + inset + 7.0, glyph.top() + 8.8),
        ],
        stroke,
    );
    painter.line_segment(
        [
            Pos2::new(glyph.left() + inset, glyph.top() + 12.2),
            Pos2::new(glyph.left() + inset + 10.0, glyph.top() + 12.2),
        ],
        stroke,
    );
}
