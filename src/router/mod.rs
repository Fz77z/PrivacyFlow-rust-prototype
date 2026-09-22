use crate::state::Route;
use anyhow::{anyhow, Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
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
pub struct InferenceResult {
    pub transcript: String,
    pub asr_ms: u128,
    pub route: Route,
    pub router_ms: u128,
    /// The text the route produced, which is the raw transcript for
    /// `PASS_THROUGH` and the processor's rewrite for a processed route.
    pub output: String,
    /// Set only when a route actually ran a text processor.
    pub processing_ms: Option<u128>,
    /// Which recognizer produced `transcript`. Carried so a retained corpus
    /// says what its own baseline was, rather than being ambiguous about it if
    /// the production model ever changes mid-collection.
    pub asr_model: String,
}

/// What one reply from the worker means.
///
/// Silence is neither a transcript nor a failure. The worker measures every
/// capture before transcribing it, because Whisper does not return nothing for
/// silence, it invents a plausible sentence. A capture below that floor has
/// nothing to route and nothing to insert, which is a different answer from
/// the transcriber breaking and must not be reported as one.
#[derive(Debug)]
pub enum Reply {
    Transcribed(InferenceResult),
    NoSpeech,
    /// Speech was heard and could not be decoded. Distinct from silence
    /// because the user did say something and it was thrown away, and it
    /// carries the numbers behind that so the console can show them.
    ///
    /// `detail` is composed by the worker rather than here, because there is
    /// more than one way for a decode to be refused and the number that
    /// explains one says nothing about the other: a repetition loop scored a
    /// perfectly healthy log probability while it was looping. It is
    /// measurements only, never any part of what was decoded.
    Unintelligible {
        seconds: f64,
        detail: String,
    },
    /// The utterance failed after the worker had already recognised speech.
    /// It carries the transcript so the words can be preserved rather than
    /// lost: routing, rewriting and the safety guards can all refuse without
    /// the user losing what they said. `transcript` is absent when the
    /// failure happened before there was anything to recognise.
    Failed {
        message: String,
        transcript: Option<String>,
    },
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
        let (python, script) = runtime_paths(&research)?;

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
            .name("privacyflow-worker-reader".into())
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

    /// Ask the worker to make the router and the text processor resident
    /// while the user is still speaking.
    ///
    /// Under memory pressure macOS compresses idle weights within seconds,
    /// and decompressing them used to cost the first route after a pause
    /// roughly 450ms. Paying that during speech takes it off the wait after
    /// release. S1-mini is warmed for the same reason and goes idle longer,
    /// because most dictations pass through without a rewrite. A dictation
    /// released before this finishes waits for the remainder, which is work
    /// its own route and rewrite would otherwise have done.
    /// See docs/investigations/2026-09-22-router-latency-and-benchmark-protocol.md.
    ///
    /// A reply that is not the acknowledgement means the request and reply
    /// streams no longer line up, and every later dictation would receive
    /// the wrong answer, so that poisons the worker. A warm-up the worker
    /// reports as failed leaves it healthy and is returned as an error.
    pub fn prepare(&mut self) -> Result<()> {
        #[derive(Serialize)]
        struct Request {
            prepare: bool,
        }

        if let Some(reason) = &self.fatal {
            return Err(anyhow!("{reason}. Quit and reopen PrivacyFlow."));
        }
        if let Err(error) = self.write_request(&Request { prepare: true }) {
            return Err(self.poison(format!(
                "The inference worker stopped accepting requests: {error}"
            )));
        }
        let line = self.read_response(INFERENCE_BASE_TIMEOUT, "while preparing the router")?;
        match parse_prepared(&line) {
            Ok(Prepared::Ready) => Ok(()),
            Ok(Prepared::Failed(message)) => {
                Err(anyhow!("The worker could not prepare the models: {message}"))
            }
            Err(error) => Err(self.poison(format!(
                "The inference worker answered a warm-up with something else ({error:#})"
            ))),
        }
    }

    /// `audio_duration` sizes this request's wedge timeout; it is not used for
    /// anything the worker decides.
    pub fn transcribe_and_route(
        &mut self,
        audio_path: &Path,
        audio_duration: Duration,
    ) -> Result<Reply> {
        #[derive(Serialize)]
        struct Request<'a> {
            audio_path: &'a str,
        }

        if let Some(reason) = &self.fatal {
            return Err(anyhow!("{reason}. Quit and reopen PrivacyFlow."));
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
        let error = anyhow!("{reason}. Quit and reopen PrivacyFlow.");
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
    asr_model: Option<String>,
    route: Option<Route>,
    router_ms: Option<f64>,
    output: Option<String>,
    processor: Option<String>,
    processing_ms: Option<f64>,
    error: Option<String>,
    no_speech: Option<bool>,
    unintelligible: Option<bool>,
    audio_seconds: Option<f64>,
    detail: Option<String>,
}

/// What the worker said about a warm-up request.
#[derive(Debug, PartialEq)]
enum Prepared {
    Ready,
    /// The worker is healthy but the warm-up itself raised.
    Failed(String),
}

/// Read the reply to a warm-up request, refusing anything else.
///
/// Kept apart from the transport for the same reason as `parse_response`:
/// this is the part that decides whether the protocol is still in step.
fn parse_prepared(line: &str) -> Result<Prepared> {
    #[derive(Deserialize)]
    struct Reply {
        prepared: Option<bool>,
        error: Option<String>,
    }
    let reply: Reply =
        serde_json::from_str(line).context("Inference worker sent invalid JSON")?;
    if let Some(error) = reply.error {
        return Ok(Prepared::Failed(error));
    }
    if reply.prepared == Some(true) {
        return Ok(Prepared::Ready);
    }
    // The line itself is left out: out of step, it may be a dictation's reply,
    // and that carries what the user said.
    Err(anyhow!("expected a warm-up acknowledgement"))
}

/// Turn one worker reply into the result the pipeline inserts.
///
/// This is the whole contract between the Python worker and the application,
/// so it is kept separate from the transport in order to stay testable.
fn parse_response(line: &str) -> Result<Reply> {
    let response: Response =
        serde_json::from_str(line).context("Inference worker sent invalid JSON")?;
    if let Some(error) = response.error {
        // A per-utterance failure, not a broken worker, so it travels as a
        // reply rather than an error: the worker is still healthy and the
        // next dictation must not be poisoned by this one.
        return Ok(Reply::Failed {
            message: error,
            transcript: response.transcript,
        });
    }
    // Checked after the error and before everything else: a reply that says
    // nothing was said carries none of the fields a transcript must have.
    if response.no_speech == Some(true) {
        return Ok(Reply::NoSpeech);
    }
    // Checked in the same place and for the same reason: a reply that says
    // the decode was not speech carries none of the fields a transcript must
    // have. The numbers default to zero rather than refusing the reply: the
    // answer is what matters, and the figures only decorate the console.
    if response.unintelligible == Some(true) {
        return Ok(Reply::Unintelligible {
            seconds: response.audio_seconds.unwrap_or_default(),
            detail: response.detail.unwrap_or_else(|| "no reason given".to_owned()),
        });
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
    Ok(Reply::Transcribed(InferenceResult {
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
        route: response
            .route
            .ok_or_else(|| anyhow!("Inference worker omitted route"))?,
        router_ms: response
            .router_ms
            .ok_or_else(|| anyhow!("Inference worker omitted router latency"))?
            .round() as u128,
        asr_model: response
            .asr_model
            .ok_or_else(|| anyhow!("Inference worker omitted the ASR model name"))?,
    }))
}

impl Drop for KevWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[link(name = "c")]
extern "C" {
    /// POSIX `kill`. Declared rather than taken from a binding crate, because
    /// one signal to one process does not justify a dependency.
    fn kill(pid: i32, signal: i32) -> i32;
}

/// A way to stop the worker from outside the thread that owns it.
///
/// `Drop for KevWorker` cannot do this job on its own. The worker lives on a
/// detached thread, and when the process exits that thread's stack is never
/// unwound, so the drop never runs. Worse, the thread spends the first ten
/// seconds or so blocked inside `KevWorker::start` while Python loads three
/// models, where it cannot notice that its work channel has closed. Quitting
/// during that window left the worker running, and it went on to finish
/// loading and write into a pipe nobody was holding.
#[derive(Clone, Default)]
pub struct WorkerShutdown(Arc<AtomicI32>);

impl WorkerShutdown {
    /// Remember a worker, so it can be stopped later.
    pub fn watch(&self, worker: &KevWorker) {
        self.0.store(worker.child.id() as i32, Ordering::Release);
    }

    /// Stop the worker, if one ever started.
    ///
    /// SIGKILL rather than a polite request: this runs while the application
    /// is going away, there is nothing left to co-ordinate with, and the
    /// worker holds no state worth flushing. Its own audio is already
    /// deleted, and its records are written as each dictation finishes.
    pub fn stop(&self) {
        let pid = self.0.swap(0, Ordering::AcqRel);
        if pid <= 0 {
            return;
        }
        // SAFETY: a process id this process spawned, and SIGKILL, which is
        // defined for every process. A pid that has already exited fails
        // harmlessly with ESRCH.
        unsafe { kill(pid, 9) };
    }
}

/// Where the sibling research checkout lives.
///
/// This was once resolved with `env!("CARGO_MANIFEST_DIR")`, which the
/// compiler bakes in as the directory the binary was built in. That worked
/// only while PrivacyFlow ran from its own checkout, and broke silently the
/// moment either the app or the research repository moved.
pub(crate) fn research_root() -> Result<PathBuf> {
    let override_path = std::env::var_os("PRIVACYFLOW_RESEARCH_ROOT").map(PathBuf::from);
    resolve_research_root(override_path, dirs::home_dir())
}

/// The resolution itself, kept free of the environment so it can be tested.
fn resolve_research_root(
    override_path: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = override_path {
        return Ok(path);
    }
    let home =
        home.ok_or_else(|| anyhow!("PrivacyFlow could not determine the macOS home directory"))?;
    Ok(home.join("Desktop").join("localflow-research"))
}

/// The two files PrivacyFlow requires from the research checkout. This is a
/// contract with that project: if either moves, PrivacyFlow must say so
/// clearly rather than starting a worker that cannot run.
pub(crate) fn runtime_paths(root: &Path) -> Result<(PathBuf, PathBuf)> {
    let python = root.join(".venv-kev/bin/python");
    let script = root.join("scripts/localflow_worker.py");
    for (path, relative) in [
        (&python, ".venv-kev/bin/python"),
        (&script, "scripts/localflow_worker.py"),
    ] {
        if !path.is_file() {
            return Err(anyhow!(
                "PrivacyFlow could not find {relative} in the research runtime at {}. \
                 Set PRIVACYFLOW_RESEARCH_ROOT if the checkout is somewhere else.",
                root.display()
            ));
        }
    }
    Ok((python, script))
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
        assert!(after.contains("Quit and reopen PrivacyFlow"), "{after}");
    }

    /// The acknowledgement is consumed, so the next reply the pipeline reads
    /// belongs to the next request.
    #[test]
    fn a_warm_up_consumes_exactly_its_own_acknowledgement() {
        let (mut worker, response_tx) = silent_worker();
        response_tx
            .send(r#"{"prepared": true, "prepare_ms": 71.3}"#.to_owned())
            .unwrap();
        response_tx.send(PASS_THROUGH_REPLY.to_owned()).unwrap();

        worker.prepare().unwrap();
        let next = worker
            .read_response(Duration::from_millis(50), "while transcribing")
            .unwrap();
        assert_eq!(next, PASS_THROUGH_REPLY);
        assert!(worker.fatal.is_none());
    }

    /// A warm-up answered with anything else means requests and replies are
    /// out of step, and every later dictation would get the wrong answer. That
    /// must stop the worker, and must not repeat the stray reply, which can
    /// carry what the user said.
    #[test]
    fn a_warm_up_answered_out_of_step_poisons_the_worker() {
        let (mut worker, response_tx) = silent_worker();
        response_tx.send(PASS_THROUGH_REPLY.to_owned()).unwrap();

        let error = worker.prepare().unwrap_err().to_string();
        assert!(error.contains("Quit and reopen PrivacyFlow"), "{error}");
        assert!(!error.contains("written using the dictation"), "{error}");
        assert!(worker.fatal.is_some());
    }

    /// A warm-up the worker reports as failed leaves the worker usable: the
    /// dictation behind it still runs and reports its own outcome.
    #[test]
    fn a_failed_warm_up_does_not_poison_the_worker() {
        let (mut worker, response_tx) = silent_worker();
        response_tx
            .send(r#"{"error": "MPS backend out of memory"}"#.to_owned())
            .unwrap();

        let error = worker.prepare().unwrap_err().to_string();
        assert!(error.contains("MPS backend out of memory"), "{error}");
        assert!(worker.fatal.is_none());
    }

    // Captured verbatim from the real worker, so a change to its reply shape
    // fails here rather than silently at the moment text is inserted.
    const PASS_THROUGH_REPLY: &str = r#"{"transcript": "This was written using the dictation.", "asr_ms": 1131.521666000026, "asr_model": "mlx-community/whisper-large-v3-turbo", "route": "PASS_THROUGH", "scores": {"PASS_THROUGH": 1.0}, "router_ms": 133.1131249999089, "rules_route": "PASS_THROUGH", "output": "This was written using the dictation.", "processor": "", "processed": "", "processing_ms": 0.0, "post_asr_ms": 133.97916700023416}"#;
    const TRANSFORM_REPLY: &str = r#"{"transcript": "Yo yo yo it's your boy", "asr_ms": 1045.2008329998534, "asr_model": "mlx-community/whisper-large-v3-turbo", "route": "TRANSFORM", "scores": {"TRANSFORM": 1.0}, "router_ms": 157.617541000036, "rules_route": "TRANSFORM", "output": "Yo yo yo, it's your boy.", "processor": "superwhisper/s1-mini", "processed": "Yo yo yo, it's your boy.", "processing_ms": 394.2971670003317, "post_asr_ms": 553.1987919998755}"#;

    #[test]
    fn a_pass_through_reply_inserts_the_raw_transcript_and_reports_no_processing() {
        let Reply::Transcribed(result) = parse_response(PASS_THROUGH_REPLY).unwrap() else {
            panic!("a reply carrying a transcript is not silence");
        };
        assert_eq!(result.route, Route::PassThrough);
        assert_eq!(result.output, "This was written using the dictation.");
        assert_eq!(result.output, result.transcript);
        assert_eq!(result.processing_ms, None);
    }

    #[test]
    fn a_processed_reply_inserts_the_rewrite_rather_than_the_transcript() {
        let Reply::Transcribed(result) = parse_response(TRANSFORM_REPLY).unwrap() else {
            panic!("a reply carrying a transcript is not silence");
        };
        assert_eq!(result.route, Route::Transform);
        assert_eq!(result.transcript, "Yo yo yo it's your boy");
        assert_eq!(result.output, "Yo yo yo, it's your boy.");
        assert_eq!(result.processing_ms, Some(394));
    }

    /// Pressing the key and saying nothing is not a failed dictation. The
    /// worker measures the capture and answers this instead of an error, and
    /// the two must never collapse into each other: silence reported as a
    /// failure tells the user their words were lost, and a failure reported
    /// as silence hides a broken transcriber.
    #[test]
    fn a_silent_capture_is_reported_as_no_speech_rather_than_as_a_failure() {
        assert!(matches!(
            parse_response(r#"{"no_speech": true}"#).unwrap(),
            Reply::NoSpeech
        ));
    }

    /// Silence and an unintelligible decode are different answers and must
    /// not arrive as the same one. Nothing was said is a non-event; a
    /// dictation Whisper could not decode threw the user's words away, and
    /// the numbers behind that are what the console shows them.
    #[test]
    fn an_unintelligible_decode_is_its_own_reply_not_silence() {
        let reply = parse_response(
            r#"{"unintelligible": true, "audio_seconds": 2.763,
                "detail": "average log probability -7.89, floor -1"}"#,
        )
        .unwrap();
        match reply {
            Reply::Unintelligible { seconds, detail } => {
                assert_eq!(seconds, 2.763);
                assert_eq!(detail, "average log probability -7.89, floor -1");
            }
            other => panic!("expected an unintelligible reply, got {other:?}"),
        }
        assert!(matches!(parse_response(r#"{"no_speech": true}"#).unwrap(), Reply::NoSpeech));
    }

    /// The exact bytes the worker emits for a repetition loop, copied from its
    /// output rather than written by hand here.
    ///
    /// This is the half of the contract a Rust test can hold: the worker
    /// composes the phrase because it owns the thresholds, so the only thing
    /// that can silently break is the two sides disagreeing on the field. A
    /// mismatch would reach the user as "no reason given" beside a refusal,
    /// which looks like the worker being unhelpful rather than like a bug here.
    #[test]
    fn a_repetition_loop_arrives_with_the_number_that_actually_failed() {
        let reply = parse_response(
            r#"{"unintelligible": true, "audio_seconds": 5.296, "detail": "compression ratio 9.16, ceiling 2.4"}"#,
        )
        .unwrap();
        match reply {
            Reply::Unintelligible { seconds, detail } => {
                assert_eq!(seconds, 5.296);
                assert_eq!(detail, "compression ratio 9.16, ceiling 2.4");
                // The log probability was -0.179 while this was looping, which
                // is healthier than the median dictation. Showing it as the
                // reason is the mistake this field exists to prevent.
                assert!(!detail.contains("log probability"));
            }
            other => panic!("expected an unintelligible reply, got {other:?}"),
        }
    }

    /// A reply that carries an error is a failure whatever else it says, so
    /// the error is read first.
    #[test]
    fn an_error_is_still_a_failure_even_beside_a_no_speech_flag() {
        let Reply::Failed { message, .. } =
            parse_response(r#"{"no_speech": true, "error": "worker died"}"#).unwrap()
        else {
            panic!("an error outranks a no-speech flag");
        };
        assert_eq!(message, "worker died");
    }

    /// A per-utterance failure travels as a reply rather than an error: the
    /// worker is still healthy, and the next dictation must not be poisoned.
    #[test]
    fn a_worker_error_reply_is_reported_and_never_produces_text() {
        let Reply::Failed { message, transcript } =
            parse_response(r#"{"error": "COMPLEX processing is not implemented yet"}"#).unwrap()
        else {
            panic!("an error reply is a failure, not a transcript");
        };
        assert_eq!(message, "COMPLEX processing is not implemented yet");
        assert_eq!(transcript, None, "this failure produced no words to preserve");
    }

    /// Once the worker has recognised speech, a later failure must not take
    /// the words with it: they travel back so the app can preserve them.
    #[test]
    fn a_failure_after_recognition_carries_the_words_for_preservation() {
        let Reply::Failed { message, transcript } = parse_response(
            r#"{"error": "S1-mini returned an empty rewrite", "transcript": "what I said"}"#,
        )
        .unwrap() else {
            panic!("expected a failure");
        };
        assert_eq!(message, "S1-mini returned an empty rewrite");
        assert_eq!(transcript.as_deref(), Some("what I said"));
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

    /// The compile-time path was correct only while the binary ran from the
    /// checkout that built it. A bundle in /Applications must resolve its
    /// runtime by asking, not by remembering where it was compiled.
    #[test]
    fn an_explicit_override_wins_over_the_default() {
        let resolved = resolve_research_root(
            Some(PathBuf::from("/tmp/elsewhere")),
            Some(PathBuf::from("/Users/someone")),
        )
        .unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/elsewhere"));
    }

    #[test]
    fn without_an_override_the_runtime_is_found_beside_the_user_desktop() {
        let resolved =
            resolve_research_root(None, Some(PathBuf::from("/Users/someone"))).unwrap();
        assert_eq!(
            resolved,
            PathBuf::from("/Users/someone/Desktop/localflow-research")
        );
    }

    /// The other tests exercise the pure resolver. This one exercises
    /// `research_root` itself, which is the only place the variable's name and
    /// the decision to ask the environment rather than bake in a build-time
    /// path actually live. Reverting that body to `env!("CARGO_MANIFEST_DIR")`,
    /// or typoing the name, breaks the documented override in README and is
    /// invisible to every other test here.
    ///
    /// `set_var` mutates process-global state and Rust runs tests in the same
    /// process, so this is only safe while no other test reads or writes an
    /// environment variable. None does today. Check that before adding one.
    #[test]
    fn the_documented_override_variable_is_the_one_actually_read() {
        std::env::set_var("PRIVACYFLOW_RESEARCH_ROOT", "/tmp/override-probe");
        let resolved = research_root();
        std::env::remove_var("PRIVACYFLOW_RESEARCH_ROOT");
        assert_eq!(resolved.unwrap(), PathBuf::from("/tmp/override-probe"));
    }

    /// Failing without a home directory is better than guessing at one.
    #[test]
    fn no_home_directory_is_an_explicit_failure() {
        let error = resolve_research_root(None, None).unwrap_err().to_string();
        assert!(
            error.contains("home directory"),
            "error should say what could not be determined, got: {error}"
        );
    }

    /// A wrong path used to produce a worker that could not start, with nothing
    /// saying where PrivacyFlow had looked. The message must name the file, and
    /// it must name the right one: both entries of the contract are checked,
    /// because a regression that reported the wrong `relative` string would be
    /// invisible if only one arm were exercised.
    #[test]
    fn a_missing_runtime_file_is_named_in_the_error() {
        let dir = std::env::temp_dir().join(format!("privacyflow-root-{}", std::process::id()));
        // Removed before every assertion, so a failing one cannot leak it...
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".venv-kev/bin")).unwrap();

        let missing_python = runtime_paths(&dir).unwrap_err().to_string();
        std::fs::write(dir.join(".venv-kev/bin/python"), b"#!/bin/sh\n").unwrap();
        let missing_script = runtime_paths(&dir).unwrap_err().to_string();
        std::fs::remove_dir_all(&dir).unwrap();

        assert!(
            missing_python.contains(".venv-kev/bin/python"),
            "error should name the missing interpreter, got: {missing_python}"
        );
        assert!(
            missing_script.contains("scripts/localflow_worker.py"),
            "error should name the missing file, got: {missing_script}"
        );
        assert!(
            missing_script.contains(&dir.display().to_string()),
            "error should name the root it searched, got: {missing_script}"
        );
    }
}
