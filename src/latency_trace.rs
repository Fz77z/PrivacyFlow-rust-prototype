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
