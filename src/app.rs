use crate::audio::{CapturedAudio, Microphone};
use crate::platform::{
    frontmost_application_pid, insert_text, GlobalHotkey, HotkeyEvent, Insertion,
};
use crate::router::{KevWorker, Reply};
use crate::state::{AppState, Failure, HudState, Route, Timings, WorkerStatus};
use crate::ui;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver as HotkeyReceiver;
use std::time::{Duration, Instant};

const MAX_RECORDING_DURATION: Duration = Duration::from_secs(120);

/// How long a dictation must be in the pipeline before the capsule says so.
/// Long enough that an answer arriving almost immediately, which is what a
/// capture with no speech in it does, replaces the listening state directly
/// instead of flashing Transcribing on the way past. Short enough that a real
/// dictation, which takes about a second, still reads as instant feedback.
const PROCESSING_ANNOUNCE_DELAY: Duration = Duration::from_millis(120);

struct WorkItem {
    captured: CapturedAudio,
    speech_finished: Instant,
    queued_at: Instant,
    target_pid: Option<i32>,
}
/// What became of one utterance. It either reached the cursor, in one of the
/// two ways a dictation can land, or it was lost at a named stage. It is never
/// both, so the two answers share one field rather than sitting side by side
/// with one of them always meaningless.
enum Outcome {
    Inserted(Insertion),
    /// Which stage failed, categorised where it happened. The pipeline is the
    /// only thing that knows whether the words were lost to the transcriber or
    /// to the paste, so the whole failure travels back rather than being
    /// reconstructed by the UI.
    Failed(Failure),
}

struct WorkResult {
    transcript: String,
    route: Option<Route>,
    output: String,
    timings: Timings,
    outcome: Outcome,
}

/// The pipeline thread's only way of speaking to the UI. Readiness travels
/// with results so there is a single ordering of everything the UI learns.
enum PipelineMessage {
    WorkerReady,
    WorkerFailed(String),
    Finished(Box<WorkResult>),
    /// The capture held no speech. Deliberately not a `Finished` carrying an
    /// empty result: there is no transcript, no route and no insertion to
    /// report, and the capsule has nothing to do but say so and settle.
    NoSpeech,
}

pub struct LocalFlowApp {
    state: AppState,
    microphone: Option<Microphone>,
    hotkey_events: HotkeyReceiver<HotkeyEvent>,
    _hotkey: GlobalHotkey,
    work_tx: Sender<WorkItem>,
    result_rx: Receiver<PipelineMessage>,
    recording_started: Option<Instant>,
    target_pid: Option<i32>,
    data_dir: PathBuf,
}

impl LocalFlowApp {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf) -> Self {
        install_visuals(&cc.egui_ctx);
        // The capsule floats over whatever the user is writing in, so clicking
        // it to drag it or to open the console must not take focus away from
        // that. Done here because the window exists by the time this runs and
        // the console, which is a normal window and should take focus, does
        // not exist yet.
        let capsule_non_activating = crate::platform::make_capsule_non_activating(cc);
        let repaint = cc.egui_ctx.clone();
        let (hotkey, hotkey_events, hotkey_error) =
            match GlobalHotkey::right_option(move || repaint.request_repaint()) {
                Ok((hotkey, events)) => (hotkey, events, None),
                Err(error) => {
                    let (_tx, events) = std::sync::mpsc::channel();
                    (
                        GlobalHotkey,
                        events,
                        Some(Failure::blocked(
                            "Hotkey unavailable",
                            format!("Hotkey unavailable: {error:#}"),
                        )),
                    )
                }
            };
        // The microphone is opened once, here, so a keypress only has to
        // restart an already-built stream instead of spending device setup out
        // of the first moments of speech...
        let (microphone, microphone_error) = match Microphone::open() {
            Ok(microphone) => (Some(microphone), None),
            Err(error) => (
                None,
                Some(Failure::input_unavailable(
                    "Microphone unavailable",
                    format!("Microphone unavailable: {error:#}"),
                )),
            ),
        };
        let mut state = AppState {
            hotkey_installed: hotkey_error.is_none(),
            microphone_available: microphone.is_some(),
            capsule_non_activating,
            ..Default::default()
        };
        // Losing this is a functional problem, not a cosmetic one: a capsule
        // that takes focus when clicked leaves the next dictation with nowhere
        // to go, which surfaces later as "No text field focused" and looks
        // like the user's mistake. It is reported last because the other two
        // stop dictation outright.
        let focus_error = (!capsule_non_activating).then(|| {
            Failure::blocked(
                "Capsule takes focus",
                "The capsule could not be stopped from taking keyboard focus.                  Clicking it will move focus away from what you are writing in,                  and the next dictation will report no text field focused.",
            )
        });
        if let Some(failure) = hotkey_error.or(microphone_error).or(focus_error) {
            // The dot points at the console, so the console has to have
            // something to show when the user follows it. Clearing the dwell
            // timer keeps a startup failure on the capsule indefinitely: there
            // is no working state for it to decay back into...
            state.record_failure(failure);
            state.done_at = None;
        }
        let (work_tx, work_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let audio_dir = data_dir.join("cache").join("audio");
        sweep_audio_cache(&audio_dir);
        start_pipeline_worker(work_rx, result_tx, audio_dir, cc.egui_ctx.clone());
        Self {
            state,
            microphone,
            hotkey_events,
            _hotkey: hotkey,
            work_tx,
            result_rx,
            recording_started: None,
            target_pid: None,
            data_dir,
        }
    }

    fn start_recording(&mut self) {
        // A dictation already in flight owns the microphone and the capsule,
        // so a second press is ignored rather than allowed to restart either.
        if matches!(self.state.hud, HudState::Listening | HudState::Processing) {
            return;
        }
        let target_pid = frontmost_application_pid();
        if target_pid == Some(std::process::id() as i32) || target_pid.is_none() {
            self.fail(Failure::blocked(
                "No text field focused",
                "Focus the destination text field before dictating",
            ));
            return;
        }
        let Some(microphone) = &self.microphone else {
            self.fail(Failure::input_unavailable(
                "Microphone unavailable",
                "The microphone is unavailable; restart LocalFlow",
            ));
            return;
        };
        // The stream was built at startup, so this only restarts it. Opening
        // the device here would cost over a hundred milliseconds of speech...
        if let Err(error) = microphone.start_recording() {
            self.fail(Failure::input_unavailable("Microphone unavailable", error.to_string()));
            return;
        }
        self.state.reset_for_recording();
        self.target_pid = target_pid;
        self.recording_started = Some(Instant::now());
    }

    fn finish_recording(&mut self) {
        if self.recording_started.take().is_none() {
            return;
        }
        let Some(microphone) = &self.microphone else {
            return;
        };
        self.state.begin_processing();
        let speech_finished = Instant::now();
        // Only the microphone stream is stopped here. Draining the capture
        // buffer and encoding the WAV happen on the pipeline thread, so
        // releasing the hotkey never janks the HUD or delays the next press...
        match microphone.stop_recording() {
            Ok(captured) => {
                if self
                    .work_tx
                    .send(WorkItem {
                        captured,
                        speech_finished,
                        queued_at: Instant::now(),
                        target_pid: self.target_pid.take(),
                    })
                    .is_err()
                {
                    self.fail(Failure::dropped(
                        "Transcription failed",
                        "The processing worker stopped unexpectedly",
                    ));
                }
            }
            Err(error) => self.fail(Failure::dropped("Recording failed", error.to_string())),
        }
    }

    fn receive_results(&mut self) {
        while let Ok(message) = self.result_rx.try_recv() {
            match message {
                PipelineMessage::WorkerReady => self.state.worker = WorkerStatus::Ready,
                PipelineMessage::WorkerFailed(why) => {
                    self.state.worker = WorkerStatus::Failed(why);
                }
                PipelineMessage::Finished(result) => self.apply_result(*result),
                PipelineMessage::NoSpeech => self.state.record_no_speech(),
            }
        }
    }

    /// The pipeline's verdict becomes the capsule's state. This is the only
    /// transition out of Transcribing, so it either files a finished dictation
    /// or reports which stage lost it.
    fn apply_result(&mut self, result: WorkResult) {
        self.state.transcript = result.transcript;
        self.state.route = result.route;
        self.state.output = result.output;
        self.state.timings = result.timings;
        match result.outcome {
            Outcome::Failed(failure) => self.fail(failure),
            // A dictation whose destination went away is still a success from
            // the user's side: the words exist and are on the pasteboard. It
            // is reported as its own state rather than as either a clean
            // insert or a failure, because it is neither.
            Outcome::Inserted(insertion) => self.state.record_inserted(insertion),
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.state.record_failure(failure);
    }
}

impl eframe::App for LocalFlowApp {
    /// The capsule paints its own shape into a transparent window, so the
    /// window itself must contribute nothing. eframe's default clear colour is
    /// a 70% opaque near-black across the whole viewport, which shows up as a
    /// rectangle around the capsule's rounded corners.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Color32::TRANSPARENT.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.hotkey_events.try_recv() {
            match event {
                HotkeyEvent::Pressed => self.start_recording(),
                HotkeyEvent::Released => self.finish_recording(),
            }
        }
        self.receive_results();
        self.state.announce_processing(PROCESSING_ANNOUNCE_DELAY);
        if self
            .recording_started
            .is_some_and(|started| started.elapsed() >= MAX_RECORDING_DURATION)
        {
            // Pause without writing a WAV. This bounds memory use even if macOS
            // or a modifier-key edge case loses the release event.
            if let Some(microphone) = &self.microphone {
                let _ = microphone.stop_recording();
            }
            self.recording_started = None;
            self.target_pid = None;
            self.fail(Failure::dropped(
                "Recording hit 2 min",
                "Recording stopped after two minutes; please dictate again",
            ));
        }
        if let Some(done_at) = self.state.done_at {
            if done_at.elapsed() > Duration::from_millis(1400)
                && self.state.hud != HudState::Listening
            {
                self.state.hud = HudState::Idle;
                self.state.done_at = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        }
        if self.recording_started.is_some() {
            if let Some(microphone) = &self.microphone {
                self.state.mic_level = microphone.level();
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if self.state.hud == HudState::Processing {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        // Nothing else will wake the UI in time to make the announcement,
        // because the pipeline only repaints when it has an answer.
        if self.state.processing_since.is_some() {
            ctx.request_repaint_after(PROCESSING_ANNOUNCE_DELAY);
        }

        // A console buried behind other windows is exactly when someone
        // reaches for the menu item, so opening it also raises it.
        let mut raise_console = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                if let Some(action) = ui::capsule::show(ui, &self.state, ui.input(|i| i.time)) {
                    match action {
                        ui::capsule::CapsuleAction::ToggleConsole => {
                            if self.state.console_open {
                                self.state.console_open = false;
                            } else {
                                self.state.open_console();
                                raise_console = true;
                            }
                        }
                        // The menu item says "Open console", so it opens one:
                        // an already-open console is raised rather than shut.
                        ui::capsule::CapsuleAction::OpenConsole => {
                            self.state.open_console();
                            raise_console = true;
                        }
                        ui::capsule::CapsuleAction::Moved(position) => {
                            let size = ui::theme::CAPSULE_SIZE;
                            crate::window_position::save(
                                &self.data_dir,
                                crate::window_position::Centre {
                                    x: position.x + size.x / 2.0,
                                    y: position.y + size.y / 2.0,
                                },
                            );
                        }
                        ui::capsule::CapsuleAction::Quit => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close)
                        }
                    }
                }
            });

        if self.state.console_open {
            let builder = egui::ViewportBuilder::default()
                .with_title("LocalFlow")
                .with_inner_size([640.0, 520.0])
                .with_min_inner_size([520.0, 400.0]);
            let data_dir = self.data_dir.clone();
            let state = &mut self.state;
            // Immediate rather than deferred: a deferred viewport's callback must be
            // Fn + Send + Sync + 'static, which would force AppState behind a mutex
            // for no reason other than the signature.
            let stay_open = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("console"),
                builder,
                move |ctx, _class| {
                    if raise_console {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    ui::console::show(ctx, state, &data_dir)
                },
            );
            if !stay_open {
                self.state.console_open = false;
            }
        }
    }
}

/// The UI sleeps when nothing is happening, so the pipeline wakes it after
/// every message. Without that, a finished dictation sits unread in the
/// channel and the next hotkey press is swallowed by a stale Processing state.
fn start_pipeline_worker(
    work_rx: Receiver<WorkItem>,
    result_tx: Sender<PipelineMessage>,
    audio_dir: PathBuf,
    repaint: egui::Context,
) {
    std::thread::Builder::new()
        .name("localflow-pipeline".into())
        .spawn(move || {
            // The Python process loads mlx-whisper and the exact pool_300 Kev
            // checkpoint once, then remains resident for the app lifetime.
            let mut worker = KevWorker::start().map_err(|error| error.to_string());
            match &worker {
                Ok(_) => {
                    let _ = result_tx.send(PipelineMessage::WorkerReady);
                }
                Err(error) => {
                    let _ = result_tx.send(PipelineMessage::WorkerFailed(error.clone()));
                }
            }
            repaint.request_repaint();
            for item in work_rx {
                let _ = result_tx.send(process(&mut worker, &audio_dir, item));
                repaint.request_repaint();
            }
        })
        .expect("Could not start LocalFlow pipeline worker");
}

/// Utterance audio is temporary, but a crash or a force quit leaves the last
/// WAV behind. The instance lock guarantees no other LocalFlow is running, so
/// anything still here belongs to a previous run and must not outlive it.
fn sweep_audio_cache(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No cache directory at all is the ordinary first-run case.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!(
                "LocalFlow could not read its audio cache at {}: {error}",
                dir.display()
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Err(error) = std::fs::remove_file(&path) {
            eprintln!(
                "LocalFlow could not delete leftover audio at {}: {error}",
                path.display()
            );
        }
    }
}

fn process(
    worker: &mut Result<KevWorker, String>,
    audio_dir: &Path,
    item: WorkItem,
) -> PipelineMessage {
    let speech_finished = item.speech_finished;
    let mut timings = Timings {
        queue_ms: Some(item.queued_at.elapsed().as_millis()),
        ..Default::default()
    };
    let target_pid = item.target_pid;
    let finalize_started = Instant::now();
    let audio = match item.captured.write_wav(audio_dir) {
        Ok(audio) => audio,
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                String::new(),
                None,
                String::new(),
                "Transcription failed",
                error.to_string(),
            )
        }
    };
    timings.capture_finalize_ms = Some(finalize_started.elapsed().as_millis());
    timings.audio_ms = Some(audio.duration.as_millis());

    let inference = match worker {
        Ok(worker) => worker.transcribe_and_route(&audio.path, audio.duration),
        Err(error) => Err(anyhow::anyhow!(error.clone())),
    };
    // Audio is temporary and is never retained by the app, so a failure to
    // delete it is a broken promise rather than a detail to swallow.
    if let Err(error) = std::fs::remove_file(&audio.path) {
        eprintln!(
            "LocalFlow could not delete {} after transcription: {error}",
            audio.path.display()
        );
    }
    let inference = match inference {
        Ok(Reply::Transcribed(inference)) => inference,
        // Nothing was said, so there is nothing to route, insert or file.
        Ok(Reply::NoSpeech) => return PipelineMessage::NoSpeech,
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                String::new(),
                None,
                String::new(),
                "Transcription failed",
                error.to_string(),
            )
        }
    };
    timings.asr_ms = Some(inference.asr_ms);
    timings.router_ms = Some(inference.router_ms);

    // The worker decides what each route produces, including refusing an
    // unimplemented one, so there is a single place that maps route to text.
    timings.transform_ms = inference.processing_ms;

    let target_pid = match target_pid {
        Some(pid) => pid,
        None => {
            return failed(
                timings,
                speech_finished,
                inference.transcript,
                Some(inference.route),
                String::new(),
                "Couldn't insert",
                "No destination app was focused when dictation started".to_owned(),
            )
        }
    };
    let insert_started = Instant::now();
    let insertion = match insert_text(&inference.output, target_pid) {
        Ok(insertion) => insertion,
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                inference.transcript,
                Some(inference.route),
                String::new(),
                "Couldn't insert",
                error.to_string(),
            )
        }
    };
    timings.insert_ms = Some(insert_started.elapsed().as_millis());
    timings.total_ms = Some(speech_finished.elapsed().as_millis());
    PipelineMessage::Finished(Box::new(WorkResult {
        transcript: inference.transcript,
        route: Some(inference.route),
        output: inference.output,
        timings,
        outcome: Outcome::Inserted(insertion),
    }))
}

fn failed(
    mut timings: Timings,
    speech_finished: Instant,
    transcript: String,
    route: Option<Route>,
    output: String,
    headline: &'static str,
    error: String,
) -> PipelineMessage {
    timings.total_ms = Some(speech_finished.elapsed().as_millis());
    PipelineMessage::Finished(Box::new(WorkResult {
        transcript,
        route,
        output,
        timings,
        // Every pipeline failure happens after the user has spoken, so the
        // kind is settled here: the words did not come back.
        outcome: Outcome::Failed(Failure::dropped(headline, error)),
    }))
}

fn install_visuals(ctx: &egui::Context) {
    crate::ui::theme::install(ctx);
    let mut visuals = egui::Visuals::dark();
    // The capsule's right-click menu and its failure tooltip are painted with
    // this, so it has to be the capsule's own fill rather than a copy of it...
    visuals.window_fill = crate::ui::theme::FILL;
    visuals.panel_fill = egui::Color32::TRANSPARENT;
    visuals.window_rounding = egui::Rounding::same(16.0);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(39, 44, 55);
    ctx.set_visuals(visuals);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The promise is that dictation audio never outlives its utterance, and a
    /// crash is exactly when that promise used to break: nothing deleted the
    /// WAV the previous run was still holding.
    #[test]
    fn startup_deletes_audio_left_behind_by_a_previous_run() {
        let dir = std::env::temp_dir().join(format!("localflow-sweep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let leftover = dir.join("utterance-20260921T000000.000Z.wav");
        std::fs::write(&leftover, b"leftover audio").unwrap();

        sweep_audio_cache(&dir);
        assert!(!leftover.exists(), "leftover audio survived startup");

        // A cache directory that does not exist yet is the first-run case, not
        // a failure worth reporting.
        std::fs::remove_dir_all(&dir).unwrap();
        sweep_audio_cache(&dir);
    }
}
