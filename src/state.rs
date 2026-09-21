use crate::platform::Insertion;
use std::time::Instant;

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
    /// A precondition was not met and LocalFlow never started listening.
    /// Nothing was captured, so nothing was lost.
    Blocked,
    /// The microphone cannot be used at all. Nothing was lost, but pressing
    /// again will not help; the remedy is outside LocalFlow.
    InputUnavailable,
    /// Capture began and the pipeline failed before text reached the cursor.
    /// The only kind where the user spoke and the words did not come back.
    Dropped,
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

#[derive(Debug, Clone)]
pub struct DebugRecord {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub transcript: String,
    pub route: Option<Route>,
    pub output: String,
    pub timings: Timings,
    pub failure: Option<Failure>,
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
    Status,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub hud: HudState,
    pub mic_level: f32,
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
    /// Whether a microphone device opened at startup.
    pub microphone_available: bool,
    /// Whether the capsule's window actually refuses keyboard focus, checked
    /// once at startup by asking the window after it was changed. The class of
    /// a window does not change afterwards, so this does not need re-asking
    /// the way a permission does.
    pub capsule_non_activating: bool,
    /// When the capsule last settled on a finished or failed dictation. The
    /// capsule returns to Ready 1.4s later, so the timer belongs with the
    /// result it describes.
    pub done_at: Option<Instant>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            hud: HudState::Idle,
            mic_level: 0.0,
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
            microphone_available: false,
            capsule_non_activating: false,
            done_at: None,
        }
    }
}

impl AppState {
    /// Starts a new dictation. Everything describing the previous one goes,
    /// including the dwell timer, so a press that arrives while the last
    /// result is still on screen cannot be retired by that result's clock.
    pub fn reset_for_recording(&mut self) {
        self.hud = HudState::Listening;
        self.mic_level = 0.0;
        self.clear_result();
        self.last_failure = None;
        self.done_at = None;
    }

    /// Forgets the last dictation's text and timings without touching the
    /// capsule's state.
    pub fn clear_result(&mut self) {
        self.transcript.clear();
        self.route = None;
        self.output.clear();
        self.timings = Timings::default();
    }

    /// Files a failure: the capsule shows it, the console keeps it, and the
    /// dot points at the console unless the console is already the thing the
    /// user is looking at.
    pub fn record_failure(&mut self, failure: Failure) {
        self.hud = HudState::Error;
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
        self.done_at = Some(Instant::now());
    }

    pub fn push_history(&mut self, failure: Option<Failure>, insertion: Option<Insertion>) {
        self.history.insert(
            0,
            DebugRecord {
                timestamp: chrono::Utc::now(),
                transcript: self.transcript.clone(),
                route: self.route,
                output: self.output.clone(),
                timings: self.timings.clone(),
                failure,
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
    pub fn record_no_speech(&mut self) {
        self.hud = HudState::NoSpeech;
        self.done_at = Some(Instant::now());
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
        state.record_failure(Failure::blocked("No text field focused", "detail"));
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
        state.record_failure(Failure::dropped("Couldn't insert", "Target application changed"));
        assert_eq!(state.history[0].transcript, "half a sentence");
    }

    /// The dot exists to send the user to the console. Setting it while the
    /// console is open leaves a dot nothing can clear.
    #[test]
    fn a_failure_raised_while_the_console_is_open_does_not_set_the_dot() {
        let mut state = AppState { console_open: true, ..Default::default() };
        state.record_failure(Failure::dropped("Transcription failed", "worker died"));
        assert!(!state.unread_failure);
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
}
