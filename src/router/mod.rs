use crate::state::Route;
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

/// Loading mlx-whisper and the Kev checkpoint, then warming both, is slow but
/// bounded. Past this the worker is wedged rather than starting.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

/// The fixed part of one utterance's budget: request framing, model dispatch,
/// the routing decision, and an S1-mini rewrite, none of which grow with the
/// length of the audio.
const INFERENCE_BASE_TIMEOUT: Duration = Duration::from_secs(60);

/// The part that does grow with the audio. Transcription runs many times
/// faster than real time, so this is deliberately loose: the timeout exists to
/// catch a wedged worker, and a flat bound would let a merely slow machine
/// look identical to one and kill an otherwise healthy process.
const INFERENCE_TIMEOUT_PER_AUDIO_SECOND: u32 = 4;

/// How long one utterance may take before the worker counts as wedged.
fn inference_timeout(audio: Duration) -> Duration {
    INFERENCE_BASE_TIMEOUT + audio * INFERENCE_TIMEOUT_PER_AUDIO_SECOND
}

#[derive(Debug, Clone)]
pub struct RouteResult {
    pub route: Route,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone)]
pub struct InferenceResult {
    pub transcript: String,
    pub asr_ms: u128,
    pub route: RouteResult,
    /// The text the route produced, which is the raw transcript for
    /// `PASS_THROUGH` and the processor's rewrite for a processed route.
    pub output: String,
    /// Set only when a route actually ran a text processor.
    pub processing_ms: Option<u128>,
}

/// The resident bridge to the exact research setup: mlx-whisper large-v3-turbo
/// plus the `scaling_run/checkpoints/pool_300` Kev checkpoint. This is a narrow
/// MVP seam, not a generic model-hosting protocol.
///
/// Responses are read through a channel rather than directly from the pipe so
/// that a wedged Python process fails explicitly instead of blocking the
/// pipeline thread, and every dictation after it, forever.
pub struct KevWorker {
    child: Child,
    input: ChildStdin,
    responses: Receiver<String>,
    fatal: Option<String>,
}

impl KevWorker {
    pub fn start() -> Result<Self> {
        let research = research_root()?;
        let python = research.join(".venv-kev/bin/python");
        let script = research.join("scripts/localflow_worker.py");
        if !python.is_file() || !script.is_file() {
            return Err(anyhow!(
                "LocalFlow research runtime is unavailable at {}",
                research.display()
            ));
        }

        let mut child = Command::new(python)
            .arg(script)
            .current_dir(&research)
            .env("HF_HUB_OFFLINE", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("Could not start the resident pool_300 inference worker")?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Inference worker stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Inference worker stdout unavailable"))?;

        // The reader thread ends when the worker closes stdout, which
        // disconnects the channel and surfaces as an explicit error below...
        let (response_tx, responses) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("localflow-worker-reader".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if response_tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .context("Could not start the inference worker reader")?;

        let mut worker = Self {
            child,
            input,
            responses,
            fatal: None,
        };

        #[derive(Deserialize)]
        struct Ready {
            ready: bool,
            error: Option<String>,
        }
        let line = worker.read_response(STARTUP_TIMEOUT, "while loading its models")?;
        let ready: Ready =
            serde_json::from_str(&line).context("Inference worker sent invalid startup JSON")?;
        if !ready.ready {
            return Err(anyhow!(
                "Inference worker failed to load: {}",
                ready.error.unwrap_or_else(|| "unknown error".to_owned())
            ));
        }
        Ok(worker)
    }

    /// `audio_duration` sizes this request's wedge timeout; it is not used for
    /// anything the worker decides.
    pub fn transcribe_and_route(
        &mut self,
        audio_path: &Path,
        audio_duration: Duration,
    ) -> Result<InferenceResult> {
        #[derive(Serialize)]
        struct Request<'a> {
            audio_path: &'a str,
        }

        if let Some(reason) = &self.fatal {
            return Err(anyhow!("{reason}. Quit and reopen LocalFlow."));
        }
        let path = audio_path
            .to_str()
            .ok_or_else(|| anyhow!("Audio path is not valid UTF-8"))?;
        if let Err(error) = self.write_request(&Request { audio_path: path }) {
            return Err(self.poison(format!(
                "The inference worker stopped accepting audio: {error}"
            )));
        }
        let line = self.read_response(inference_timeout(audio_duration), "while transcribing")?;

        // A per-utterance failure is reported in the response itself and
        // leaves the resident worker healthy, so it must not poison it...
        parse_response(&line)
    }

    fn write_request<T: Serialize>(&mut self, request: &T) -> std::io::Result<()> {
        serde_json::to_writer(&mut self.input, request)?;
        self.input.write_all(b"\n")?;
        self.input.flush()
    }

    /// Wait for one JSONL response, treating a timeout or a closed pipe as a
    /// permanent worker failure rather than something to retry into.
    fn read_response(&mut self, timeout: Duration, activity: &str) -> Result<String> {
        match self.responses.recv_timeout(timeout) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => Err(self.poison(format!(
                "The inference worker stopped responding {activity} after {} seconds",
                timeout.as_secs()
            ))),
            Err(RecvTimeoutError::Disconnected) => {
                Err(self.poison("The inference worker exited".to_owned()))
            }
        }
    }

    /// Record a permanent failure and stop the child, so later dictations fail
    /// immediately with the same explanation instead of hanging again.
    fn poison(&mut self, reason: String) -> anyhow::Error {
        let _ = self.child.kill();
        let error = anyhow!("{reason}. Quit and reopen LocalFlow.");
        self.fatal = Some(reason);
        error
    }
}

/// One JSONL reply from the worker. Every field is optional on the wire so
/// that a malformed reply is reported as a missing field rather than as a
/// parse failure that says nothing about what was wrong.
#[derive(Deserialize)]
struct Response {
    transcript: Option<String>,
    asr_ms: Option<f64>,
    route: Option<Route>,
    router_ms: Option<f64>,
    output: Option<String>,
    processor: Option<String>,
    processing_ms: Option<f64>,
    error: Option<String>,
}

/// Turn one worker reply into the result the pipeline inserts.
///
/// This is the whole contract between the Python worker and the application,
/// so it is kept separate from the transport in order to stay testable.
fn parse_response(line: &str) -> Result<InferenceResult> {
    let response: Response =
        serde_json::from_str(line).context("Inference worker sent invalid JSON")?;
    if let Some(error) = response.error {
        return Err(anyhow!(error));
    }

    // A processor name is what distinguishes a rewritten route from a
    // pass-through, so its latency is only reported when one actually ran.
    let processed_by = response.processor.unwrap_or_default();
    let processing_ms = if processed_by.is_empty() {
        None
    } else {
        Some(
            response
                .processing_ms
                .ok_or_else(|| anyhow!("Inference worker omitted processing latency"))?
                .round() as u128,
        )
    };
    Ok(InferenceResult {
        transcript: response
            .transcript
            .ok_or_else(|| anyhow!("Inference worker omitted transcript"))?,
        output: response
            .output
            .ok_or_else(|| anyhow!("Inference worker omitted output text"))?,
        processing_ms,
        asr_ms: response
            .asr_ms
            .ok_or_else(|| anyhow!("Inference worker omitted ASR latency"))?
            .round() as u128,
        route: RouteResult {
            route: response
                .route
                .ok_or_else(|| anyhow!("Inference worker omitted route"))?,
            elapsed_ms: response
                .router_ms
                .ok_or_else(|| anyhow!("Inference worker omitted router latency"))?
                .round() as u128,
        },
    })
}

impl Drop for KevWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn research_root() -> Result<PathBuf> {
    let app_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let parent = app_root
        .parent()
        .ok_or_else(|| anyhow!("LocalFlow has no parent directory"))?;
    Ok(parent.join("localflow-research"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand in for a wedged worker: a real child that accepts a request and
    /// never answers. Loading the actual models is not needed to prove this.
    /// The returned sender must outlive the worker, otherwise the channel
    /// disconnects and the worker sees an exit rather than a silent stall.
    fn silent_worker() -> (KevWorker, crossbeam_channel::Sender<String>) {
        let mut child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("could not spawn the stand-in worker");
        let input = child.stdin.take().unwrap();
        let (response_tx, responses) = crossbeam_channel::unbounded();
        let worker = KevWorker {
            child,
            input,
            responses,
            fatal: None,
        };
        (worker, response_tx)
    }

    /// A worker that stops answering must fail the utterance explicitly, and
    /// every later utterance must fail immediately rather than hang again.
    #[test]
    fn a_silent_worker_times_out_instead_of_blocking_forever() {
        let (mut worker, _response_tx) = silent_worker();
        let timed_out = worker
            .read_response(Duration::from_millis(50), "while transcribing")
            .unwrap_err()
            .to_string();
        assert!(timed_out.contains("stopped responding"), "{timed_out}");
        assert!(worker.fatal.is_some());

        let after = worker
            .transcribe_and_route(Path::new("/tmp/never-read.wav"), Duration::from_secs(3))
            .unwrap_err()
            .to_string();
        assert!(after.contains("Quit and reopen LocalFlow"), "{after}");
    }

    // Captured verbatim from the real worker, so a change to its reply shape
    // fails here rather than silently at the moment text is inserted.
    const PASS_THROUGH_REPLY: &str = r#"{"transcript": "This was written using the dictation.", "asr_ms": 1131.521666000026, "asr_model": "mlx-community/whisper-large-v3-turbo", "route": "PASS_THROUGH", "scores": {"PASS_THROUGH": 1.0}, "router_ms": 133.1131249999089, "rules_route": "PASS_THROUGH", "output": "This was written using the dictation.", "processor": "", "processed": "", "processing_ms": 0.0, "post_asr_ms": 133.97916700023416}"#;
    const TRANSFORM_REPLY: &str = r#"{"transcript": "Yo yo yo it's your boy", "asr_ms": 1045.2008329998534, "asr_model": "mlx-community/whisper-large-v3-turbo", "route": "TRANSFORM", "scores": {"TRANSFORM": 1.0}, "router_ms": 157.617541000036, "rules_route": "TRANSFORM", "output": "Yo yo yo, it's your boy.", "processor": "superwhisper/s1-mini", "processed": "Yo yo yo, it's your boy.", "processing_ms": 394.2971670003317, "post_asr_ms": 553.1987919998755}"#;

    #[test]
    fn a_pass_through_reply_inserts_the_raw_transcript_and_reports_no_processing() {
        let result = parse_response(PASS_THROUGH_REPLY).unwrap();
        assert_eq!(result.route.route, Route::PassThrough);
        assert_eq!(result.output, "This was written using the dictation.");
        assert_eq!(result.output, result.transcript);
        assert_eq!(result.processing_ms, None);
    }

    #[test]
    fn a_processed_reply_inserts_the_rewrite_rather_than_the_transcript() {
        let result = parse_response(TRANSFORM_REPLY).unwrap();
        assert_eq!(result.route.route, Route::Transform);
        assert_eq!(result.transcript, "Yo yo yo it's your boy");
        assert_eq!(result.output, "Yo yo yo, it's your boy.");
        assert_eq!(result.processing_ms, Some(394));
    }

    #[test]
    fn a_worker_error_reply_is_reported_and_never_produces_text() {
        let error = parse_response(r#"{"error": "COMPLEX processing is not implemented yet"}"#)
            .unwrap_err()
            .to_string();
        assert_eq!(error, "COMPLEX processing is not implemented yet");
    }

    /// The bug this guards: the timeout used to equal the recorder's maximum
    /// audio length, so transcribing a full two-minute utterance on a slow
    /// machine could be mistaken for a wedged worker and poison it.
    #[test]
    fn the_inference_timeout_always_outgrows_the_audio_it_covers() {
        for seconds in [0, 1, 30, 120] {
            let audio = Duration::from_secs(seconds);
            assert!(inference_timeout(audio) > audio * 2, "{seconds}s of audio");
        }
    }

    #[test]
    fn kev_route_names_match_the_worker_protocol() {
        assert_eq!(
            serde_json::from_str::<Route>("\"LIGHT_CLEANUP\"").unwrap(),
            Route::LightCleanup
        );
        assert_eq!(
            serde_json::to_string(&Route::PassThrough).unwrap(),
            "\"PASS_THROUGH\""
        );
    }
}
