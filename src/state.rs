use crate::platform::Insertion;
use std::time::{Duration, Instant};

/// What the capsule is showing, which is also the app's single answer to
/// "where is this dictation up to?". Listening and Processing are the two
/// states a dictation is in flight, so nothing else needs to track that
/// separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudState {
    Idle,
    Listening,
    Processing,
    Done,
    /// Transcribed and put on the pasteboard, but not pasted, because the
    /// destination the user started dictating into was no longer frontmost.
    /// This is a success with a caveat rather than a failure: the words are
    /// in the user's hands, they just need a paste.
    Copied,
    /// The key was held and nothing was said. Neither a dictation nor a
    /// failure: there was nothing to transcribe, so the capsule says so and
    /// settles back without filing anything.
    NoSpeech,
    /// Speech was heard and could not be decoded. Not silence, because the
    /// user spoke, and not a failure, because nothing broke: the words were
    /// simply not understood, and saying nothing about it left the capsule
    /// looking as though the dictation had evaporated.
    NotUnderstood,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Route {
    #[serde(rename = "PASS_THROUGH")]
    PassThrough,
    #[serde(rename = "LIGHT_CLEANUP")]
    LightCleanup,
    #[serde(rename = "TRANSFORM")]
    Transform,
    #[serde(rename = "COMPLEX")]
    Complex,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::PassThrough => "PASS_THROUGH",
            Route::LightCleanup => "LIGHT_CLEANUP",
            Route::Transform => "TRANSFORM",
            Route::Complex => "COMPLEX",
        }
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Timings {
    pub audio_ms: Option<u128>,
    pub capture_finalize_ms: Option<u128>,
    pub queue_ms: Option<u128>,
    pub asr_ms: Option<u128>,
    pub router_ms: Option<u128>,
    pub transform_ms: Option<u128>,
    pub insert_ms: Option<u128>,
    pub total_ms: Option<u128>,
}

/// What a failure means for the person dictating, which is a different
/// question from what went wrong technically. The capsule paints this, so the
/// mark answers "was anything lost?" before the words are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// A precondition was not met and PrivacyFlow never started listening.
    /// Nothing was captured, so nothing was lost.
    Blocked,
    /// The microphone cannot be used at all. Nothing was lost, but pressing
    /// again will not help; the remedy is outside PrivacyFlow.
    InputUnavailable,
    /// Capture began and the pipeline failed before text reached the cursor.
    /// The only kind where the user spoke and the words did not come back.
    Dropped,
}

/// What became of the user's words when the pipeline failed after producing
/// them. Preserving is not inserting: text that failed its processing is never
/// typed into the document as though it had succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preserved {
    /// The raw transcription, because processing never completed.
    Raw,
    /// The finished text, which processing produced but insertion could not place.
    Processed,
    /// The words could not even be put on the pasteboard.
    Unavailable(String),
}

/// A failure as the user experiences it: a kind, a headline short enough for
/// the capsule, and the original message kept intact for the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub kind: FailureKind,
    pub headline: &'static str,
    pub detail: String,
}

impl Failure {
    pub fn blocked(headline: &'static str, detail: impl Into<String>) -> Self {
        Self { kind: FailureKind::Blocked, headline, detail: detail.into() }
    }

    pub fn input_unavailable(headline: &'static str, detail: impl Into<String>) -> Self {
        Self { kind: FailureKind::InputUnavailable, headline, detail: detail.into() }
    }

    pub fn dropped(headline: &'static str, detail: impl Into<String>) -> Self {
        Self { kind: FailureKind::Dropped, headline, detail: detail.into() }
    }
}

/// Whether a toast is reporting a dictation that survived or one that was
/// lost. The capsule's palette already answers that question in one colour,
/// and the toast borrows the same answer rather than inventing a second one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Copied,
    Failed,
    /// Raised while the key is still down, not after. It is the one toast
    /// that reports something the user can still do something about.
    Quiet,
}

/// What PrivacyFlow says when a dictation ends up on the clipboard instead of
/// at the cursor.
///
/// The capsule already shows these outcomes, but only as one word, for 1.4
/// seconds, in a widget the size of a thumbnail - and every one of them
/// happens precisely because the text did not appear where the user was
/// looking. The toast is the part of the answer that says what was kept and
/// how to place it.
#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub headline: String,
    /// The text that is actually on the clipboard, so that what the user
    /// reads is what they will paste.
    pub body: String,
    pub footer: &'static str,
    pub raised_at: Instant,
}

impl Toast {
    fn copied(headline: &str, body: &str) -> Self {
        Self {
            kind: ToastKind::Copied,
            headline: headline.to_owned(),
            body: body.to_owned(),
            footer: "Copied to clipboard · ⌘V",
            raised_at: Instant::now(),
        }
    }

    /// Said while the recording is still running, because it is the only
    /// moment it can be acted on: leaning in or speaking up now lifts the
    /// mean back over the floor and the words survive. Afterwards there is
    /// nothing to do but dictate it again.
    fn quiet() -> Self {
        Self {
            kind: ToastKind::Quiet,
            headline: "PrivacyFlow can barely hear you".to_owned(),
            body: "This dictation is too quiet to be transcribed. Move closer \
                   to the microphone, or speak up."
                .to_owned(),
            footer: "Still recording · it can still be saved",
            raised_at: Instant::now(),
        }
    }

    /// A failure's toast points at the console, because the reason a
    /// dictation was lost is often longer than three lines - the
    /// Accessibility remedy is a paragraph - and the console is where the
    /// whole of it is kept.
    fn failed(headline: &str, body: &str, footer: &'static str) -> Self {
        Self {
            kind: ToastKind::Failed,
            headline: headline.to_owned(),
            body: body.to_owned(),
            footer,
            raised_at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DebugRecord {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub transcript: String,
    pub route: Option<Route>,
    pub output: String,
    pub timings: Timings,
    pub failure: Option<Failure>,
    /// A remark about an outcome that is neither an insertion nor a failure,
    /// in plain language. The one case today is a dictation that could not be
    /// decoded, where the numbers behind the decision are the only thing
    /// worth keeping.
    pub note: Option<String>,
    /// How the text got where it was going, for the dictations that succeeded.
    /// Absent on a failure, where nothing was inserted at all.
    pub insertion: Option<Insertion>,
}

/// What the resident Python worker is actually doing. Loading mlx-whisper and
/// the Kev checkpoint takes a bounded but long time, so "not ready yet" is an
/// ordinary state rather than a problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    Starting,
    Ready,
    Failed(String),
}

/// Which tab of the console window is showing. Activity is the default
/// because the console is opened most often to see what just happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleTab {
    #[default]
    Activity,
    Settings,
    Status,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub hud: HudState,
    /// How far each of the mark's bars stands while listening, from the voice
    /// meter. Only meaningful while the capsule is listening.
    pub voice_bars: [f32; 4],
    pub transcript: String,
    pub route: Option<Route>,
    pub output: String,
    pub timings: Timings,
    pub last_failure: Option<Failure>,
    pub history: Vec<DebugRecord>,
    pub console_open: bool,
    pub console_tab: ConsoleTab,
    pub unread_failure: bool,
    pub worker: WorkerStatus,
    /// Whether the Right Option watcher actually installed at startup. The
    /// Status tab reports this rather than assuming the binding works.
    pub hotkey_installed: bool,
    /// Whether the capsule's window actually refuses keyboard focus, checked
    /// once at startup by asking the window after it was changed. The class of
    /// a window does not change afterwards, so this does not need re-asking
    /// the way a permission does.
    pub capsule_non_activating: bool,
    /// What the user has chosen. Loaded once at startup.
    pub settings: crate::settings::Settings,
    /// Why the settings file could not be read, if it existed and could not.
    /// Shown as a banner in the Settings tab, which is where someone who
    /// wants to fix it will look.
    pub settings_problem: Option<String>,
    /// Why the last attempt to save failed. Shown beside the control, because
    /// a ticked checkbox that did not save is the interface lying.
    pub settings_write_error: Option<String>,
    /// Why the cue sounds cannot play, if they cannot. Shown beside their
    /// checkbox rather than raised as a startup failure: no audio output
    /// costs the user a confirmation sound, not a dictation.
    pub cue_problem: Option<String>,
    /// Why the microphone could not be opened, the last time it was tried.
    /// Shown under the microphone choice, which is where someone who just
    /// picked one will look.
    pub microphone_problem: Option<String>,
    /// Set when a different microphone is chosen, so the app reopens it
    /// straight away rather than on the next press.
    pub microphone_choice_changed: bool,
    /// The connected inputs, listed once when the microphone list is opened
    /// and dropped when it closes. Listing costs about 80 ms, far too much to
    /// repeat every frame the list is showing.
    pub microphone_choices: Option<Result<Vec<String>, String>>,
    /// When the capture was handed to the pipeline, while the capsule is
    /// still showing the state before it. The mirror of `done_at`: that one
    /// retires a state after a delay, this one promotes one.
    pub processing_since: Option<Instant>,
    /// When the capsule last settled on a finished or failed dictation. The
    /// capsule returns to Ready 1.4s later, so the timer belongs with the
    /// result it describes.
    pub done_at: Option<Instant>,
    /// What to say about words that went to the clipboard rather than to the
    /// cursor, if anything. At most one: a toast describes the last
    /// dictation, and there is only ever one of those.
    pub toast: Option<Toast>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            hud: HudState::Idle,
            voice_bars: [0.0; 4],
            transcript: String::new(),
            route: None,
            output: String::new(),
            timings: Timings::default(),
            last_failure: None,
            history: Vec::new(),
            console_open: false,
            console_tab: ConsoleTab::default(),
            unread_failure: false,
            worker: WorkerStatus::Starting,
            hotkey_installed: false,
            capsule_non_activating: false,
            settings: Default::default(),
            settings_problem: None,
            settings_write_error: None,
            cue_problem: None,
            microphone_problem: None,
            microphone_choice_changed: false,
            microphone_choices: None,
            processing_since: None,
            done_at: None,
            toast: None,
        }
    }
}

impl AppState {
    /// Starts a new dictation. Everything describing the previous one goes,
    /// including the dwell timer, so a press that arrives while the last
    /// result is still on screen cannot be retired by that result's clock.
    pub fn reset_for_recording(&mut self) {
        self.hud = HudState::Listening;
        self.voice_bars = [0.0; 4];
        self.clear_result();
        self.last_failure = None;
        self.processing_since = None;
        self.done_at = None;
        self.toast = None;
    }

    /// Forgets the last dictation's text and timings without touching the
    /// capsule's state.
    pub fn clear_result(&mut self) {
        self.transcript.clear();
        self.route = None;
        self.output.clear();
        self.timings = Timings::default();
    }

    /// The capture is on its way to the worker.
    ///
    /// The capsule deliberately does not change yet. Most dictations take
    /// about a second and deserve to say what they are doing, but a capture
    /// with no speech in it is refused before transcription starts and its
    /// answer arrives within a frame or two. Announcing the work first made
    /// the capsule flash a state it was never meaningfully in.
    pub fn begin_processing(&mut self) {
        self.processing_since = Some(Instant::now());
    }

    /// What the capsule calls the work it is waiting on.
    ///
    /// A capture pressed before the models have loaded is queued behind them,
    /// because the pipeline thread cannot take work until the worker exists.
    /// Calling that "Transcribing" claimed the one thing that was certainly
    /// not happening, and turned a ten second startup into what looked like a
    /// hang. Derived from the worker rather than latched, so it becomes true
    /// again the moment the worker reports in.
    pub fn processing_label(&self) -> &'static str {
        match self.worker {
            WorkerStatus::Starting => "Loading models",
            _ => "Transcribing",
        }
    }

    /// Say "Transcribing" once the wait has lasted long enough to be worth
    /// mentioning. A wait that has already been answered is not promoted:
    /// there is something real on the capsule by then.
    pub fn announce_processing(&mut self, delay: Duration) {
        let Some(since) = self.processing_since else {
            return;
        };
        if since.elapsed() >= delay {
            self.hud = HudState::Processing;
            self.processing_since = None;
        }
    }

    /// Files a dictation that reached the cursor, in whichever of the two
    /// ways it landed.
    pub fn record_inserted(&mut self, insertion: Insertion) {
        self.hud = match insertion {
            Insertion::Pasted => HudState::Done,
            Insertion::CopiedOnly | Insertion::CopiedNoField => HudState::Copied,
        };
        // Only the two clipboard endings say anything. A dictation that
        // landed at the cursor needs no announcement: the user is watching
        // their own words appear.
        self.toast = match insertion {
            Insertion::Pasted => None,
            Insertion::CopiedOnly => {
                Some(Toast::copied("Destination changed", &self.output))
            }
            Insertion::CopiedNoField => {
                Some(Toast::copied("No text field focused", &self.output))
            }
        };
        self.push_history(None, Some(insertion));
        self.settle();
    }

    /// Files a failure: the capsule shows it, the console keeps it, and the
    /// dot points at the console unless the console is already the thing the
    /// user is looking at.
    pub fn record_failure(&mut self, failure: Failure, preserved: Option<Preserved>) {
        self.hud = HudState::Error;
        // Raised before the fields are cleared, and from whichever of them
        // the clipboard actually holds: a toast that showed the processed
        // text while the clipboard held the raw transcription would describe
        // words the user is not about to paste.
        self.toast = match &preserved {
            Some(Preserved::Processed) => Some(Toast::failed(
                failure.headline,
                &self.output,
                "Copied to clipboard · ⌘V · details in the console",
            )),
            Some(Preserved::Raw) => Some(Toast::failed(
                failure.headline,
                &self.transcript,
                "Raw transcript copied · ⌘V · details in the console",
            )),
            // Nothing was kept, so there is nothing to tell the user to
            // paste. Saying so here would send them to press Cmd-V for
            // whatever happened to be on the clipboard already.
            Some(Preserved::Unavailable(_)) | None => None,
        };
        if !self.console_open {
            self.unread_failure = true;
        }
        // Blocked and InputUnavailable both mean capture never began, so the
        // fields still holding the previous dictation must not be filed
        // against this failure as though its words were lost...
        if failure.kind != FailureKind::Dropped {
            self.clear_result();
        }
        self.last_failure = Some(failure.clone());
        self.push_history(Some(failure), None);
        self.settle();
    }

    /// Files a dictation that was heard and not understood.
    ///
    /// Told to the user rather than swallowed, unlike silence: they spoke,
    /// and what they said was discarded. It is still not a failure, so it
    /// neither raises the unread dot nor paints the capsule red.
    pub fn record_not_understood(&mut self, note: String) {
        self.hud = HudState::NotUnderstood;
        // Whatever Whisper decoded was not speech, so none of it is filed
        // against this dictation as though it had been.
        self.clear_result();
        self.push_history_with(None, None, Some(note));
        self.settle();
    }

    pub fn push_history(&mut self, failure: Option<Failure>, insertion: Option<Insertion>) {
        self.push_history_with(failure, insertion, None);
    }

    fn push_history_with(
        &mut self,
        failure: Option<Failure>,
        insertion: Option<Insertion>,
        note: Option<String>,
    ) {
        self.history.insert(
            0,
            DebugRecord {
                timestamp: chrono::Utc::now(),
                transcript: self.transcript.clone(),
                route: self.route,
                output: self.output.clone(),
                timings: self.timings.clone(),
                failure,
                note,
                insertion,
            },
        );
        self.history.truncate(50);
    }

    /// Files a press that held no speech.
    ///
    /// The omissions are the point. Nothing goes into the history, because a
    /// dictation that captured no words has nothing to show in it, and the
    /// unread dot is left exactly as it was, because the dot means "a failure
    /// is waiting in the console" and this is not one. The capsule says so
    /// for a moment and the dwell timer takes it back to Ready.
    /// Warn that the press being held is heading for refusal.
    pub fn warn_quiet(&mut self) {
        self.toast = Some(Toast::quiet());
    }

    pub fn record_no_speech(&mut self) {
        self.hud = HudState::NoSpeech;
        self.settle();
    }

    /// The end of a dictation, however it ended: the dwell timer starts, and
    /// any pending announcement is abandoned, because there is now something
    /// real on the capsule that must not be painted over.
    fn settle(&mut self) {
        self.processing_since = None;
        self.done_at = Some(Instant::now());
    }

    /// Take the toast away once it has been up long enough to read.
    ///
    /// Its own clock rather than the capsule's: the capsule retires a single
    /// word, and this retires a sentence of the user's own speech.
    pub fn retire_toast(&mut self, after: Duration) {
        if self.toast.as_ref().is_some_and(|toast| toast.raised_at.elapsed() >= after) {
            self.toast = None;
        }
    }

    /// Opening the console is the acknowledgement, so it is the one place the
    /// unread dot is cleared.
    pub fn open_console(&mut self) {
        self.console_open = true;
        self.unread_failure = false;
    }
}

#[cfg(test)]
mod tests {

    /// The warning is raised while the key is down, and the refusal it
    /// predicted arrives the moment the key comes up. If ending the dictation
    /// cleared it, the user would see the warning flash and vanish before
    /// they could read why nothing was transcribed.
    #[test]
    fn a_quiet_warning_outlives_the_refusal_it_predicted() {
        let mut state = AppState::default();
        state.reset_for_recording();
        state.warn_quiet();
        state.record_no_speech();
        let toast = state.toast.expect("the warning must still be readable");
        assert_eq!(toast.kind, ToastKind::Quiet);
    }
    use super::*;
    use std::time::Duration;

    #[test]
    fn recording_reset_clears_the_previous_result() {
        let mut state = AppState {
            transcript: "old".into(),
            output: "old output".into(),
            route: Some(Route::Complex),
            last_failure: Some(Failure::dropped("Old failure", "old error")),
            ..Default::default()
        };
        state.reset_for_recording();
        assert_eq!(state.hud, HudState::Listening);
        assert!(
            state.transcript.is_empty()
                && state.output.is_empty()
                && state.route.is_none()
                && state.last_failure.is_none()
        );
    }

    /// The capsule answers "was anything lost?" before any words are read, so a
    /// failure that reaches the user without a kind is a failure with no meaning.
    #[test]
    fn a_failure_carries_its_kind_and_keeps_the_original_message() {
        let failure = Failure::dropped(
            "Transcription failed",
            "mlx-whisper worker exited before returning a transcript (exit code 1)",
        );
        assert_eq!(failure.kind, FailureKind::Dropped);
        assert_eq!(failure.headline, "Transcription failed");
        assert_eq!(
            failure.detail,
            "mlx-whisper worker exited before returning a transcript (exit code 1)",
            "the capsule shortens the display, never the diagnostic"
        );
    }

    /// The dot exists so a failure that happened while the user was typing
    /// elsewhere is still discoverable. Recording again must not erase it.
    #[test]
    fn starting_a_new_recording_keeps_an_unread_failure_visible() {
        let mut state = AppState {
            last_failure: Some(Failure::blocked("No text field focused", "detail")),
            unread_failure: true,
            ..Default::default()
        };
        state.reset_for_recording();
        assert!(state.last_failure.is_none(), "the capsule shows the new recording");
        assert!(state.unread_failure, "but the unread dot survives until the console is opened");
    }

    /// Opening the console is the one thing that clears the dot.
    #[test]
    fn opening_the_console_clears_the_unread_dot() {
        let mut state = AppState { unread_failure: true, ..Default::default() };
        state.open_console();
        assert!(state.console_open);
        assert!(!state.unread_failure);
    }

    /// History is the permanent record, so it keeps the full text and the kind.
    #[test]
    fn history_records_the_full_detail_not_the_headline() {
        let mut state = AppState::default();
        state.push_history(Some(Failure::dropped("Couldn't insert", "Target application changed")), None);
        let recorded = state.history[0].failure.as_ref().unwrap();
        assert_eq!(recorded.detail, "Target application changed");
        assert_eq!(recorded.kind, FailureKind::Dropped);
    }

    /// A dictation that starts while the previous result is still on screen
    /// must take the capsule with it. If the old dwell timer survives, it
    /// expires mid-transcription, forces the capsule back to Ready and strands
    /// the recording that is actually running.
    #[test]
    fn starting_a_recording_retires_the_previous_result_s_dwell_timer() {
        let mut state = AppState {
            hud: HudState::Done,
            done_at: Some(Instant::now() - Duration::from_millis(1300)),
            ..Default::default()
        };
        state.reset_for_recording();
        assert!(state.done_at.is_none(), "the finished dictation's clock must not outlive it");
    }

    /// Blocked means nothing was captured. Filing the previous utterance's
    /// transcript against it would tell the reader that text was transcribed
    /// and then lost, which is the opposite of what the kind means.
    #[test]
    fn a_blocked_failure_does_not_file_the_previous_utterance() {
        let mut state = AppState {
            transcript: "the thing I said last time".into(),
            output: "The thing I said last time.".into(),
            route: Some(Route::LightCleanup),
            timings: Timings { audio_ms: Some(1840), ..Default::default() },
            ..Default::default()
        };
        state.record_failure(Failure::blocked("No text field focused", "detail"), None);
        let record = &state.history[0];
        assert!(record.transcript.is_empty() && record.output.is_empty());
        assert!(record.route.is_none());
        assert!(record.timings.audio_ms.is_none(), "no audio was captured to time");
        assert_eq!(record.failure.as_ref().unwrap().detail, "detail");
    }

    /// The capsule shows "Copied" for 1400 ms, and that state exists only
    /// because the user switched away, so the moment it is shown is the moment
    /// they are guaranteed not to be looking. If the record does not carry it,
    /// a dictation that was never pasted is indistinguishable from one that
    /// was, and the app has claimed a success it did not achieve.
    #[test]
    fn history_distinguishes_a_copied_dictation_from_a_pasted_one() {
        let mut state = AppState { transcript: "some words".into(), ..Default::default() };
        state.push_history(None, Some(Insertion::CopiedOnly));
        state.push_history(None, Some(Insertion::Pasted));
        assert_eq!(state.history[0].insertion, Some(Insertion::Pasted));
        assert_eq!(state.history[1].insertion, Some(Insertion::CopiedOnly));
    }

    /// A dropped dictation is the one kind where the user spoke, so whatever
    /// did come back belongs in the record next to the error.
    #[test]
    fn a_dropped_failure_keeps_the_transcript_it_did_get() {
        let mut state = AppState { transcript: "half a sentence".into(), ..Default::default() };
        state.record_failure(Failure::dropped("Couldn't insert", "Target application changed"), None);
        assert_eq!(state.history[0].transcript, "half a sentence");
    }

    /// The dot exists to send the user to the console. Setting it while the
    /// console is open leaves a dot nothing can clear.
    #[test]
    fn a_failure_raised_while_the_console_is_open_does_not_set_the_dot() {
        let mut state = AppState { console_open: true, ..Default::default() };
        state.record_failure(Failure::dropped("Transcription failed", "worker died"), None);
        assert!(!state.unread_failure);
    }

    /// A capture pressed in the first seconds after launch is queued behind
    /// the model load, which takes about ten seconds. The capsule announced
    /// that as "Transcribing", so the one slow path left in the app claimed to
    /// be doing the one thing it could not yet do, and read as a hang.
    #[test]
    fn work_queued_before_the_models_load_does_not_claim_to_be_transcribing() {
        let mut state = AppState::default();
        assert_eq!(state.worker, WorkerStatus::Starting);
        assert_eq!(state.processing_label(), "Loading models");

        // Derived rather than latched, so it corrects itself the moment the
        // worker reports in, without anything having to remember to fix it.
        state.worker = WorkerStatus::Ready;
        assert_eq!(state.processing_label(), "Transcribing");
    }

    /// The capsule used to announce Transcribing the instant the key came up.
    /// A capture with no speech in it is refused before transcription starts,
    /// so its answer lands within a frame or two, and the capsule flashed
    /// green, blue and grey in the time it takes to blink.
    #[test]
    fn an_answer_that_beats_the_delay_never_announces_transcribing() {
        let mut state = AppState::default();
        state.reset_for_recording();
        state.begin_processing();

        state.announce_processing(Duration::from_secs(1));
        assert_eq!(state.hud, HudState::Listening, "too soon to say anything");

        state.record_no_speech();
        // The wait is over, so a promotion that arrives late must not paint
        // Transcribing over the result the user is already reading.
        state.announce_processing(Duration::ZERO);
        assert_eq!(state.hud, HudState::NoSpeech);
    }

    /// A real dictation takes about a second, which is well worth announcing.
    #[test]
    fn a_wait_long_enough_to_notice_does_say_transcribing() {
        let mut state = AppState::default();
        state.reset_for_recording();
        state.begin_processing();
        state.announce_processing(Duration::ZERO);
        assert_eq!(state.hud, HudState::Processing);
    }

    /// Pressing the key and saying nothing is a non-event, not a dictation.
    /// There is no transcript to keep and nothing for the user to go and read,
    /// so filing it in Activity would pad the history with blank cards and
    /// raising the dot would send them to the console to find them.
    #[test]
    fn a_silent_press_leaves_no_trace_in_the_console() {
        let mut state = AppState::default();
        state.record_no_speech();
        assert_eq!(state.hud, HudState::NoSpeech);
        assert!(state.done_at.is_some(), "the capsule has to settle back to Ready");
        assert!(state.history.is_empty(), "there was no dictation to file");
        assert!(!state.unread_failure, "nothing failed, so nothing is unread");
    }

    /// Being misheard is not silence and it is not a failure. The user spoke,
    /// the words were discarded, and the app used to answer that with the
    /// same quiet nothing it gives a press that held no speech: the capsule
    /// sat on Transcribing and then went back to Ready with no text, no
    /// clipboard and no explanation.
    #[test]
    fn a_dictation_that_could_not_be_decoded_says_so_and_files_it() {
        let mut state = AppState { transcript: "garbage".into(), ..Default::default() };
        state.record_not_understood("2.8 s of audio, confidence -7.89".into());
        assert_eq!(state.hud, HudState::NotUnderstood);
        assert!(state.last_failure.is_none(), "not understanding is not a failure");
        assert!(!state.unread_failure, "and it does not raise the unread dot");
        assert!(state.toast.is_none(), "there is no text to tell the user to paste");
        assert_eq!(
            state.history[0].note.as_deref(),
            Some("2.8 s of audio, confidence -7.89"),
            "the console keeps the numbers, which are the only way to tune the floor"
        );
        assert!(state.history[0].transcript.is_empty(), "whatever it decoded was not speech");
        assert!(state.done_at.is_some(), "the capsule has to settle back to Ready");
    }

    /// The dot outlives the capsule, so a failure the user has not opened the
    /// console for must survive them pressing the key and saying nothing.
    #[test]
    fn a_silent_press_does_not_clear_an_earlier_unread_failure() {
        let mut state = AppState {
            unread_failure: true,
            ..Default::default()
        };
        state.record_no_speech();
        assert!(state.unread_failure);
    }

    /// The Status tab reports what is true, so the worker starts out unknown
    /// rather than optimistically ready.
    #[test]
    fn the_worker_starts_unknown_and_only_becomes_ready_when_it_says_so() {
        let state = AppState::default();
        assert_eq!(state.worker, WorkerStatus::Starting);
    }

    /// Activity is what happened and Settings is what PrivacyFlow does, so the
    /// console must still open on Activity: adding a tab must not change
    /// which one a user lands on when they follow the unread dot.
    #[test]
    fn the_console_still_opens_on_activity() {
        assert_eq!(AppState::default().console_tab, ConsoleTab::Activity);
    }

    /// The capsule says "Copied" for 1.4s, and this state only happens
    /// because the paste could not go where the user was pointing, which is
    /// precisely when they are least likely to be watching a small widget in
    /// the corner. The toast is what actually reaches them.
    #[test]
    fn a_dictation_copied_because_nothing_was_focused_announces_itself() {
        let mut state = AppState { output: "The finished text.".into(), ..Default::default() };
        state.record_inserted(Insertion::CopiedNoField);
        let toast = state.toast.as_ref().expect("the user has to be told where their words went");
        assert_eq!(toast.headline, "No text field focused");
        assert_eq!(toast.body, "The finished text.");
        assert_eq!(toast.kind, ToastKind::Copied);
    }

    /// Red means the user lost something. A dictation that reached the
    /// clipboard lost nothing: it is a success that needs a Cmd-V, and
    /// painting it as a failure taught the user to read the error state as
    /// noise. Both clipboard endings are checked, because they arrive from
    /// different places and only share this if something keeps them together.
    #[test]
    fn a_dictation_that_reached_the_clipboard_is_never_painted_as_an_error() {
        for insertion in [Insertion::CopiedOnly, Insertion::CopiedNoField] {
            let mut state = AppState { output: "words".into(), ..Default::default() };
            state.record_inserted(insertion);
            assert_eq!(state.hud, HudState::Copied, "{insertion:?} is not an error state");
            assert!(state.last_failure.is_none(), "{insertion:?} left a failure behind");
            assert!(!state.unread_failure, "{insertion:?} raised the unread dot");
            assert_eq!(state.toast.as_ref().unwrap().kind, ToastKind::Copied);
        }
    }

    /// A dictation that landed at the cursor needs no announcement: the user
    /// is looking at their own words appearing.
    #[test]
    fn an_ordinary_paste_says_nothing() {
        let mut state = AppState { output: "The finished text.".into(), ..Default::default() };
        state.record_inserted(Insertion::Pasted);
        assert!(state.toast.is_none());
    }

    /// Raw text is never described as though it had been processed. The
    /// clipboard holds the transcription, so the toast has to show that and
    /// not the output field, which still holds whatever processing managed
    /// before it failed.
    #[test]
    fn a_failure_that_kept_the_raw_transcript_shows_the_transcript() {
        let mut state = AppState {
            transcript: "what I actually said".into(),
            output: "half rewritten".into(),
            ..Default::default()
        };
        state.record_failure(
            Failure::dropped("Rewriting failed", "the worker stopped"),
            Some(Preserved::Raw),
        );
        let toast = state.toast.as_ref().expect("the words survived, so say where");
        assert_eq!(toast.headline, "Rewriting failed");
        assert_eq!(toast.body, "what I actually said");
        assert_eq!(toast.kind, ToastKind::Failed);
    }

    /// A toast that says the words are on the clipboard when they are not is
    /// worse than silence: it sends the user to press Cmd-V for nothing.
    #[test]
    fn a_failure_that_could_not_keep_anything_raises_no_toast() {
        let mut state = AppState { transcript: "gone".into(), ..Default::default() };
        state.record_failure(
            Failure::dropped("Transcription failed", "the worker stopped"),
            Some(Preserved::Unavailable("Could not access macOS pasteboard".into())),
        );
        assert!(state.toast.is_none());
    }

    /// Pressing the key again is the user moving on. A toast about the last
    /// dictation hanging over the next one would describe the wrong words.
    #[test]
    fn a_new_recording_takes_the_toast_with_it() {
        let mut state = AppState { output: "old words".into(), ..Default::default() };
        state.record_inserted(Insertion::CopiedOnly);
        assert!(state.toast.is_some());
        state.reset_for_recording();
        assert!(state.toast.is_none());
    }

    /// The toast retires on its own clock rather than on the capsule's: it
    /// carries more to read than a one word label does.
    #[test]
    fn a_toast_retires_once_its_time_is_up() {
        let mut state = AppState { output: "words".into(), ..Default::default() };
        state.record_inserted(Insertion::CopiedOnly);
        state.retire_toast(Duration::from_secs(60));
        assert!(state.toast.is_some(), "still well within its time");
        state.retire_toast(Duration::ZERO);
        assert!(state.toast.is_none());
    }

    /// A settings file that could not be read has to reach the user. It is
    /// recorded as a failure so the unread dot points at the console, the
    /// same way every other startup problem is surfaced.
    #[test]
    fn an_unreadable_settings_file_is_reported_like_any_other_startup_problem() {
        let mut state = AppState::default();
        state.record_failure(
            Failure::blocked(
                "Settings unreadable",
                "Could not read /tmp/settings.json: expected value at line 1 column 3",
            ),
            None,
        );
        assert!(state.unread_failure);
        assert!(state.history[0]
            .failure
            .as_ref()
            .unwrap()
            .detail
            .contains("settings.json"));
    }
}
