use crate::audio::{RecordedAudio, Recorder};
use crate::platform::{frontmost_application_pid, insert_text, GlobalHotkey, HotkeyEvent};
use crate::router::{KevWorker, RouteResult};
use crate::state::{dur_ms, AppState, HudState, RecordingState, Route, Timings};
use crate::ui;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::Receiver as HotkeyReceiver;
use std::time::{Duration, Instant};

const MAX_RECORDING_DURATION: Duration = Duration::from_secs(120);

struct WorkItem {
    audio: RecordedAudio,
    speech_finished: Instant,
    queued_at: Instant,
    capture_finalize_ms: u128,
    target_pid: Option<i32>,
}
struct WorkResult {
    transcript: String,
    route_result: Option<RouteResult>,
    output: String,
    timings: Timings,
    error: Option<String>,
}

pub struct LocalFlowApp {
    state: AppState,
    recorder: Option<Recorder>,
    hotkey_events: HotkeyReceiver<HotkeyEvent>,
    _hotkey: GlobalHotkey,
    work_tx: Sender<WorkItem>,
    result_rx: Receiver<WorkResult>,
    audio_dir: PathBuf,
    done_at: Option<Instant>,
    recording_started: Option<Instant>,
    target_pid: Option<i32>,
}

impl LocalFlowApp {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf) -> Self {
        install_visuals(&cc.egui_ctx);
        let repaint = cc.egui_ctx.clone();
        let (hotkey, hotkey_events, hotkey_error) =
            match GlobalHotkey::right_option(move || repaint.request_repaint()) {
                Ok((hotkey, events)) => (hotkey, events, None),
                Err(error) => {
                    let (_tx, events) = std::sync::mpsc::channel();
                    (
                        GlobalHotkey,
                        events,
                        Some(format!("Hotkey unavailable: {error:#}")),
                    )
                }
            };
        let mut state = AppState::default();
        if let Some(error) = hotkey_error {
            state.hud = HudState::Error;
            state.last_error = Some(error);
        }
        let (work_tx, work_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let audio_dir = data_dir.join("cache").join("audio");
        start_pipeline_worker(work_rx, result_tx);
        Self {
            state,
            recorder: None,
            hotkey_events,
            _hotkey: hotkey,
            work_tx,
            result_rx,
            audio_dir,
            done_at: None,
            recording_started: None,
            target_pid: None,
        }
    }

    fn start_recording(&mut self) {
        if self.recorder.is_some() || self.state.recording != RecordingState::Idle {
            return;
        }
        if self.state.debug_open {
            self.fail(
                "Close debug, then focus the destination text field before dictating".to_owned(),
            );
            return;
        }
        let target_pid = frontmost_application_pid();
        if target_pid == Some(std::process::id() as i32) || target_pid.is_none() {
            self.fail("Focus the destination text field before dictating".to_owned());
            return;
        }
        self.state.reset_for_recording();
        match Recorder::start() {
            Ok(recorder) => {
                self.target_pid = target_pid;
                self.recording_started = Some(Instant::now());
                self.recorder = Some(recorder);
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn finish_recording(&mut self) {
        let Some(recorder) = self.recorder.take() else {
            return;
        };
        self.recording_started = None;
        self.state.recording = RecordingState::Processing;
        self.state.hud = HudState::Processing;
        let speech_finished = Instant::now();
        let finalize_started = Instant::now();
        match recorder.stop_to_wav(&self.audio_dir) {
            Ok(audio) => {
                self.state.timings.audio_ms = Some(dur_ms(audio.duration));
                let capture_finalize_ms = dur_ms(finalize_started.elapsed());
                self.state.timings.capture_finalize_ms = Some(capture_finalize_ms);
                if self
                    .work_tx
                    .send(WorkItem {
                        audio,
                        speech_finished,
                        queued_at: Instant::now(),
                        capture_finalize_ms,
                        target_pid: self.target_pid.take(),
                    })
                    .is_err()
                {
                    self.fail("The processing worker stopped unexpectedly".to_owned());
                }
            }
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn receive_results(&mut self) {
        while let Ok(result) = self.result_rx.try_recv() {
            self.state.transcript = result.transcript;
            self.state.route = result.route_result.as_ref().map(|r| r.route);
            self.state.output = result.output;
            self.state.timings = result.timings;
            if let Some(error) = result.error {
                self.fail(error);
            } else {
                self.state.recording = RecordingState::Idle;
                self.state.hud = HudState::Done;
                self.state.push_history(None);
                self.done_at = Some(Instant::now());
            }
        }
    }

    fn fail(&mut self, message: String) {
        self.state.recording = RecordingState::Idle;
        self.state.hud = HudState::Error;
        self.state.last_error = Some(message.clone());
        self.state.push_history(Some(message));
        self.done_at = Some(Instant::now());
    }

    fn set_debug_open(&mut self, ctx: &egui::Context, open: bool) {
        self.state.debug_open = open;
        let size = if open {
            egui::Vec2::new(640.0, 520.0)
        } else {
            egui::Vec2::new(340.0, 84.0)
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(size));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
    }

    fn draw_debug(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let background = ui.max_rect();
        ui.painter().rect_filled(
            background,
            egui::Rounding::ZERO,
            egui::Color32::from_rgb(25, 28, 36),
        );
        let content = background.shrink2(egui::Vec2::new(20.0, 16.0));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content), |ui| {
            ui.horizontal(|ui| {
                ui.heading("LocalFlow debug");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Quit").min_size(egui::vec2(46.0, 26.0))).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if ui.add(egui::Button::new("← Back").min_size(egui::vec2(66.0, 26.0))).clicked() {
                        self.set_debug_open(ctx, false);
                    }
                });
            });
            ui.label(egui::RichText::new("Local-only development information. Audio is deleted after ASR.").small());
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(10.0);

            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for record in &self.state.history {
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(29, 33, 43))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 66, 80)))
                        .rounding(egui::Rounding::same(10.0))
                        .inner_margin(egui::Margin::same(14.0))
                        .show(ui, |ui| {
                            ui.set_min_width(560.0);
                            ui.label(egui::RichText::new(record.timestamp.format("%H:%M:%S").to_string()).strong());
                            ui.add_space(5.0);
                            ui.label(egui::RichText::new("ASR").small().strong());
                            ui.monospace(&record.transcript);
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Route").small().strong());
                                ui.monospace(record.route.map(|route| route.as_str()).unwrap_or("—"));
                            });
                            ui.label(egui::RichText::new("Output").small().strong());
                            ui.monospace(&record.output);
                            ui.add_space(6.0);
                            ui.small(format!(
                                "audio {} · finalize {} · queue {} · ASR {} · router {} · insert {} · total {} ms",
                                opt_ms(record.timings.audio_ms),
                                opt_ms(record.timings.capture_finalize_ms),
                                opt_ms(record.timings.queue_ms),
                                opt_ms(record.timings.asr_ms),
                                opt_ms(record.timings.router_ms),
                                opt_ms(record.timings.insert_ms),
                                opt_ms(record.timings.total_ms),
                            ));
                            if let Some(error) = &record.error {
                                ui.add_space(5.0);
                                ui.colored_label(egui::Color32::LIGHT_RED, error);
                            }
                        });
                    ui.add_space(8.0);
                }
            });
        });
    }
}

impl eframe::App for LocalFlowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.hotkey_events.try_recv() {
            match event {
                HotkeyEvent::Pressed => self.start_recording(),
                HotkeyEvent::Released => self.finish_recording(),
            }
        }
        self.receive_results();
        if self
            .recording_started
            .is_some_and(|started| started.elapsed() >= MAX_RECORDING_DURATION)
        {
            // Drop the stream without writing a WAV. This bounds memory use even
            // if macOS or a modifier-key edge case loses the release event.
            self.recorder.take();
            self.recording_started = None;
            self.target_pid = None;
            self.fail("Recording stopped after two minutes; please dictate again".to_owned());
        }
        if let Some(done_at) = self.done_at {
            if done_at.elapsed() > Duration::from_millis(1400)
                && self.state.hud != HudState::Listening
            {
                self.state.hud = HudState::Idle;
                self.done_at = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        if let Some(recorder) = &self.recorder {
            self.state.mic_level = recorder.level();
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if self.state.hud == HudState::Processing {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                if self.state.debug_open {
                    self.draw_debug(ui, ctx);
                } else if let Some(action) = ui::hud::show(
                    ui,
                    self.state.hud,
                    self.state.mic_level,
                    self.state.last_error.as_deref(),
                    ui.input(|i| i.time),
                ) {
                    match action {
                        ui::hud::HudAction::ToggleDebug => self.set_debug_open(ctx, true),
                        ui::hud::HudAction::Close => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close)
                        }
                    }
                }
            });
    }
}

fn start_pipeline_worker(work_rx: Receiver<WorkItem>, result_tx: Sender<WorkResult>) {
    std::thread::Builder::new()
        .name("localflow-pipeline".into())
        .spawn(move || {
            // The Python process loads mlx-whisper and the exact pool_300 Kev
            // checkpoint once, then remains resident for the app lifetime.
            let mut worker = KevWorker::start().map_err(|error| error.to_string());
            for item in work_rx {
                let result = process(&mut worker, item);
                let _ = result_tx.send(result);
            }
        })
        .expect("Could not start LocalFlow pipeline worker");
}

fn process(worker: &mut Result<KevWorker, String>, item: WorkItem) -> WorkResult {
    let speech_finished = item.speech_finished;
    let mut timings = Timings {
        audio_ms: Some(dur_ms(item.audio.duration)),
        capture_finalize_ms: Some(item.capture_finalize_ms),
        queue_ms: Some(dur_ms(item.queued_at.elapsed())),
        ..Default::default()
    };
    let target_pid = item.target_pid;
    let inference = match worker {
        Ok(worker) => worker.transcribe_and_route(&item.audio.path),
        Err(error) => Err(anyhow::anyhow!(error.clone())),
    };
    let _ = std::fs::remove_file(&item.audio.path); // Audio is temporary and is never retained by the app.
    let inference = match inference {
        Ok(value) => value,
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                String::new(),
                None,
                String::new(),
                error.to_string(),
            )
        }
    };
    timings.asr_ms = Some(inference.asr_ms);
    timings.router_ms = Some(inference.route.elapsed_ms);
    if let Some(warning) = inference.shadow_warning {
        eprintln!(
            "LocalFlow inserted dictation but did not record its shadow observation: {warning}"
        );
    }

    // This milestone intentionally proves the safe vertical slice only. We do
    // not apply the old placeholder cleanup to a non-PASS utterance.
    if inference.route.route != Route::PassThrough {
        return failed(
            timings,
            speech_finished,
            inference.transcript,
            Some(inference.route),
            String::new(),
            "This utterance was not PASS_THROUGH; cleanup and transformation are not wired yet"
                .to_owned(),
        );
    }

    let target_pid = match target_pid {
        Some(pid) => pid,
        None => {
            return failed(
                timings,
                speech_finished,
                inference.transcript,
                Some(inference.route),
                String::new(),
                "No destination app was focused when dictation started".to_owned(),
            )
        }
    };
    let insert_started = Instant::now();
    if let Err(error) = insert_text(&inference.transcript, target_pid) {
        return failed(
            timings,
            speech_finished,
            inference.transcript,
            Some(inference.route),
            String::new(),
            error.to_string(),
        );
    }
    timings.insert_ms = Some(dur_ms(insert_started.elapsed()));
    timings.total_ms = Some(dur_ms(speech_finished.elapsed()));
    WorkResult {
        transcript: inference.transcript.clone(),
        route_result: Some(inference.route),
        output: inference.transcript,
        timings,
        error: None,
    }
}

fn failed(
    mut timings: Timings,
    speech_finished: Instant,
    transcript: String,
    route_result: Option<RouteResult>,
    output: String,
    error: String,
) -> WorkResult {
    timings.total_ms = Some(dur_ms(speech_finished.elapsed()));
    WorkResult {
        transcript,
        route_result,
        output,
        timings,
        error: Some(error),
    }
}
fn opt_ms(value: Option<u128>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| "—".into())
}
fn install_visuals(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.window_fill = egui::Color32::from_rgb(25, 28, 36);
    visuals.panel_fill = egui::Color32::TRANSPARENT;
    visuals.window_rounding = egui::Rounding::same(16.0);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(39, 44, 55);
    ctx.set_visuals(visuals);
}
