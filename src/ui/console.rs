use crate::state::{AppState, ConsoleTab, WorkerStatus};
use crate::ui::theme;
use egui::{Color32, RichText, Ui};

/// The console is a conventional macOS window: it has a title bar, it
/// resizes, and it never blocks dictation. Returns false when the user has
/// asked to close it.
pub fn show(
    ctx: &egui::Context,
    state: &mut AppState,
    data_dir: &std::path::Path,
    microphone_name: Option<&str>,
) -> bool {
    let mut stay_open = true;
    // The shared `panel_fill` is transparent because the capsule paints its
    // own shape into a transparent window. This window is an ordinary opaque
    // one, so it states its background instead of inheriting that.
    let frame = egui::Frame::central_panel(&ctx.style()).fill(theme::FILL);
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut state.console_tab, ConsoleTab::Activity, "Activity");
            ui.selectable_value(&mut state.console_tab, ConsoleTab::Settings, "Settings");
            ui.selectable_value(&mut state.console_tab, ConsoleTab::Status, "Status");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Quit LocalFlow").clicked() {
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::ROOT,
                        egui::ViewportCommand::Close,
                    );
                }
            });
        });
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);
        match state.console_tab {
            ConsoleTab::Activity => activity(ui, state),
            ConsoleTab::Settings => settings(ui, state, data_dir),
            ConsoleTab::Status => status(ui, state, data_dir, microphone_name),
        }
    });
    if ctx.input(|i| i.viewport().close_requested()) {
        stay_open = false;
    }
    stay_open
}

fn activity(ui: &mut Ui, state: &AppState) {
    if state.history.is_empty() {
        ui.label(RichText::new("Nothing dictated yet.").color(theme::MUTED));
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for record in &state.history {
            egui::Frame::none()
                .fill(theme::CARD_FILL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .rounding(egui::Rounding::same(10.0))
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(
                        RichText::new(record.timestamp.format("%H:%M:%S").to_string()).strong(),
                    );
                    // A dictation that was heard and not understood has no
                    // transcript, route, output or timings to show. The whole
                    // of what is known about it is the remark, so the card is
                    // that remark rather than a form of empty fields.
                    if let Some(note) = &record.note {
                        ui.add_space(5.0);
                        ui.colored_label(theme::MUTED, note);
                        return;
                    }
                    ui.add_space(5.0);
                    ui.label(RichText::new("ASR").small().strong());
                    ui.monospace(&record.transcript);
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Route").small().strong());
                        ui.monospace(record.route.map(|route| route.as_str()).unwrap_or("—"));
                    });
                    ui.label(RichText::new("Output").small().strong());
                    ui.monospace(&record.output);
                    ui.add_space(6.0);
                    ui.small(format!(
                        "audio {} · finalize {} · queue {} · ASR {} · router {} · S1 {} · insert {} · total {} ms",
                        opt_ms(record.timings.audio_ms),
                        opt_ms(record.timings.capture_finalize_ms),
                        opt_ms(record.timings.queue_ms),
                        opt_ms(record.timings.asr_ms),
                        opt_ms(record.timings.router_ms),
                        opt_ms(record.timings.transform_ms),
                        opt_ms(record.timings.insert_ms),
                        opt_ms(record.timings.total_ms),
                    ));
                    // Both the capsule's "Copied" and the toast that goes
                    // with it are gone within seconds, and they appear
                    // precisely when the text did not land where the user was
                    // looking. The durable record has to carry the reason.
                    if let Some(why) = why_copied(record.insertion) {
                        ui.add_space(5.0);
                        ui.colored_label(theme::MUTED, why);
                    }
                    // The console keeps the original message, whatever the
                    // capsule had room to say.
                    if let Some(failure) = &record.failure {
                        ui.add_space(5.0);
                        ui.colored_label(theme::ERROR_TEXT, &failure.detail);
                    }
                });
            ui.add_space(8.0);
        }
    });
}

/// Why a dictation was left on the clipboard, for the two endings where it
/// was, and nothing for the one where it reached the cursor.
fn why_copied(insertion: Option<crate::platform::Insertion>) -> Option<&'static str> {
    match insertion? {
        crate::platform::Insertion::Pasted => None,
        crate::platform::Insertion::CopiedOnly => Some(
            "Copied to the clipboard, not pasted: the destination app was no longer \
             frontmost.",
        ),
        crate::platform::Insertion::CopiedNoField => Some(
            "Copied to the clipboard, not pasted: the destination app had no text field \
             focused to receive it.",
        ),
    }
}

/// One setting. The tab is thin because LocalFlow has one thing to configure,
/// and it should look thin rather than be padded out with controls that do
/// not exist.
fn settings(ui: &mut Ui, state: &mut AppState, data_dir: &std::path::Path) {
    if let Some(problem) = state.settings_problem.clone() {
        egui::Frame::none()
            .fill(theme::CARD_FILL)
            .stroke(egui::Stroke::new(1.0, theme::ERROR_BORDER))
            .rounding(egui::Rounding::same(10.0))
            .inner_margin(egui::Margin::same(14.0))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new("Your settings could not be read").strong());
                ui.add_space(4.0);
                ui.colored_label(theme::ERROR_TEXT, problem);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "LocalFlow started on its defaults. The file is left as it is until \
                         you change a setting here.",
                    )
                    .small()
                    .color(theme::MUTED),
                );
            });
        ui.add_space(14.0);
    }

    let mut minimal_mode = state.settings.minimal_mode;
    if ui.checkbox(&mut minimal_mode, "Minimal mode").changed() {
        state.settings.minimal_mode = minimal_mode;
        state.settings_write_error = crate::settings::save(data_dir, state.settings).err();
        // A successful write means the file is no longer whatever it was
        // when it failed to read at startup: the banner above claims the bad
        // file is untouched, which stops being true the moment this save
        // succeeds.
        if state.settings_write_error.is_none() {
            state.settings_problem = None;
        }
    }
    ui.add_space(2.0);
    ui.label(
        RichText::new(
            "Shrink the capsule to a small bead when it is not in use. It grows while you \
             dictate, and when you point at it.",
        )
        .small()
        .color(theme::MUTED),
    );
    if let Some(error) = &state.settings_write_error {
        ui.add_space(6.0);
        ui.colored_label(theme::ERROR_TEXT, format!("Not saved: {error}"));
    }
}

/// Read-only, and reports only what can actually be observed. This tab is for
/// checking whether things are working, not for changing them; the Settings
/// tab is where LocalFlow's one setting lives.
fn status(
    ui: &mut Ui,
    state: &AppState,
    data_dir: &std::path::Path,
    microphone_name: Option<&str>,
) {
    let worker = match &state.worker {
        WorkerStatus::Starting => ("Starting".to_owned(), theme::MUTED),
        WorkerStatus::Ready => ("Ready".to_owned(), theme::INSERTED),
        WorkerStatus::Failed(why) => (why.clone(), theme::ERROR_TEXT),
    };
    let hotkey = if state.hotkey_installed {
        ("Right Option (hold to dictate)".to_owned(), theme::LABEL)
    } else {
        ("Right Option - watcher did not install".to_owned(), theme::ERROR_TEXT)
    };
    let microphone = match microphone_name {
        Some(name) => (name.to_owned(), theme::LABEL),
        None => ("No device opened".to_owned(), theme::ERROR_TEXT),
    };
    // Verified at startup by asking the capsule's own window, rather than
    // assumed from the fact that the attempt was made. The whole behaviour is
    // a thing that silently does not happen, so an unchecked claim about it
    // would be worth nothing.
    let focus = if state.capsule_non_activating {
        ("Capsule does not take focus".to_owned(), theme::LABEL)
    } else {
        ("Capsule takes focus when clicked".to_owned(), theme::ERROR_TEXT)
    };
    // Asked live rather than cached at startup, because the user can grant
    // this while the app is running and the answer is only useful if it is
    // current. Without it a dictation transcribes correctly and then nothing
    // reaches the cursor, which reads as a transcription fault rather than a
    // permission one.
    let accessibility = if crate::platform::can_synthesize_input() {
        ("Allowed to send keystrokes".to_owned(), theme::LABEL)
    } else {
        (
            "Not allowed - add LocalFlow to Accessibility, then relaunch".to_owned(),
            theme::ERROR_TEXT,
        )
    };
    // Ask the router how it resolved the research root, and let it decide what
    // counts as present, rather than recomputing either here. Validating with a
    // local is_dir() used to let this row read healthy at the same moment the
    // worker row reported the runtime missing.
    let (research, research_color) = match crate::router::research_root() {
        Ok(path) => match crate::router::runtime_paths(&path) {
            Ok(_) => (path.display().to_string(), theme::MUTED),
            Err(err) => (err.to_string(), theme::ERROR_TEXT),
        },
        Err(err) => (err.to_string(), theme::ERROR_TEXT),
    };
    egui::Grid::new("status").num_columns(2).spacing([18.0, 10.0]).show(ui, |ui| {
        row(ui, "Hotkey", &hotkey.0, hotkey.1);
        row(ui, "Microphone", &microphone.0, microphone.1);
        row(ui, "Accessibility", &accessibility.0, accessibility.1);
        row(ui, "Window focus", &focus.0, focus.1);
        row(ui, "Inference worker", &worker.0, worker.1);
        row(ui, "ASR", "mlx-community/whisper-large-v3-turbo", theme::LABEL);
        row(ui, "Router", "scaling_run/checkpoints/pool_300", theme::LABEL);
        row(ui, "Text processor", "superwhisper/s1-mini", theme::LABEL);
        row(ui, "Research root", &research, research_color);
        row(ui, "Data directory", &data_dir.display().to_string(), theme::MUTED);
    });
    ui.add_space(14.0);
    ui.label(
        RichText::new("Local only. Audio is deleted after transcription.")
            .small()
            .color(theme::MUTED),
    );
}

/// A latency that was never measured, because the dictation did not reach
/// that stage, reads as a dash rather than as a zero that claims it was free.
fn opt_ms(value: Option<u128>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
}

fn row(ui: &mut Ui, label: &str, value: &str, color: Color32) {
    ui.label(RichText::new(label).small().strong().color(theme::MUTED));
    ui.label(RichText::new(value).color(color));
    ui.end_row();
}
