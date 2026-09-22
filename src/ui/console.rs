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
    let frame = egui::Frame::central_panel(&ctx.style())
        .fill(theme::FILL)
        .inner_margin(egui::Margin::same(20.0));
    egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
        tab_bar(ui, &mut state.console_tab);
        ui.add_space(18.0);
        match state.console_tab {
            ConsoleTab::Activity => activity(ui, state),
            ConsoleTab::Settings => settings(ui, ctx, state, data_dir),
            ConsoleTab::Status => status(ui, state, data_dir, microphone_name),
        }
    });
    if ctx.input(|i| i.viewport().close_requested()) {
        stay_open = false;
    }
    stay_open
}

/// The three tabs as one segmented control, which is how macOS presents a
/// choice between views, rather than as three loose buttons.
fn tab_bar(ui: &mut Ui, tab: &mut ConsoleTab) {
    egui::Frame::none()
        .fill(theme::CARD_FILL)
        .rounding(egui::Rounding::same(9.0))
        .inner_margin(egui::Margin::same(3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (value, name) in [
                    (ConsoleTab::Activity, "Activity"),
                    (ConsoleTab::Settings, "Settings"),
                    (ConsoleTab::Status, "Status"),
                ] {
                    ui.selectable_value(tab, value, name);
                }
            });
        });
}

/// The history, newest first as recorded. Each card leads with what the user
/// said, because that is what they came to find; how it got there is one
/// click away rather than competing with it.
fn activity(ui: &mut Ui, state: &AppState) {
    if state.history.is_empty() {
        ui.label(RichText::new("Nothing dictated yet.").color(theme::MUTED));
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (index, record) in state.history.iter().enumerate() {
            ui.push_id(index, |ui| history_card(ui, record));
            ui.add_space(10.0);
        }
    });
}

fn history_card(ui: &mut Ui, record: &crate::state::DebugRecord) {
    egui::Frame::none()
        .fill(theme::CARD_FILL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            // Shown in the user's own time zone. The record is kept in UTC,
            // which is right for storing and wrong for reading.
            let time = record.timestamp.with_timezone(&chrono::Local).format("%H:%M");
            ui.label(RichText::new(time.to_string()).small().color(theme::MUTED));
            ui.add_space(6.0);

            // A dictation that was heard and not understood has no
            // transcript, route, output or timings to show. The whole of
            // what is known about it is the remark, so the card is that
            // remark rather than a form of empty fields.
            if let Some(note) = &record.note {
                ui.colored_label(theme::MUTED, note);
                return;
            }

            // What reached the user, or failing that what was heard.
            let words = if record.output.is_empty() { &record.transcript } else { &record.output };
            if !words.is_empty() {
                ui.label(RichText::new(words).size(14.5).color(theme::LABEL));
            }
            // Both the capsule's "Copied" and the toast that goes with it are
            // gone within seconds, and they appear precisely when the text
            // did not land where the user was looking. The durable record
            // has to carry the reason.
            if let Some(why) = why_copied(record.insertion) {
                ui.add_space(6.0);
                ui.colored_label(theme::MUTED, why);
            }
            // The console keeps the original message, whatever the capsule
            // had room to say.
            if let Some(failure) = &record.failure {
                ui.add_space(6.0);
                ui.colored_label(theme::ERROR_TEXT, &failure.detail);
            }

            ui.add_space(8.0);
            egui::CollapsingHeader::new(RichText::new("Details").small().color(theme::MUTED))
                .id_salt("details")
                .show(ui, |ui| details(ui, record));
        });
}

/// How a dictation was produced: what was heard, how it was routed, and
/// where the time went. For diagnosing, so it is kept out of the way.
fn details(ui: &mut Ui, record: &crate::state::DebugRecord) {
    egui::Grid::new("details").num_columns(2).spacing([14.0, 6.0]).show(ui, |ui| {
        row(ui, "Heard", &record.transcript, theme::LABEL);
        row(
            ui,
            "Route",
            record.route.map(|route| route.as_str()).unwrap_or("—"),
            theme::LABEL,
        );
        row(ui, "Output", &record.output, theme::LABEL);
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(format!(
            "audio {} · finalize {} · queue {} · ASR {} · router {} · S1 {} · insert {} · total {} ms",
            opt_ms(record.timings.audio_ms),
            opt_ms(record.timings.capture_finalize_ms),
            opt_ms(record.timings.queue_ms),
            opt_ms(record.timings.asr_ms),
            opt_ms(record.timings.router_ms),
            opt_ms(record.timings.transform_ms),
            opt_ms(record.timings.insert_ms),
            opt_ms(record.timings.total_ms),
        ))
        .small()
        .color(theme::MUTED),
    );
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
fn settings(ui: &mut Ui, ctx: &egui::Context, state: &mut AppState, data_dir: &std::path::Path) {
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

    ui.add_space(14.0);

    let mut sound_cues = state.settings.sound_cues;
    if ui.checkbox(&mut sound_cues, "Sound cues").changed() {
        state.settings.sound_cues = sound_cues;
        state.settings_write_error = crate::settings::save(data_dir, state.settings).err();
        if state.settings_write_error.is_none() {
            state.settings_problem = None;
        }
    }
    ui.add_space(2.0);
    ui.label(
        RichText::new(
            "Play a short, quiet tone when a dictation starts and when it ends. It rises on \
             the press and falls on the release.",
        )
        .small()
        .color(theme::MUTED),
    );
    // Reported here rather than as a startup failure, because no audio output
    // costs a confirmation sound rather than a dictation. This checkbox is
    // where someone wondering why they hear nothing will look.
    if let Some(problem) = &state.cue_problem {
        ui.add_space(6.0);
        ui.colored_label(theme::ERROR_TEXT, format!("Cannot play: {problem}"));
    }

    if let Some(error) = &state.settings_write_error {
        ui.add_space(6.0);
        ui.colored_label(theme::ERROR_TEXT, format!("Not saved: {error}"));
    }

    // Quitting is rare and final, so it lives at the foot of Settings rather
    // than beside the tabs, where it was the most prominent control in the
    // window. The capsule's right-click menu offers it too.
    ui.add_space(28.0);
    ui.separator();
    ui.add_space(10.0);
    if ui.button("Quit LocalFlow").clicked() {
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }
}

/// Read-only, and reports only what can actually be observed. This tab is for
/// checking whether things are working, not for changing them; the Settings
/// tab is where the settings live.
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
