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

/// Generous upper bound for transcribing and routing one utterance, which the
/// recorder already caps at two minutes of audio.
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(120);

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
    pub shadow_warning: Option<String>,
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

    pub fn transcribe_and_route(&mut self, audio_path: &Path) -> Result<InferenceResult> {
        #[derive(Serialize)]
        struct Request<'a> {
            audio_path: &'a str,
        }
        #[derive(Deserialize)]
        struct Response {
            transcript: Option<String>,
            asr_ms: Option<f64>,
            route: Option<Route>,
            router_ms: Option<f64>,
            shadow_error: Option<String>,
            error: Option<String>,
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
        let line = self.read_response(INFERENCE_TIMEOUT, "while transcribing")?;

        // A JSON error is a per-utterance failure the worker recovers from, so
        // it must not poison the still-healthy resident process...
        let response: Response =
            serde_json::from_str(&line).context("Inference worker sent invalid JSON")?;
        if let Some(error) = response.error {
            return Err(anyhow!(error));
        }
        Ok(InferenceResult {
            transcript: response
                .transcript
                .ok_or_else(|| anyhow!("Inference worker omitted transcript"))?,
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
            shadow_warning: response.shadow_error,
        })
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
            .transcribe_and_route(Path::new("/tmp/never-read.wav"))
            .unwrap_err()
            .to_string();
        assert!(after.contains("Quit and reopen LocalFlow"), "{after}");
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
