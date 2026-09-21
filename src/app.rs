use crate::audio::{CapturedAudio, Microphone};
use crate::platform::{
    frontmost_application_pid, insert_text, GlobalHotkey, HotkeyEvent, Insertion,
};
use crate::router::{KevWorker, Reply, WorkerShutdown};
use crate::state::{AppState, Failure, HudState, Route, Timings, WorkerStatus};
use crate::ui;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver as HotkeyReceiver;
use std::time::{Duration, Instant};

/// How long the capsule takes to change size. This is drawing rather than an
/// operating system window resize, so it is eased at the display's rate.
const GROW_SECONDS: f32 = 0.18;

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
    /// The utterance's WAV filename stem, which the worker sees as part of the
    /// audio path and records against its own trace. Correlating on this rather
    /// than on timestamps or arrival order means a dropped or reordered record
    /// cannot silently pair the wrong halves of a dictation.
    trace_id: Option<String>,
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
    /// Stops the Python worker when the application goes away. The worker
    /// lives on a detached thread whose stack is never unwound at process
    /// exit, so its own `Drop` cannot be relied on to do it.
    worker_shutdown: WorkerShutdown,
    /// The window's current size. It follows the minimal mode setting and
    /// nothing else, so it changes only when the user toggles that, never
    /// while the capsule is animating between its three painted sizes.
    window_size: egui::Vec2,
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
        // that. Done here because the window exists by the time this runs.
        // The console is left alone by name rather than by timing: the
        // replacement identifies the capsule and defers for every other
        // window, so it does not matter that the console is created later.
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
                "The capsule could not be stopped from taking keyboard focus. \
                 Clicking it will move focus away from what you are writing in, \
                 and the next dictation will report no text field focused.",
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
        let worker_shutdown = WorkerShutdown::default();
        start_pipeline_worker(
            work_rx,
            result_tx,
            audio_dir,
            cc.egui_ctx.clone(),
            worker_shutdown.clone(),
        );
        let centre = crate::window_position::load(&data_dir);
        // Matches what main.rs already decided the window starts at, from the
        // same setting and the same `size_for` rule. Seeded rather than left
        // `None` so "already the right size" is true from the very first
        // frame: an unseeded `None` would read as a change on frame one and
        // immediately resize a window that was already correct.
        let window_size = ui::theme::window_size(state.settings.minimal_mode);
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
            window_size,
            worker_shutdown,
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

    /// Resize the window when, and only when, the minimal mode setting has
    /// changed.
    ///
    /// Minimal mode needs a catchment larger than the capsule so it can
    /// notice someone approaching. Minimal mode off needs no such thing, and
    /// giving it one would mean the default setting quietly swallowed clicks
    /// in a ring of screen the capsule does not visibly occupy. So the window
    /// follows the setting. It does not follow the capsule's painted size,
    /// which is what makes the animation free.
    fn follow_setting_with_the_window(&mut self, ctx: &egui::Context) {
        let wanted = ui::theme::window_size(self.state.settings.minimal_mode);
        if wanted == self.window_size {
            return;
        }
        let Some(centre) = self.centre else {
            return;
        };
        // Placed from the visible capsule rather than from the window, so the
        // catchment's invisible ring is never what pushes the capsule away
        // from a screen edge the user put it against.
        let capsule = ui::theme::CAPSULE_SIZE;
        let (x, y) = crate::window_position::place(
            centre,
            (capsule.x, capsule.y),
            &crate::platform::work_areas(),
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(wanted));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            x - (wanted.x - capsule.x) / 2.0,
            y - (wanted.y - capsule.y) / 2.0,
        )));
        self.window_size = wanted;
    }

    /// What the capsule should be painted as, and how solid.
    ///
    /// Returns the painted size, which is animated and so is usually between
    /// the three fixed sizes. The capsule works out its own layout from it.
    ///
    /// The window never changes size, so nothing here touches the viewport.
    /// The pointer comes from egui rather than from the screen, because the
    /// window is now the catchment and receives real move events across the
    /// whole of it, including the parts it does not paint.
    fn choose_shape(&mut self, ctx: &egui::Context) -> egui::Vec2 {
        let minimal = self.state.settings.minimal_mode;
        let window = ctx.screen_rect();
        // The capsule sits in the middle of the catchment, and this is the
        // rectangle the user is reaching for. Entering it expands the
        // capsule. The ring outside it is the lead-in: it is what lets the
        // window see a pointer coming before it arrives, and it is also the
        // part that swallows clicks without ever painting anything.
        let reach = egui::Rect::from_center_size(window.center(), ui::theme::CAPSULE_SIZE);
        let pointer = ctx.input(|i| i.pointer.hover_pos());
        let pointing = match (minimal, pointer) {
            (false, _) => false,
            (true, Some(pos)) => reach.contains(pos) || self.dragging,
            // No pointer at all means it is outside the catchment entirely.
            (true, None) => self.dragging,
        };
        // A dictation that has not yet retired counts as active, with one
        // exception: a startup failure deliberately clears `done_at` so it
        // never retires on its own. Without carving that out, `active` would
        // stay true for the rest of the session, pinning a minimal-mode
        // capsule at the dictating size forever with no user action behind
        // it, the governing rule inverted. Every mid-dictation failure goes
        // through `record_failure` and then `settle`, which sets `done_at`.
        let active = self.state.hud != HudState::Idle
            && !(self.state.hud == HudState::Error && self.state.done_at.is_none());
        let size = ui::capsule::size_for(minimal, pointing, active);
        // Animated, because this is now drawing rather than an operating
        // system window resize. Both axes are eased on the same clock, so the
        // capsule cannot shear.
        let target = size.points();
        egui::vec2(
            ctx.animate_value_with_time(egui::Id::new("capsule_width"), target.x, GROW_SECONDS),
            ctx.animate_value_with_time(egui::Id::new("capsule_height"), target.y, GROW_SECONDS),
        )
    }
}

impl eframe::App for LocalFlowApp {
    /// The capsule paints its own shape into a transparent window, so the
    /// window itself must contribute nothing. eframe's default clear colour
    /// is a 70% opaque near-black across the whole viewport, which would show
    /// up as a rectangle around the capsule's rounded corners, and now that
    /// the window is a catchment much larger than the capsule it would show
    /// up as a rectangle around a great deal of empty space.
    /// Stop the worker before the process goes away.
    ///
    /// Nothing else will. `KevWorker` kills its child when it drops, but it
    /// is owned by a detached thread that is never unwound at exit, and for
    /// the first ten seconds that thread is blocked inside `start` loading
    /// models where it cannot see its channel close.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.worker_shutdown.stop();
    }

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

        self.follow_setting_with_the_window(ctx);
        let painted = self.choose_shape(ctx);

        // A console buried behind other windows is exactly when someone
        // reaches for the menu item, so opening it also raises it.
        let mut raise_console = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let response =
                    ui::capsule::show(ui, &self.state, ui.input(|i| i.time), painted);
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
                            // Converted with the size of the window that was
                            // actually dragged, not with the catchment's.
                            // Minimal mode off gives a window barely larger
                            // than the capsule, and using the catchment's
                            // size there put the remembered centre tens of
                            // points adrift, once per drag, compounding
                            // across restarts.
                            let centre = crate::window_position::centre_of_window(
                                (position.x, position.y),
                                (self.window_size.x, self.window_size.y),
                            );
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
    shutdown: WorkerShutdown,
) {
    std::thread::Builder::new()
        .name("localflow-pipeline".into())
        .spawn(move || {
            // The Python process loads mlx-whisper and the exact pool_300 Kev
            // checkpoint once, then remains resident for the app lifetime.
            let mut worker = KevWorker::start().map_err(|error| error.to_string());
            match &worker {
                Ok(worker) => {
                    // Registered the moment it exists, so quitting during the
                    // ten seconds of model loading still stops it.
                    shutdown.watch(worker);
                    let _ = result_tx.send(PipelineMessage::WorkerReady);
                }
                Err(error) => {
                    let _ = result_tx.send(PipelineMessage::WorkerFailed(error.clone()));
                }
            }
            repaint.request_repaint();
            for item in work_rx {
                let message = process(&mut worker, &audio_dir, item);
                // Traced before the send only in the sense of being prepared
                // here; the UI is told first, because a diagnostic must never
                // sit between a finished dictation and the capsule showing it.
                let trace = latency_trace_for(&message);
                let _ = result_tx.send(message);
                repaint.request_repaint();
                if let Some((id, outcome, route, timings)) = trace {
                    crate::latency_trace::record(&crate::latency_trace::LatencyTrace {
                        trace_id: &id,
                        captured_at: chrono::Utc::now().to_rfc3339(),
                        outcome,
                        route,
                        timings: &timings,
                    });
                }
            }
        })
        .expect("Could not start LocalFlow pipeline worker");
}

/// Everything a finished dictation contributes to the latency dataset, or
/// nothing when it never got far enough to have an id to correlate on.
fn latency_trace_for(
    message: &PipelineMessage,
) -> Option<(String, &'static str, Option<Route>, Timings)> {
    let PipelineMessage::Finished(result) = message else {
        return None;
    };
    let id = result.trace_id.clone()?;
    let outcome = match result.outcome {
        Outcome::Inserted(Insertion::Pasted) => "inserted",
        Outcome::Inserted(Insertion::CopiedOnly) => "copied",
        Outcome::Failed(_) => "failed",
    };
    Some((id, outcome, result.route, result.timings.clone()))
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
                // No WAV was written, so there is nothing produced and no id
                // for a worker record to pair with.
                Partial::default(),
                "Transcription failed",
                error.to_string(),
            )
        }
    };
    timings.capture_finalize_ms = Some(finalize_started.elapsed().as_millis());
    timings.audio_ms = Some(audio.duration.as_millis());
    let trace_id = audio
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_owned());

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
        // The pipeline failed after recognising speech. The failure stands,
        // and the words are not thrown away with it.
        Ok(Reply::Failed { message, transcript }) => {
            let words = transcript.unwrap_or_default();
            let preserved = preserve(&words, || Preserved::Raw);
            return failed(
                timings,
                speech_finished,
                Partial { transcript: words, trace_id, ..Default::default() },
                "Dictation failed",
                failure_detail(&message, &preserved),
            );
        }
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                Partial { trace_id, ..Default::default() },
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
                Partial {
                    transcript: inference.transcript,
                    route: Some(inference.route),
                    output: inference.output.clone(),
                    trace_id,
                },
                "Couldn't insert",
                // Processing succeeded, so what is preserved is the finished
                // text rather than the raw transcription.
                failure_detail(
                    "No destination app was focused when dictation started.",
                    &preserve(&inference.output, || Preserved::Processed),
                ),
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
                Partial {
                    transcript: inference.transcript,
                    route: Some(inference.route),
                    output: String::new(),
                    trace_id,
                },
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
        trace_id,
    }))
}

/// What became of the user's words when the pipeline failed after producing
/// them. Preserving is not inserting: text that failed its processing is never
/// typed into the document as though it had succeeded.
#[derive(Debug, PartialEq, Eq)]
pub enum Preserved {
    /// The raw transcription, because processing never completed.
    Raw,
    /// The finished text, which processing produced but insertion could not place.
    Processed,
    /// The words could not even be put on the pasteboard.
    Unavailable(String),
}

/// The detail the console shows for a failure that happened after the user's
/// words existed.
///
/// The original failure always leads: preservation is something that also
/// happened, never a replacement for the reason. A failed preservation is
/// reported alongside rather than swallowed, and raw text is never described
/// as though it had been processed.
pub fn failure_detail(original: &str, preserved: &Preserved) -> String {
    match preserved {
        Preserved::Raw => format!(
            "{original} Your words are on the clipboard: press Cmd-V to place them. \
             This is the raw transcription, not the processed text."
        ),
        Preserved::Processed => format!(
            "{original} The finished text is on the clipboard: press Cmd-V to place it."
        ),
        Preserved::Unavailable(why) => format!(
            "{original} The words could not be put on the clipboard either: {why}"
        ),
    }
}

/// Put a failed dictation's words somewhere the user can reach them.
///
/// Reports what happened rather than returning a Result, because a failure
/// here must never replace the failure that lost the dictation.
fn preserve(text: &str, kind: fn() -> Preserved) -> Preserved {
    if text.is_empty() {
        return Preserved::Unavailable("there was no text to preserve".to_owned());
    }
    match crate::platform::copy_to_pasteboard(text) {
        Ok(()) => kind(),
        Err(error) => Preserved::Unavailable(format!("{error:#}")),
    }
}

/// Whatever a lost dictation did manage to produce before it was lost. Empty
/// for a failure early enough that nothing had been produced yet.
#[derive(Default)]
struct Partial {
    transcript: String,
    route: Option<Route>,
    output: String,
    trace_id: Option<String>,
}

fn failed(
    mut timings: Timings,
    speech_finished: Instant,
    partial: Partial,
    headline: &'static str,
    error: String,
) -> PipelineMessage {
    timings.total_ms = Some(speech_finished.elapsed().as_millis());
    PipelineMessage::Finished(Box::new(WorkResult {
        transcript: partial.transcript,
        route: partial.route,
        output: partial.output,
        timings,
        // Every pipeline failure happens after the user has spoken, so the
        // kind is settled here: the words did not come back.
        outcome: Outcome::Failed(Failure::dropped(headline, error)),
        trace_id: partial.trace_id,
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

#[cfg(test)]
mod preservation_tests {
    use super::*;

    /// The failure that lost the dictation is what the user needs to read.
    /// Preservation is something that also happened, never a replacement.
    #[test]
    fn the_original_failure_leads_and_preservation_follows() {
        let detail = failure_detail("S1-mini failed to process this LIGHT_CLEANUP utterance.",
                                    &Preserved::Raw);
        assert!(detail.starts_with("S1-mini failed to process this LIGHT_CLEANUP utterance."));
        assert!(detail.contains("clipboard"));
    }

    /// Raw text must never be described as though it had been processed. The
    /// user is deciding whether to paste it, and that decision needs the truth.
    #[test]
    fn raw_text_is_not_presented_as_processed() {
        let detail = failure_detail("Routing failed.", &Preserved::Raw);
        assert!(detail.contains("raw transcription"));
        assert!(!detail.contains("finished text"));
    }

    #[test]
    fn processed_text_is_described_as_finished() {
        let detail = failure_detail("No destination app was focused.", &Preserved::Processed);
        assert!(detail.contains("finished text"));
        assert!(!detail.contains("raw transcription"));
    }

    /// A clipboard failure is a second problem, not a replacement for the first.
    #[test]
    fn a_preservation_failure_is_reported_beside_the_original_not_instead_of_it() {
        let detail = failure_detail(
            "COMPLEX processing is not implemented yet.",
            &Preserved::Unavailable("Could not access macOS pasteboard".to_owned()),
        );
        assert!(detail.contains("COMPLEX processing is not implemented yet."),
                "the original failure must survive a failed preservation");
        assert!(detail.contains("Could not access macOS pasteboard"));
    }

    /// An ASR failure has nothing to preserve, and must not claim otherwise.
    #[test]
    fn nothing_is_claimed_when_there_were_no_words() {
        let preserved = preserve("", || Preserved::Raw);
        assert_eq!(preserved, Preserved::Unavailable("there was no text to preserve".to_owned()));
        let detail = failure_detail("Transcription failed.", &preserved);
        assert!(!detail.contains("press Cmd-V"));
    }
}
