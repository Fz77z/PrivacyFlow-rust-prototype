//! Record what one dictation cost, from the key coming up to text appearing.
//!
//! The worker already traces its own half. This is the other half, and the two
//! are correlated on the utterance's WAV filename stem: the worker sees it in
//! the audio path it is given, and the app knows it because it chose the name.
//! Pairing on that rather than on timestamps or arrival order means a dropped
//! or reordered record cannot silently join the wrong halves together.
//!
//! Written beside the worker's trace, in the research checkout, so one
//! directory holds the whole picture.
//!
//! This is measurement, not behaviour. It runs after the result has been sent
//! to the UI, and a failure to write costs a measurement and nothing else.

use crate::state::{Route, Timings};
use serde::Serialize;
use std::io::Write;

#[derive(Serialize)]
pub struct LatencyTrace<'a> {
    pub trace_id: &'a str,
    pub captured_at: String,
    /// `inserted`, `copied` or `failed`, so a slow dictation can be told from
    /// one that never arrived.
    pub outcome: &'static str,
    pub route: Option<Route>,
    #[serde(flatten)]
    pub timings: &'a Timings,
}

/// Append one dictation's timings, if there is somewhere to put them.
///
/// Silent when the research checkout cannot be found: tracing is a diagnostic
/// that must never become a reason a dictation reports a failure.
pub fn record(trace: &LatencyTrace<'_>) {
    let Ok(root) = crate::router::research_root() else {
        return;
    };
    let path = root.join("data").join("shadow").join("localflow_traces.jsonl");
    let Ok(line) = serde_json::to_string(trace) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let opened = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    match opened {
        Ok(mut file) => {
            if let Err(error) = writeln!(file, "{line}") {
                eprintln!("LocalFlow could not record a latency trace: {error}");
            }
        }
        Err(error) => {
            eprintln!(
                "LocalFlow could not open {} for latency tracing: {error}",
                path.display()
            );
        }
    }
}

/// A failure the app raised itself, before any work reached the pipeline.
///
/// These used to disappear entirely: the worker traces its own failures and
/// the pipeline traces its results, but a dictation refused at the microphone
/// left no record at all. Metadata only - what failed and where, never what
/// was said. There is no field here that could hold dictated text, and a test
/// pins that.
#[derive(Serialize)]
pub struct AppFailure<'a> {
    pub captured_at: String,
    pub outcome: &'static str,
    pub failing_stage: &'static str,
    pub kind: &'static str,
    pub headline: &'a str,
    pub detail: &'a str,
}

/// Build the record for an app-side failure.
///
/// Separate from writing it so the shape is testable without a filesystem.
pub fn app_failure<'a>(
    stage: &'static str,
    failure: &'a crate::state::Failure,
) -> AppFailure<'a> {
    AppFailure {
        captured_at: chrono::Utc::now().to_rfc3339(),
        outcome: "failed",
        failing_stage: stage,
        kind: match failure.kind {
            crate::state::FailureKind::Blocked => "blocked",
            crate::state::FailureKind::InputUnavailable => "input_unavailable",
            crate::state::FailureKind::Dropped => "dropped",
        },
        headline: failure.headline,
        detail: &failure.detail,
    }
}

/// A capture the app refused before it became a dictation.
///
/// Numbers only, and by construction: nothing was transcribed, so there is
/// nothing here that could carry what the user said. It is recorded at all
/// because a floor that silently discards presses has to be reviewable
/// against the presses it discarded.
#[derive(serde::Serialize)]
struct Silence {
    captured_at: String,
    outcome: &'static str,
    failing_stage: &'static str,
    audio_seconds: f64,
    rms: f32,
    /// The loudest window, recorded beside the mean that decided. It is here
    /// to be compared against, not acted on.
    peak_rms: f32,
}

fn silence(seconds: f64, rms: f32, peak: f32) -> Silence {
    Silence {
        captured_at: chrono::Utc::now().to_rfc3339(),
        outcome: "no_speech",
        failing_stage: "capture",
        audio_seconds: (seconds * 1000.0).round() / 1000.0,
        rms,
        peak_rms: peak,
    }
}

/// Append a refused capture, if there is somewhere to put it.
pub fn record_silence(seconds: f64, rms: f32, peak: f32) {
    append(&silence(seconds, rms, peak));
}

/// Append a capture the floor let through.
///
/// The counterpart of the refusals. A floor can only be judged against both
/// sides of it: what it turned away says nothing about what it should have.
pub fn record_capture(seconds: f64, rms: f32, peak: f32) {
    append(&Silence {
        captured_at: chrono::Utc::now().to_rfc3339(),
        outcome: "captured",
        failing_stage: "capture",
        audio_seconds: (seconds * 1000.0).round() / 1000.0,
        rms,
        peak_rms: peak,
    });
}

/// Append an app-side failure, if there is somewhere to put it.
pub fn record_app_failure(stage: &'static str, failure: &crate::state::Failure) {
    append(&app_failure(stage, failure));
}

/// Write one record to the app's own trace, if there is somewhere to put it.
fn append(record: &impl serde::Serialize) {
    let Ok(root) = crate::router::research_root() else {
        return;
    };
    let path = root.join("data").join("shadow").join("localflow_traces.jsonl");
    let Ok(line) = serde_json::to_string(record) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Failure;

    /// The trace is written to disk and kept. Dictated text is transient by
    /// policy, so a failure record must carry what went wrong and nothing the
    /// user said.
    #[test]
    fn an_app_failure_trace_carries_no_dictated_content() {
        let failure = Failure::dropped(
            "Transcription failed",
            "The processing worker stopped unexpectedly",
        );
        let record = serde_json::to_value(app_failure("finish_recording", &failure)).unwrap();
        // serde_json orders keys alphabetically, so this is a set comparison.
        let keys: Vec<_> = record.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            vec!["captured_at", "detail", "failing_stage", "headline", "kind", "outcome"],
            "a new field here is a new way for dictated text to reach the disk"
        );
        assert_eq!(record["kind"], "dropped");
        assert_eq!(record["failing_stage"], "finish_recording");
    }
}
