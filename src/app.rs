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

/// Within this distance of the capsule's centre the bead is solid.
const NEAR_RADIUS: f32 = 120.0;
/// Beyond this distance it has faded as far as it goes.
const FAR_RADIUS: f32 = 420.0;
/// How faint the bead is allowed to get.
///
/// Deliberately not zero. LocalFlow has no Dock icon and no menu bar item, so
/// a bead that fades to nothing is an application the user cannot find, which
/// is the same failure as a capsule restored onto a display that is gone.
const BEAD_OPACITY_FLOOR: f32 = 0.18;
const _: () = assert!(BEAD_OPACITY_FLOOR > 0.0, "a bead that can vanish cannot be found again");

/// How solid the bead should be, given how far away the pointer is.
fn bead_opacity(distance: f32) -> f32 {
    if distance <= NEAR_RADIUS {
        return 1.0;
    }
    if distance >= FAR_RADIUS {
        return BEAD_OPACITY_FLOOR;
    }
    let travelled = (distance - NEAR_RADIUS) / (FAR_RADIUS - NEAR_RADIUS);
    1.0 - travelled * (1.0 - BEAD_OPACITY_FLOOR)
}

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
    /// Where the capsule is centred. Seeded from the remembered position at
    /// startup, updated whenever the capsule is dragged, and read back from
    /// the viewport on the first frame if nothing was remembered: that is the
    /// only way to learn where the window manager actually put the window.
    centre: Option<crate::window_position::Centre>,
    /// Whether the capsule was being dragged last frame. Read at the top of
    /// `update`, before this frame's response exists, so a fast drag that
    /// carries the pointer outside the window for a frame does not shrink the
    /// capsule out from under the user mid-drag.
    dragging: bool,
    /// The size the window is currently resized to. Seeded at construction
    /// from the size the window actually starts at (which `main.rs` decides
    /// from the same setting), then kept in step whenever the window is
    /// resized. Compared against this frame's chosen size so the window is
    /// only touched, and `work_areas` only queried, on an actual transition
    /// between the three sizes, and this holds regardless of whether minimal
    /// mode is currently on or off: it also covers being switched off while
    /// the window is not yet full size.
    applied_size: Option<ui::capsule::CapsuleSize>,
}

impl LocalFlowApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        data_dir: PathBuf,
        settings: crate::settings::Load,
    ) -> Self {
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
        // Settings problems are reported last because the other three stop
        // dictation outright; an unreadable settings file does not.
        let settings_error = settings
            .problem
            .clone()
            .map(|problem| Failure::blocked("Settings unreadable", problem));
        let mut state = AppState {
            hotkey_installed: hotkey_error.is_none(),
            capsule_non_activating,
            settings: settings.settings,
            settings_problem: settings.problem,
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
        if let Some(failure) =
            hotkey_error.or(microphone_error).or(focus_error).or(settings_error)
        {
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
        let capsule_size = (ui::theme::CAPSULE_SIZE.x, ui::theme::CAPSULE_SIZE.y);
        let centre = crate::window_position::load(&data_dir, capsule_size);
        // Matches what main.rs already decided the window starts at, from the
        // same setting. Seeded rather than left `None` so "already the right
        // size" is true from the very first frame: an unseeded `None` would
        // read as a change on frame one and immediately resize a window that
        // was already correct.
        let applied_size = Some(if state.settings.minimal_mode {
            ui::capsule::CapsuleSize::Bead
        } else {
            ui::capsule::CapsuleSize::Full
        });
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
            centre,
            dragging: false,
            applied_size,
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

        // Where the capsule has never been moved and nothing was remembered,
        // there is nothing to place it from until the window manager has put
        // it somewhere. That position only exists once the viewport has been
        // shown, so it is read back here, on the first frame it is available,
        // rather than guessed at construction.
        if self.centre.is_none() {
            if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
                self.centre =
                    Some(crate::window_position::Centre { x: rect.center().x, y: rect.center().y });
            }
        }

        // Minimal mode has to know where the pointer is even when it is
        // outside the window, which egui cannot report: it measures relative
        // to the window, and a window that resizes under the cursor perturbs
        // the very number deciding whether it should resize.
        let minimal = self.state.settings.minimal_mode;
        let (pointing, opacity) = if minimal {
            let (px, py) = crate::platform::pointer_in_window_space();
            let rect = ctx.input(|i| i.viewport().outer_rect);
            let pointing = rect.is_some_and(|rect| {
                rect.contains(egui::pos2(px as f32, py as f32))
            }) || self.dragging;
            let distance = self
                .centre
                .map(|centre| {
                    egui::pos2(centre.x, centre.y).distance(egui::pos2(px as f32, py as f32))
                })
                .unwrap_or(0.0);
            (pointing, bead_opacity(distance))
        } else {
            (false, 1.0)
        };
        let size = ui::capsule::size_for(minimal, pointing, self.state.hud != HudState::Idle);
        let target = size.points();
        let (width, height) = (target.x, target.y);
        if minimal {
            // Polling the pointer means an idle LocalFlow in minimal mode
            // wakes ten times a second rather than sleeping until an event.
            // That is the price of the proximity fade, and it is paid only
            // while minimal mode is on, which is not the default. This is the
            // one thing that is genuinely specific to minimal mode being on;
            // the resize below is not, and must not be gated the same way.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // Resized only on an actual transition, never merely because minimal
        // mode is on or off. Gating this on `minimal` instead of on the size
        // actually changing was tried and was wrong: turning minimal mode off
        // while the window was a bead left it a bead forever, because
        // `size_for` was already back to reporting `Full` but nothing was
        // left to apply it. `applied_size` is seeded at construction from the
        // size the window actually starts at, so "already the right size" is
        // true from the first frame in both the minimal and the full case,
        // and this block runs at all only when the window is not already
        // what `size` calls for, which includes both the drag-off-an-edge
        // case (identical size, nothing sent) and the toggle-off-while-a-bead
        // case (differing size, applied once).
        //
        // The window is snapped straight to the chosen size rather than
        // tweened towards it. A tween would leave the window and the capsule
        // `show` paints disagreeing about the size for the duration of the
        // animation, which is either clipped content on a grow or a capsule
        // floating inside an oversized window on a shrink. `work_areas` is
        // only asked inside this same branch, since a per-frame
        // `NSScreen::screens` call for a size that has not changed buys
        // nothing.
        if self.applied_size != Some(size) {
            if let Some(centre) = self.centre {
                let (x, y) = crate::window_position::place(
                    centre,
                    (width, height),
                    &crate::platform::work_areas(),
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, height)));
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
                self.applied_size = Some(size);
            }
        }

        // A console buried behind other windows is exactly when someone
        // reaches for the menu item, so opening it also raises it.
        let mut raise_console = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let response =
                    ui::capsule::show(ui, &self.state, ui.input(|i| i.time), size, opacity);
                self.dragging = response.dragging;
                if let Some(action) = response.action {
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
                            let centre = crate::window_position::Centre {
                                x: position.x + width / 2.0,
                                y: position.y + height / 2.0,
                            };
                            self.centre = Some(centre);
                            crate::window_position::save(&self.data_dir, centre);
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
            let microphone_name = self.microphone.as_ref().map(|m| m.device_name());
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
                    ui::console::show(ctx, state, &data_dir, microphone_name)
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

    /// The bead fades as the pointer moves away, and stops fading at a floor.
    /// It must never reach zero: LocalFlow has no Dock icon and no menu bar
    /// item, so a bead that can become invisible is an application with no
    /// way back, which is the same failure as a capsule restored off screen.
    #[test]
    fn the_bead_fades_with_distance_but_never_disappears() {
        assert_eq!(bead_opacity(0.0), 1.0);
        assert_eq!(bead_opacity(NEAR_RADIUS), 1.0);
        assert_eq!(bead_opacity(FAR_RADIUS), BEAD_OPACITY_FLOOR);
        assert_eq!(bead_opacity(10_000.0), BEAD_OPACITY_FLOOR);
        let middle = bead_opacity((NEAR_RADIUS + FAR_RADIUS) / 2.0);
        assert!(middle > BEAD_OPACITY_FLOOR && middle < 1.0);
        // The floor-is-never-zero invariant is a compile-time assertion next
        // to the constant, not a runtime one here: see BEAD_OPACITY_FLOOR.
    }

    /// Monotonic, so the bead never brightens as the pointer retreats.
    #[test]
    fn the_bead_never_brightens_as_the_pointer_moves_away() {
        let mut previous = bead_opacity(0.0);
        for step in 1..=60 {
            let opacity = bead_opacity(step as f32 * 10.0);
            assert!(opacity <= previous, "opacity rose at {step}");
            previous = opacity;
        }
    }
}
