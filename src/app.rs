use crate::audio::{CapturedAudio, Microphone};
use crate::insertion::CursorMemory;
use crate::platform::{
    frontmost_application_pid, insert_text, CursorMoved, GlobalHotkey, HotkeyEvent, Insertion,
};
use crate::router::{KevWorker, Reply, WorkerShutdown};
use crate::state::{AppState, Failure, HudState, Preserved, Route, Timings, WorkerStatus};
use crate::ui;
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver as HotkeyReceiver;
use std::time::{Duration, Instant};

/// How long the bead takes to fade between faint and solid. Opacity has no
/// momentum worth modelling, so this is a plain fade rather than a spring.
const PRESENCE_SECONDS: f32 = 0.25;

/// How solid the bead is drawn while nothing is happening and nobody is
/// pointing at it. Faint enough to stay out of the way, solid enough to find.
const RESTING_PRESENCE: f32 = 0.55;

/// Matches the capture buffer's ceiling. Reaching it stops the recording; it
/// does not throw away what was captured.
const MAX_RECORDING_DURATION: Duration = Duration::from_secs(300);

/// How long a dictation must be in the pipeline before the capsule says so.
/// Long enough that an answer arriving almost immediately, which is what a
/// capture with no speech in it does, replaces the listening state directly
/// instead of flashing Transcribing on the way past. Short enough that a real
/// dictation, which takes about a second, still reads as instant feedback.
const PROCESSING_ANNOUNCE_DELAY: Duration = Duration::from_millis(120);


/// Why capture stopped, which decides whether the finished text may be typed
/// into the user's document.
///
/// Only a deliberate release may insert. A ceiling or a full buffer means the
/// capture ended without the user asking it to, and quietly typing five
/// minutes of whatever a stuck key overheard would be worse than making them
/// paste it themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureEnd {
    /// The user let go of the key. The ordinary case.
    Released,
    /// The recording ceiling stopped it.
    CeilingReached,
    /// The buffer filled, so the end of the dictation was never captured.
    BufferFull,
}

impl CaptureEnd {
    pub fn may_insert(self) -> bool {
        matches!(self, CaptureEnd::Released)
    }

    /// What to tell the user, for the endings that need explaining.
    pub fn headline(self) -> &'static str {
        match self {
            CaptureEnd::Released => "Couldn't insert",
            CaptureEnd::CeilingReached => "Recording hit 5 min",
            CaptureEnd::BufferFull => "Recording filled the buffer",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            CaptureEnd::Released => "No destination app was focused when dictation started.",
            CaptureEnd::CeilingReached =>
                "The five minute recording limit was reached, so the recording was stopped \
                 and what you said was transcribed.",
            CaptureEnd::BufferFull =>
                "The recording filled its buffer, so the end of what you said was not \
                 captured. What was captured has been transcribed.",
        }
    }
}

/// Where a finished dictation is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// Into this process, if what it has focused will take text.
    Insert(i32),
    /// Onto the clipboard, because nothing was focused that could receive it.
    /// Not a failure: the user spoke, the words came back, and they are one
    /// Cmd-V from where they were wanted.
    Clipboard,
    /// Nowhere, because the capture ended before the user had finished
    /// speaking. The words that were caught are still preserved, but this is
    /// reported as the lost dictation it is.
    Lost,
}

/// What to do with a finished dictation, given where it was aimed and how its
/// capture ended.
///
/// The two reasons a dictation does not get pasted are deliberately kept
/// apart. Having nowhere to put the text is an ordinary thing that happens
/// when the user dictates with LocalFlow's own window in front, or with
/// nothing focused at all, and refusing it was worse than answering it. A
/// capture cut short by the ceiling or the buffer is a dictation the user did
/// not finish, and calling that a success would claim something LocalFlow did
/// not do.
fn destination(target_pid: Option<i32>, capture_end: CaptureEnd) -> Destination {
    if !capture_end.may_insert() {
        return Destination::Lost;
    }
    match target_pid {
        Some(pid) => Destination::Insert(pid),
        None => Destination::Clipboard,
    }
}

/// What the pipeline thread is asked to do, in the order it was asked.
///
/// One channel for both, so a warm-up can never overtake the dictation it was
/// sent ahead of.
enum Work {
    /// The user has started speaking. Make the router resident before the
    /// dictation needs it.
    Prepare,
    Dictation(WorkItem),
}

struct WorkItem {
    captured: CapturedAudio,
    capture_end: CaptureEnd,
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
    /// What the user can reach on the clipboard, for a dictation that failed
    /// after its words existed. Carried as its own answer rather than read
    /// back out of the failure's prose, because the toast has to show the
    /// text that is actually there: the raw transcription and the finished
    /// text are different strings, and only one of them is on the clipboard.
    preserved: Option<Preserved>,
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
    /// Speech that could not be decoded, with what the console shows about it.
    NotUnderstood {
        note: String,
    },
}

pub struct LocalFlowApp {
    state: AppState,
    microphone: Option<Microphone>,
    hotkey_events: HotkeyReceiver<HotkeyEvent>,
    _hotkey: GlobalHotkey,
    work_tx: Sender<Work>,
    result_rx: Receiver<PipelineMessage>,
    recording_started: Option<Instant>,
    /// Whether this press has already been called quiet, so the warning is
    /// raised once rather than on every frame it remains true.
    warned_quiet: bool,
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
    /// The capsule's painted width and height, each eased by its own spring
    /// on the same feel so the shape cannot shear.
    capsule_width: ui::motion::Spring,
    capsule_height: ui::motion::Spring,
    /// Smooths the microphone level into the listening bars.
    voice_meter: ui::meter::VoiceMeter,
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
                        GlobalHotkey::default(),
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
        // to go, so it lands on the clipboard instead of at the cursor and
        // looks like the user's mistake. It is reported last because the other
        // two stop dictation outright.
        let focus_error = (!capsule_non_activating).then(|| {
            Failure::blocked(
                "Capsule takes focus",
                "The capsule could not be stopped from taking keyboard focus. \
                 Clicking it will move focus away from what you are writing in, \
                 and the next dictation will be copied to the clipboard rather \
                 than typed where you wanted it.",
            )
        });
        if let Some(failure) =
            hotkey_error.or(microphone_error).or(focus_error).or(settings_error)
        {
            // The dot points at the console, so the console has to have
            // something to show when the user follows it. Clearing the dwell
            // timer keeps a startup failure on the capsule indefinitely: there
            // is no working state for it to decay back into. Nothing has been
            // dictated yet, so nothing is waiting on the clipboard either.
            state.record_failure(failure, None);
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
            hotkey.cursor_moved(),
        );
        let centre = crate::window_position::load(&data_dir);
        // Matches what main.rs already decided the window starts at, from the
        // same setting and the same `size_for` rule. Seeded rather than left
        // `None` so "already the right size" is true from the very first
        // frame: an unseeded `None` would read as a change on frame one and
        // immediately resize a window that was already correct.
        let window_size = ui::theme::window_size(state.settings.minimal_mode);
        let resting =
            ui::capsule::size_for(state.settings.minimal_mode, false, false).points();
        Self {
            state,
            microphone,
            hotkey_events,
            _hotkey: hotkey,
            work_tx,
            result_rx,
            recording_started: None,
            warned_quiet: false,
            target_pid: None,
            data_dir,
            centre,
            dragging: false,
            window_size,
            worker_shutdown,
            capsule_width: ui::motion::Spring::new(resting.x),
            capsule_height: ui::motion::Spring::new(resting.y),
            voice_meter: ui::meter::VoiceMeter::default(),
        }
    }

    fn start_recording(&mut self) {
        // A dictation already in flight owns the microphone and the capsule,
        // so a second press is ignored rather than allowed to restart either.
        if matches!(self.state.hud, HudState::Listening | HudState::Processing) {
            return;
        }
        // LocalFlow's own window is not a destination, and neither is no
        // window at all. Neither is refused: the dictation runs, and what it
        // produces goes to the clipboard with a notice, which is what the
        // user wanted from pressing the key. Refusing here used to throw the
        // words away to report a condition the user could see for themselves.
        let target_pid =
            frontmost_application_pid().filter(|pid| *pid != std::process::id() as i32);
        let Some(microphone) = &self.microphone else {
            self.fail_locally("start_recording", Failure::input_unavailable(
                "Microphone unavailable",
                "The microphone is unavailable; restart LocalFlow",
            ));
            return;
        };
        // The stream was built at startup, so this only restarts it. Opening
        // the device here would cost over a hundred milliseconds of speech...
        if let Err(error) = microphone.start_recording() {
            self.fail_locally("start_recording",
                Failure::input_unavailable("Microphone unavailable", error.to_string()));
            return;
        }
        self.state.reset_for_recording();
        self.voice_meter.reset();
        self.target_pid = target_pid;
        self.recording_started = Some(Instant::now());
        self.warned_quiet = false;
        // Only once the worker is up. While it is still loading, its models
        // were just touched and are resident anyway, and a warm-up would only
        // queue in front of the dictation. A closed channel is not reported
        // here: the dictation's own send meets it and says so.
        if self.state.worker == WorkerStatus::Ready {
            let _ = self.work_tx.send(Work::Prepare);
        }
    }

    fn finish_recording(&mut self, ending: CaptureEnd) {
        if self.recording_started.take().is_none() {
            return;
        }
        let Some(microphone) = &self.microphone else {
            return;
        };
        let speech_finished = Instant::now();
        // Only the microphone stream is stopped here. Draining the capture
        // buffer and encoding the WAV happen on the pipeline thread, so
        // releasing the hotkey never janks the HUD or delays the next press...
        match microphone.stop_recording() {
            Ok(captured) => {
                // Answered here rather than anywhere further in, because a
                // press that held no speech is the one case where the user is
                // waiting to be told that nothing happened. Everything past
                // this point is a queue behind the previous dictation's
                // worker call, and this measurement costs one read of samples
                // that are already in memory.
                match captured.inspect() {
                    Ok(crate::audio::Verdict::TooQuiet { seconds, rms, peak }) => {
                        crate::latency_trace::record_silence(
                            seconds,
                            rms,
                            peak,
                            captured.device_name(),
                        );
                        // Thrown away where it lies, or its samples would be
                        // prepended to whatever is said next.
                        if let Err(error) = captured.discard() {
                            self.fail_locally("finish_recording", Failure::dropped(
                                "Recording failed",
                                error.to_string(),
                            ));
                            return;
                        }
                        self.state.record_no_speech();
                        return;
                    }
                    Ok(crate::audio::Verdict::Speech { .. }) => {}
                    // The measurement could not be taken, which says nothing
                    // about whether there is speech in there. The capture
                    // goes on to the pipeline, which measures it again on its
                    // own terms.
                    Err(error) => eprintln!("LocalFlow could not measure a capture: {error:#}"),
                }
                self.state.begin_processing();
                // A buffer that filled is still a dictation, just a shortened
                // one, so it travels as an ending rather than an error.
                let capture_end = if captured.truncated {
                    CaptureEnd::BufferFull
                } else {
                    ending
                };
                if self
                    .work_tx
                    .send(Work::Dictation(WorkItem {
                        captured,
                        capture_end,
                        speech_finished,
                        queued_at: Instant::now(),
                        target_pid: self.target_pid.take(),
                    }))
                    .is_err()
                {
                    self.fail_locally("finish_recording", Failure::dropped(
                        "Transcription failed",
                        "The processing worker stopped unexpectedly",
                    ));
                }
            }
            Err(error) => self.fail_locally(
                "finish_recording",
                Failure::dropped("Recording failed", error.to_string()),
            ),
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
                PipelineMessage::NotUnderstood { note } => {
                    self.state.record_not_understood(note)
                }
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
            Outcome::Failed(failure) => self.fail(failure, result.preserved),
            // A dictation whose destination went away is still a success from
            // the user's side: the words exist and are on the pasteboard. It
            // is reported as its own state rather than as either a clean
            // insert or a failure, because it is neither.
            Outcome::Inserted(insertion) => self.state.record_inserted(insertion),
        }
    }

    fn fail(&mut self, failure: Failure, preserved: Option<Preserved>) {
        self.state.record_failure(failure, preserved);
    }

    /// Fail before any work reached the pipeline, and leave a record of it.
    ///
    /// The pipeline traces its own results and the worker traces its own
    /// failures; a dictation refused here left nothing behind at all.
    fn fail_locally(&mut self, stage: &'static str, failure: Failure) {
        crate::latency_trace::record_app_failure(stage, &failure);
        // A dictation refused here never produced words, so there is nothing
        // on the clipboard to tell the user about.
        self.fail(failure, None);
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
    /// the three fixed sizes, and how solid to draw it. The capsule works out
    /// its own layout from the size.
    ///
    /// The window never changes size, so nothing here touches the viewport.
    /// The pointer comes from egui rather than from the screen, because the
    /// window is now the catchment and receives real move events across the
    /// whole of it, including the parts it does not paint.
    fn choose_shape(&mut self, ctx: &egui::Context) -> (egui::Vec2, f32) {
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
        // Only the recording itself grows the capsule. Once the key is let go
        // the bead is enough to carry transcribing and the result in colour,
        // and the hud is not a substitute for this: it stays Listening for a
        // moment after release, until transcription is worth announcing.
        let recording = self.recording_started.is_some();
        let size = ui::capsule::size_for(minimal, pointing, recording);
        // Sprung rather than tweened, so growing pops open and a change of
        // mind part way turns around smoothly. The feel is chosen from the
        // width, and both axes share it so they stay in step.
        let target = size.points();
        let feel = if target.x >= self.capsule_width.value() {
            ui::motion::GROW
        } else {
            ui::motion::SHRINK
        };
        let seconds = ctx.input(|i| i.stable_dt).min(1.0 / 20.0);
        let width_moving = self.capsule_width.step(target.x, seconds, feel);
        let height_moving = self.capsule_height.step(target.y, seconds, feel);
        if width_moving || height_moving {
            ctx.request_repaint();
        }
        // Faint only when the bead is simply resting. An unread failure keeps
        // it solid, because a tinted bead is the only place that failure can
        // still be seen.
        let resting = size == ui::capsule::CapsuleSize::Bead
            && self.state.hud == HudState::Idle
            && !self.state.unread_failure;
        let presence = ctx.animate_value_with_time(
            egui::Id::new("capsule_presence"),
            if resting { RESTING_PRESENCE } else { 1.0 },
            PRESENCE_SECONDS,
        );
        (
            egui::vec2(self.capsule_width.value(), self.capsule_height.value()),
            presence,
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
                HotkeyEvent::Released => self.finish_recording(CaptureEnd::Released),
            }
        }
        self.receive_results();
        self.state.announce_processing(PROCESSING_ANNOUNCE_DELAY);
        if self
            .recording_started
            .is_some_and(|started| started.elapsed() >= MAX_RECORDING_DURATION)
        {
            // The ceiling stops the recording, which is its whole purpose. It
            // used to discard the audio too, on the stated grounds of bounding
            // memory - but the buffer is preallocated, so the memory was spent
            // the moment the app started and discarding bought nothing. What
            // it cost was five minutes of someone's words.
            self.finish_recording(CaptureEnd::CeilingReached);
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
        let frame_seconds = ctx.input(|i| i.stable_dt).min(1.0 / 20.0);
        if let Some(started) = self.recording_started {
            if let Some(microphone) = &self.microphone {
                self.state.voice_bars =
                    self.voice_meter.update(microphone.level(), frame_seconds);
                // Said once per press. The measurement keeps falling while the
                // user reads it, and a toast that re-raised itself every frame
                // would never finish appearing.
                if !self.warned_quiet
                    && crate::audio::heading_for_refusal(
                        started.elapsed(),
                        microphone.recorded_rms(),
                    )
                {
                    self.warned_quiet = true;
                    self.state.warn_quiet();
                }
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.state.hud == HudState::Listening {
            // Released, but not yet announced as transcribing. The bars fall
            // away with the voice instead of freezing where it left them.
            self.state.voice_bars = self.voice_meter.update(0.0, frame_seconds);
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        if self.state.hud == HudState::Processing {
            // The transcribing wave travels, and at a slower rate it steps.
            ctx.request_repaint_after(Duration::from_millis(16));
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
        let (painted, presence) = self.choose_shape(ctx);

        // A console buried behind other windows is exactly when someone
        // reaches for the menu item, so opening it also raises it.
        let mut raise_console = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let response =
                    ui::capsule::show(ui, &self.state, ui.input(|i| i.time), painted, presence);
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

        // Painted after the capsule so it is placed from the centre this
        // frame's drag may just have changed, and retired first so a toast
        // whose time is up never gets one more frame of window.
        self.state.retire_toast(ui::toast::DWELL);
        if let (Some(toast), Some(centre)) = (self.state.toast.clone(), self.centre) {
            ui::toast::show(ctx, &toast, centre, &crate::platform::work_areas());
            // Nothing else wakes the UI while a toast is up: the dictation it
            // describes has already finished, so without this the fade would
            // stop on whatever frame the capsule last needed.
            ctx.request_repaint_after(Duration::from_millis(16));
        }

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
    work_rx: Receiver<Work>,
    result_tx: Sender<PipelineMessage>,
    audio_dir: PathBuf,
    repaint: egui::Context,
    shutdown: WorkerShutdown,
    cursor_moved: CursorMoved,
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
            // Lives with the worker rather than the app because this is the
            // thread that inserts text, and it handles one dictation at a
            // time, so the memory needs no lock to stay consistent.
            let mut cursor = CursorMemory::default();
            for work in work_rx {
                let item = match work {
                    Work::Prepare => {
                        prepare(&mut worker);
                        continue;
                    }
                    Work::Dictation(item) => item,
                };
                let message = process(&mut worker, &audio_dir, item, &mut cursor, &cursor_moved);
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
        // Kept apart from "copied": one means the user switched away, the
        // other means LocalFlow declined to paste, and a trace that merged
        // them could not tell which of the two the new check is causing.
        Outcome::Inserted(Insertion::CopiedNoField) => "copied_no_field",
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

/// Warm the router ahead of a dictation.
///
/// A failure here costs the dictation behind it speed and nothing else, so it
/// is reported on stderr rather than to the user. The dictation still runs and
/// reports its own outcome, and a worker the warm-up found broken has already
/// been poisoned, so that dictation fails with the reason.
fn prepare(worker: &mut Result<KevWorker, String>) {
    let Ok(worker) = worker else {
        return;
    };
    if let Err(error) = worker.prepare() {
        eprintln!("LocalFlow could not prepare the router for this dictation: {error:#}");
    }
}

fn process(
    worker: &mut Result<KevWorker, String>,
    audio_dir: &Path,
    item: WorkItem,
    cursor: &mut CursorMemory,
    cursor_moved: &CursorMoved,
) -> PipelineMessage {
    let speech_finished = item.speech_finished;
    let mut timings = Timings {
        queue_ms: Some(item.queued_at.elapsed().as_millis()),
        ..Default::default()
    };
    let target_pid = item.target_pid;
    let capture_end = item.capture_end;
    let finalize_started = Instant::now();
    let device_name = item.captured.device_name().to_owned();
    let audio = match item.captured.finish(audio_dir) {
        Ok(crate::audio::Finished::Recorded(audio)) => audio,
        // Nothing was said, and nothing was written: the capture answered for
        // itself before the worker was involved. Recorded as metadata so the
        // floor can be reviewed against real presses, never as a dictation.
        Ok(crate::audio::Finished::TooQuiet { seconds, rms, peak }) => {
            crate::latency_trace::record_silence(seconds, rms, peak, &device_name);
            return PipelineMessage::NoSpeech;
        }
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                // No WAV was written, so there is nothing produced and no id
                // for a worker record to pair with.
                Partial::default(),
                "Transcription failed",
                error.to_string(),
                None,
            )
        }
    };
    timings.capture_finalize_ms = Some(finalize_started.elapsed().as_millis());
    crate::latency_trace::record_capture(
        audio.duration.as_secs_f64(),
        audio.rms,
        audio.peak,
        &device_name,
    );
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
        // Heard and not decoded. The words are gone either way, but the user
        // said something, so the app says so rather than settling back as
        // though the key had never been pressed.
        Ok(Reply::Unintelligible { seconds, confidence }) => {
            return PipelineMessage::NotUnderstood {
                note: format!(
                    "Heard {seconds:.1} s and could not decode it (confidence                      {confidence:.2}). Nothing was inserted."
                ),
            }
        }
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
                Some(preserved),
            );
        }
        Err(error) => {
            return failed(
                timings,
                speech_finished,
                Partial { trace_id, ..Default::default() },
                "Transcription failed",
                error.to_string(),
                None,
            )
        }
    };
    timings.asr_ms = Some(inference.asr_ms);
    timings.router_ms = Some(inference.router_ms);

    // The worker decides what each route produces, including refusing an
    // unimplemented one, so there is a single place that maps route to text.
    timings.transform_ms = inference.processing_ms;

    let insert_started = Instant::now();
    let placed = match destination(target_pid, capture_end) {
        Destination::Lost => {
            // Processing succeeded, so what is preserved is the finished text
            // rather than the raw transcription.
            let preserved = preserve(&inference.output, || Preserved::Processed);
            return failed(
                timings,
                speech_finished,
                Partial {
                    transcript: inference.transcript,
                    route: Some(inference.route),
                    output: inference.output.clone(),
                    trace_id,
                },
                capture_end.headline(),
                failure_detail(capture_end.explanation(), &preserved),
                Some(preserved),
            );
        }
        Destination::Insert(pid) => {
            // Anything the user pressed or clicked since the last insertion
            // could have taken the cursor somewhere else, so the memory of
            // what sits behind it goes before it is consulted.
            if cursor_moved.take() {
                cursor.forget();
            }
            let (previous, before_previous) = cursor.recall(pid);
            let joined = crate::insertion::join(previous, before_previous, &inference.output);
            let placed = insert_text(&joined, pid);
            // Only text that reached the document describes where the cursor
            // now is. Anything else left it wherever it already was, which is
            // somewhere this no longer knows.
            match placed {
                Ok(Insertion::Pasted) => cursor.remember(pid, &joined),
                _ => cursor.forget(),
            }
            placed
        }
        // Nothing to paste into, so the pasteboard is the whole of the
        // insertion. It ends as the same outcome as a destination that had no
        // text field focused, because from the user's side it is the same
        // thing: the words are on the clipboard and nothing was typed
        // anywhere.
        Destination::Clipboard => match preserve(&inference.output, || Preserved::Processed) {
            Preserved::Processed => Ok(Insertion::CopiedNoField),
            unavailable => Err(anyhow::anyhow!(
                "{}",
                failure_detail(capture_end.explanation(), &unavailable)
            )),
        },
    };
    let insertion = match placed {
        Ok(insertion) => insertion,
        Err(error) => {
            // `insert_text` writes the pasteboard before anything that can
            // refuse, so these words have usually survived. Asked again here
            // rather than assumed: the one error it can return before that
            // write is the pasteboard itself being unavailable, and a toast
            // that told the user to press Cmd-V for words that are not there
            // would be worse than saying nothing.
            let preserved = preserve(&inference.output, || Preserved::Processed);
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
                error.to_string(),
                Some(preserved),
            );
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
        preserved: None,
        trace_id,
    }))
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
    preserved: Option<Preserved>,
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
        preserved,
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

    /// Pressing the key with nothing able to receive the text is not a
    /// failure and never was one worth refusing: the words exist, and the
    /// clipboard can hold them. What must not be swallowed by the same answer
    /// is a capture that ended before the user had finished speaking, because
    /// there the dictation really was cut short.
    #[test]
    fn a_dictation_with_nowhere_to_land_is_not_a_lost_one() {
        assert_eq!(destination(Some(4321), CaptureEnd::Released), Destination::Insert(4321));
        assert_eq!(destination(None, CaptureEnd::Released), Destination::Clipboard);
        assert_eq!(destination(Some(4321), CaptureEnd::CeilingReached), Destination::Lost);
        assert_eq!(destination(None, CaptureEnd::BufferFull), Destination::Lost);
    }

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

    /// The ordinary case, and the only one allowed to type into the document.
    #[test]
    fn a_deliberate_release_may_insert() {
        assert!(CaptureEnd::Released.may_insert());
    }

    /// An ending the user did not ask for must not place text for them.
    /// A stuck key recording five minutes of a meeting is exactly the case
    /// the ceiling exists for, and typing that into their document would be
    /// worse than the recording itself.
    #[test]
    fn an_ending_the_user_did_not_ask_for_never_inserts() {
        assert!(!CaptureEnd::CeilingReached.may_insert());
        assert!(!CaptureEnd::BufferFull.may_insert());
    }

    /// Reaching the ceiling is reported as what it is, and says the words
    /// were kept rather than leaving the user to guess.
    #[test]
    fn the_ceiling_explains_itself_and_says_the_words_were_transcribed() {
        assert_eq!(CaptureEnd::CeilingReached.headline(), "Recording hit 5 min");
        let detail = failure_detail(
            CaptureEnd::CeilingReached.explanation(),
            &Preserved::Processed,
        );
        assert!(detail.contains("five minute recording limit"));
        assert!(detail.contains("transcribed"));
        assert!(detail.contains("clipboard"));
    }

    /// A full buffer loses the end of the dictation, not the beginning, and
    /// the message has to be honest about which.
    #[test]
    fn a_full_buffer_says_the_end_was_lost_and_keeps_the_rest() {
        let detail = failure_detail(CaptureEnd::BufferFull.explanation(), &Preserved::Processed);
        assert!(detail.contains("end of what you said was not"));
        assert!(detail.contains("clipboard"));
    }

    /// Hitting the ceiling and then failing downstream preserves the raw
    /// transcript: processing never produced anything better to keep.
    #[test]
    fn a_downstream_failure_after_the_ceiling_still_preserves_the_raw_words() {
        let detail = failure_detail("S1-mini failed to process this LIGHT_CLEANUP utterance.",
                                    &Preserved::Raw);
        assert!(detail.contains("raw transcription"));
        assert!(detail.contains("clipboard"));
    }

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
