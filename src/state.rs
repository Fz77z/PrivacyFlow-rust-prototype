use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HudState {
    Idle,
    Listening,
    Processing,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingState {
    Idle,
    Recording,
    Processing,
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

#[derive(Debug, Clone)]
pub struct DebugRecord {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub transcript: String,
    pub route: Option<Route>,
    pub output: String,
    pub timings: Timings,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub recording: RecordingState,
    pub hud: HudState,
    pub mic_level: f32,
    pub transcript: String,
    pub route: Option<Route>,
    pub output: String,
    pub timings: Timings,
    pub last_error: Option<String>,
    pub history: Vec<DebugRecord>,
    pub debug_open: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            recording: RecordingState::Idle,
            hud: HudState::Idle,
            mic_level: 0.0,
            transcript: String::new(),
            route: None,
            output: String::new(),
            timings: Timings::default(),
            last_error: None,
            history: Vec::new(),
            debug_open: false,
        }
    }
}

impl AppState {
    pub fn reset_for_recording(&mut self) {
        self.recording = RecordingState::Recording;
        self.hud = HudState::Listening;
        self.mic_level = 0.0;
        self.transcript.clear();
        self.route = None;
        self.output.clear();
        self.timings = Timings::default();
        self.last_error = None;
    }

    pub fn push_history(&mut self, error: Option<String>) {
        self.history.insert(
            0,
            DebugRecord {
                timestamp: chrono::Utc::now(),
                transcript: self.transcript.clone(),
                route: self.route,
                output: self.output.clone(),
                timings: self.timings.clone(),
                error,
            },
        );
        self.history.truncate(50);
    }
}

pub fn dur_ms(d: Duration) -> u128 {
    d.as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_reset_clears_the_previous_result() {
        let mut state = AppState {
            transcript: "old".into(),
            output: "old output".into(),
            route: Some(Route::Complex),
            last_error: Some("old error".into()),
            ..Default::default()
        };
        state.reset_for_recording();
        assert_eq!(state.recording, RecordingState::Recording);
        assert_eq!(state.hud, HudState::Listening);
        assert!(
            state.transcript.is_empty()
                && state.output.is_empty()
                && state.route.is_none()
                && state.last_error.is_none()
        );
    }
}
