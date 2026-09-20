use crate::state::HudState;
use egui::{Color32, Pos2, Rect, Response, Rounding, Sense, Stroke, Ui, Vec2};

pub enum HudAction {
    ToggleDebug,
    Close,
}

pub fn show(
    ui: &mut Ui,
    state: HudState,
    level: f32,
    error: Option<&str>,
    time: f64,
) -> Option<HudAction> {
    let desired = Vec2::new(340.0, 84.0);
    let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        Rounding::same(20.0),
        Color32::from_rgb(25, 28, 36),
        Stroke::new(1.0, Color32::from_rgb(60, 66, 80)),
    );

    let close_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 22.0, rect.top() + 21.0),
        Vec2::splat(22.0),
    );
    let debug_rect = Rect::from_center_size(
        Pos2::new(rect.right() - 50.0, rect.top() + 21.0),
        Vec2::splat(22.0),
    );
    let debug = control(
        ui,
        debug_rect,
        response.id.with("debug"),
        "···",
        "Open debug view",
    );
    let close = control(
        ui,
        close_rect,
        response.id.with("close"),
        "×",
        "Quit LocalFlow",
    );

    let wave_rect = Rect::from_min_size(rect.min + Vec2::new(19.0, 22.0), Vec2::new(72.0, 36.0));
    let color = match state {
        HudState::Listening => Color32::from_rgb(113, 218, 178),
        HudState::Processing => Color32::from_rgb(132, 164, 255),
        HudState::Done => Color32::from_rgb(151, 224, 157),
        HudState::Error => Color32::from_rgb(246, 128, 128),
        HudState::Idle => Color32::from_gray(116),
    };
    paint_waveform(&painter, wave_rect, state, level, time, color);

    let (label, detail) = match state {
        HudState::Idle => ("Ready", "Hold Right Option to dictate"),
        HudState::Listening => ("Listening", "Release when you’re done"),
        HudState::Processing => ("Processing", "Transcribing locally"),
        HudState::Done => ("Done", "Inserted · transcript is on clipboard"),
        HudState::Error => (
            "Couldn’t complete",
            error.unwrap_or("Check permissions or model setup"),
        ),
    };
    let x = rect.left() + 108.0;
    painter.text(
        Pos2::new(x, rect.top() + 28.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(16.0),
        Color32::from_rgb(243, 245, 249),
    );
    painter.text(
        Pos2::new(x, rect.top() + 51.0),
        egui::Align2::LEFT_CENTER,
        detail,
        egui::FontId::proportional(11.5),
        Color32::from_rgb(157, 165, 181),
    );

    if close.clicked() {
        Some(HudAction::Close)
    } else if debug.clicked() {
        Some(HudAction::ToggleDebug)
    } else {
        None
    }
}

fn control(ui: &mut Ui, rect: Rect, id: egui::Id, label: &str, tooltip: &str) -> Response {
    let response = ui.interact(rect, id, Sense::click()).on_hover_text(tooltip);
    let fill = if response.hovered() {
        Color32::from_rgb(57, 63, 78)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(8.0), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(17.0),
        Color32::from_rgb(177, 185, 201),
    );
    response
}

fn paint_waveform(
    painter: &egui::Painter,
    rect: Rect,
    state: HudState,
    level: f32,
    time: f64,
    color: Color32,
) {
    let count = 8;
    let spacing = rect.width() / count as f32;
    for i in 0..count {
        let phase = time as f32
            * if state == HudState::Listening {
                7.0
            } else {
                2.0
            }
            + i as f32 * 0.7;
        let motion = (phase.sin() + 1.0) * 0.5;
        let amplitude = match state {
            HudState::Listening => 6.0 + (level * 24.0).max(motion * 6.0),
            HudState::Processing => 8.0 + motion * 12.0,
            HudState::Done => 6.0 + (i as f32 - 3.5).abs().mul_add(-0.6, 10.0),
            _ => 5.0,
        };
        let x = rect.left() + spacing * (i as f32 + 0.5);
        painter.line_segment(
            [
                Pos2::new(x, rect.center().y - amplitude / 2.0),
                Pos2::new(x, rect.center().y + amplitude / 2.0),
            ],
            Stroke::new(4.0, color),
        );
    }
}
