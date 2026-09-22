pub mod cues;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SupportedBufferSize};
use rtrb::{Consumer, Producer, RingBuffer};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The capture ceiling, in seconds.
///
/// Preallocated at startup, so this is a permanent memory cost rather than a
/// limit that only bites when reached: five minutes of mono f32 at 48kHz is
/// about 58 MB. It exists to stop a stuck modifier key recording forever, and
/// it is deliberately far beyond any plausible dictation.
///
/// Reaching it stops the recording. It does not discard what was captured.
const MAX_RECORDING_SECONDS: usize = 300;

/// Requested capture buffer, in frames.
///
/// This is the dominant cost of restarting a paused stream: the smaller the
/// buffer, the sooner the first callback arrives. Measured on the development
/// machine, the device default cost about 57 ms from `play()` to the first
/// sample, 128 frames about 33 ms, and 64 frames about 29 ms. 128 is taken
/// over 64 because it doubles the time the callback has to run for almost the
/// same gain.
const TARGET_BUFFER_FRAMES: u32 = 128;

/// A microphone opened ahead of the keypress and kept open until the system
/// input changes or its device goes away.
///
/// Opening the device and building the stream costs over a hundred
/// milliseconds, and doing that when the hotkey is pressed spends it out of
/// the first moments of speech. Paying it once at startup means a keypress
/// only has to restart an already-built stream.
///
/// It is bound to one device, not to whatever the system input is at the
/// moment: CoreAudio keeps a stream on the device it was built for. So it
/// remembers which device that was, and records whether the stream has failed, which
/// is how CoreAudio reports the device going away. `is_current` answers
/// whether it is still the right thing to record from.
///
/// The stream is paused whenever PrivacyFlow is not recording, so the
/// microphone is not live between dictations, and it is not live before the
/// first one either. That last part takes an explicit pause: cpal's CoreAudio
/// backend calls `AudioOutputUnitStart` inside `build_input_stream`, so a
/// freshly built stream is already running. Pausing it at the end of `open`
/// stops the IO while leaving the AudioUnit open and initialised, which is
/// the whole point of opening early.
pub struct Microphone {
    stream: cpal::Stream,
    /// Set by the stream's error callback. On macOS that callback fires when
    /// the device disappears, and cpal has already stopped the stream by
    /// then, so nothing more will ever be captured from it.
    failed: Arc<AtomicBool>,
    /// Only the pipeline thread ever locks this. The audio callback holds the
    /// producer and never touches the lock, so the real-time path stays
    /// wait-free.
    samples: Arc<Mutex<Consumer<f32>>>,
    sample_rate: u32,
    level: Arc<AtomicU32>,
    overflowed: Arc<AtomicBool>,
    device_name: String,
    /// What this recording measures so far, so a press heading for refusal
    /// can be answered while it is still being held.
    recorded: Arc<RecordedLevel>,
}

impl Microphone {
    pub fn open() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("No input microphone found"))?;
        let name = device
            .name()
            .unwrap_or_else(|_| "unnamed input device".to_owned());
        let supported = device
            .default_input_config()
            .context("Could not read default microphone config")?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        if channels == 0 {
            return Err(anyhow!("Microphone reported zero input channels"));
        }
        let buffer_size = requested_buffer_size(supported.buffer_size());

        // The callback downmixes to mono, so this is a fixed, preallocated
        // upper bound of two minutes rather than an unbounded Vec.
        let capacity = (sample_rate as usize)
            .checked_mul(MAX_RECORDING_SECONDS)
            .ok_or_else(|| anyhow!("Microphone sample rate is too large"))?;
        let (mut producer, consumer) = RingBuffer::new(capacity);
        let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let callback_level = level.clone();
        let recorded = Arc::new(RecordedLevel::default());
        let callback_recorded = recorded.clone();
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = overflowed.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let callback_failed = failed.clone();
        let err_fn = move |err| {
            eprintln!("PrivacyFlow audio stream error: {err}");
            callback_failed.store(true, Ordering::Relaxed);
        };

        let mut config: cpal::StreamConfig = supported.clone().into();
        config.buffer_size = buffer_size;
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    push_mono(
                        data,
                        &mut producer,
                        channels,
                        |sample| sample,
                        &callback_level,
                        &callback_overflowed,
                        &callback_recorded,
                    )
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    push_mono(
                        data,
                        &mut producer,
                        channels,
                        |sample| sample as f32 / i16::MAX as f32,
                        &callback_level,
                        &callback_overflowed,
                        &callback_recorded,
                    )
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    push_mono(
                        data,
                        &mut producer,
                        channels,
                        |sample| (sample as f32 / u16::MAX as f32) * 2.0 - 1.0,
                        &callback_level,
                        &callback_overflowed,
                        &callback_recorded,
                    )
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("Unsupported microphone sample format: {other:?}")),
        };

        // cpal starts the unit as part of building the stream. Stop the IO
        // again immediately: the device stays open, but nothing is captured
        // until the first keypress...
        stream
            .pause()
            .context("Could not pause the microphone stream after opening it")?;

        Ok(Self {
            stream,
            failed,
            samples: Arc::new(Mutex::new(consumer)),
            sample_rate,
            level,
            recorded,
            overflowed,
            device_name: name,
        })
    }

    /// Start capturing. Any samples still in the buffer belong to a recording
    /// that has already been handed on, so they are discarded rather than
    /// prepended to this one.
    pub fn start_recording(&self) -> Result<()> {
        {
            let mut samples = self
                .samples
                .lock()
                .map_err(|_| anyhow!("The audio buffer lock was poisoned"))?;
            while samples.pop().is_ok() {}
        }
        self.overflowed.store(false, Ordering::Relaxed);
        self.level.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.recorded.clear();
        self.stream
            .play()
            .context("Could not start microphone stream")
    }

    /// Stop capturing and hand back everything recorded.
    ///
    /// This runs on the UI thread the moment the hotkey is released, so it
    /// does no draining and no encoding; the caller finishes the recording off
    /// that thread.
    pub fn stop_recording(&self) -> Result<CapturedAudio> {
        // A failed stream was already stopped by cpal, and its device may no
        // longer exist to be told to pause. What it captured before failing
        // is still in the buffer and still wanted.
        if !self.has_failed() {
            self.stream
                .pause()
                .context("Could not pause microphone stream")?;
        }
        self.level.store(0.0f32.to_bits(), Ordering::Relaxed);
        // An overflow is reported rather than thrown. The ring buffer refuses
        // new samples once it is full, so what was captured is the *start* of
        // the dictation and the tail is missing. Discarding it would turn a
        // partly-recorded dictation into no dictation at all.
        Ok(CapturedAudio {
            samples: self.samples.clone(),
            sample_rate: self.sample_rate,
            truncated: self.overflowed.load(Ordering::Relaxed),
            device_name: self.device_name.clone(),
        })
    }

    /// Whether this is still the microphone to record from: the stream has
    /// not failed, and the device is still the system input.
    ///
    /// Cheap enough to ask on every keypress. It is a single CoreAudio
    /// property read, which is far less than the stream build it can save.
    pub fn is_current(&self) -> bool {
        if self.has_failed() {
            return false;
        }
        // Compared by name because cpal exposes no device identity. Two inputs
        // with the same name would be mistaken for one another, which is the
        // one switch this does not see.
        cpal::default_host()
            .default_input_device()
            .and_then(|device| device.name().ok())
            .is_some_and(|name| name == self.device_name)
    }

    /// Whether the stream has failed since it was opened, which on macOS means
    /// the device went away.
    pub fn has_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    /// What the floor will be applied to if the key came up now.
    pub fn recorded_rms(&self) -> f32 {
        self.recorded.rms()
    }

    /// The device actually being recorded from.
    ///
    /// Reported in the console because PrivacyFlow follows the system input,
    /// and the system input changing under you is otherwise invisible: a
    /// Bluetooth headset that is also your output will be forced out of its
    /// high quality profile every time you dictate, and nothing on screen
    /// would say why the music broke up.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }
}

/// Ask for a small buffer, but never one the device has said it cannot serve.
fn requested_buffer_size(supported: &SupportedBufferSize) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } => {
            BufferSize::Fixed(TARGET_BUFFER_FRAMES.clamp(*min, *max))
        }
        // A device that will not state its range keeps its own default.
        SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

/// The floor below which a capture holds no speech.
///
/// These are the worker's own numbers, from
/// `localflow-research/src/dictation_router/asr.py` (`SILENCE_RMS`,
/// `MIN_SECONDS`), and a test compares the two against that file rather than
/// trusting this comment.
///
/// The policy is deliberately held in both places. This copy is the fast
/// answer: a press with nothing in it is refused here, before a file is
/// written or the worker is spoken to, so the capsule never flashes
/// "Transcribing" to tell the user that nothing happened. The worker keeps
/// its own because it is a separate program that must not trust whatever it
/// is handed: Whisper answers silence with an invented sentence, and that
/// safety net is what caught the invention in the first place.
pub const SILENCE_RMS: f32 = 0.0017;
pub const MIN_SECONDS: f64 = 0.35;

/// What the current recording measures so far, accumulated as it arrives.
///
/// The audio callback is the only writer and it runs on one thread, so a plain
/// load-then-store is enough; the atomics are here to be read from the UI
/// thread, not to arbitrate between writers. The sum is kept in f64 for the
/// same reason `root_mean_square` uses it: an f32 total over millions of
/// squared samples loses enough precision to move the decision.
#[derive(Default)]
pub struct RecordedLevel {
    sum_squares: AtomicU64,
    frames: AtomicU64,
}

impl RecordedLevel {
    /// Fold one buffer's squared samples into the recording so far.
    fn accumulate(&self, sum_squares: f64, frames: usize) {
        let total = f64::from_bits(self.sum_squares.load(Ordering::Relaxed)) + sum_squares;
        self.sum_squares.store(total.to_bits(), Ordering::Relaxed);
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
    }

    /// Forget the previous recording. Without this the floor would be applied
    /// to every press since the app started rather than to this one.
    fn clear(&self) {
        self.sum_squares.store(0.0f64.to_bits(), Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
    }

    /// The mean the floor will be applied to, as it stands right now.
    pub fn rms(&self) -> f32 {
        let frames = self.frames.load(Ordering::Relaxed);
        if frames == 0 {
            return 0.0;
        }
        let total = f64::from_bits(self.sum_squares.load(Ordering::Relaxed));
        (total / frames as f64).sqrt() as f32
    }
}

/// How long a press must be held before PrivacyFlow says anything about it
/// being quiet.
///
/// A warning is only worth showing while the user can still act on it. Shorter
/// presses than this are over before the toast could be read, let alone
/// answered by leaning in or speaking up.
pub const QUIET_WARNING_AFTER: Duration = Duration::from_secs(3);

/// Whether a press that has been held for `held`, and whose captured audio so
/// far measures `recorded_rms`, is heading for refusal.
///
/// Measured against the same floor `verdict` will apply when the key comes up,
/// so this does not estimate the outcome, it predicts it exactly: if nothing
/// changes, this capture is refused. It stays actionable because the recording
/// is still running, and speaking up now lifts the mean back over the floor.
pub fn heading_for_refusal(held: Duration, recorded_rms: f32) -> bool {
    held >= QUIET_WARNING_AFTER && recorded_rms < SILENCE_RMS
}

/// The span the loudest-moment measurement is taken over.
///
/// Speech is bursty and silence is not, so the strongest short window
/// separates them far more sharply than one mean over the whole capture,
/// which reads a dictation with thinking pauses as quiet and a silent capture
/// with one door slam as loud.
///
/// It does not decide anything yet. It is recorded beside the mean that does,
/// because a threshold for it has to come from its own spread rather than
/// from numbers measured a different way.
pub const WINDOW_SECONDS: f64 = 0.1;

/// What a finished capture turned out to be.
///
/// An energy gate, not a speech detector. It answers "is there anything here
/// at all", and anything above the floor goes to the worker: spending ASR
/// time on noise costs a second, and refusing quiet speech costs the user
/// their words.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Speech { rms: f32, peak: f32 },
    TooQuiet { seconds: f64, rms: f32, peak: f32 },
}

/// Measure a drained capture.
///
/// The sum is accumulated in f64 for the same reason the worker does it: a
/// f32 mean over millions of squared samples loses enough precision to move
/// the decision.
pub fn verdict(samples: &[f32], sample_rate: u32) -> Verdict {
    let seconds = samples.len() as f64 / sample_rate as f64;
    let rms = root_mean_square(samples);
    let peak = loudest_window(samples, sample_rate);
    if seconds < MIN_SECONDS || rms < SILENCE_RMS {
        return Verdict::TooQuiet { seconds, rms, peak };
    }
    Verdict::Speech { rms, peak }
}

/// The verdict for a capture that arrives as two runs of samples, which is
/// how a ring buffer hands back something that wrapped.
///
/// Measured as one capture rather than as two, so the answer does not depend
/// on where in the ring the recording happened to start.
fn measure(first: &[f32], second: &[f32], sample_rate: u32) -> Verdict {
    if second.is_empty() {
        return verdict(first, sample_rate);
    }
    let mut joined = Vec::with_capacity(first.len() + second.len());
    joined.extend_from_slice(first);
    joined.extend_from_slice(second);
    verdict(&joined, sample_rate)
}

fn root_mean_square(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares: f64 = samples.iter().map(|sample| *sample as f64 * *sample as f64).sum();
    (sum_squares / samples.len() as f64).sqrt() as f32
}

/// The energy of the loudest short window in the capture.
///
/// Windows do not overlap, which can split a burst across two of them and
/// read each half. Speech lasts far longer than one window, so the strongest
/// window of a real dictation is well inside a syllable rather than on its
/// edge, and the extra bookkeeping of a sliding window buys nothing here.
///
/// A capture shorter than one window is measured whole, so the answer is
/// always about the same thing: the loudest part of what was recorded.
fn loudest_window(samples: &[f32], sample_rate: u32) -> f32 {
    let window = (WINDOW_SECONDS * sample_rate as f64).round() as usize;
    if window == 0 || samples.len() <= window {
        return root_mean_square(samples);
    }
    samples
        .chunks(window)
        .map(root_mean_square)
        .fold(0.0f32, f32::max)
}

/// What became of a finished capture.
pub enum Finished {
    /// Written to disk, ready for the worker.
    Recorded(RecordedAudio),
    /// Below the floor, so nothing was written at all.
    TooQuiet { seconds: f64, rms: f32, peak: f32 },
}

/// A finished recording that has not been drained or encoded yet.
pub struct CapturedAudio {
    samples: Arc<Mutex<Consumer<f32>>>,
    sample_rate: u32,
    /// The buffer filled before the user stopped speaking, so the end of the
    /// dictation was never captured. What is here is the beginning of it.
    pub truncated: bool,
    /// Carried on the capture rather than read from the microphone later,
    /// because the trace is written on the pipeline thread, which has the
    /// capture but not the device that produced it.
    device_name: String,
}

impl CapturedAudio {
    /// Which microphone these samples came from.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Measure the capture without consuming it.
    ///
    /// Taken on the UI thread the instant the hotkey comes up, because a
    /// press that held no speech must be answered there and then. Sending it
    /// to the pipeline thread to find out puts it behind whatever that thread
    /// is doing, and that thread spends most of its time inside a worker call
    /// for the previous dictation: one capture in this log waited seven
    /// seconds to be told it was empty.
    ///
    /// Reading the ring without committing is what keeps this free of
    /// consequence. A capture that turns out to be real is still whole
    /// afterwards, and is drained and written on the pipeline thread exactly
    /// as before.
    pub fn inspect(&self) -> Result<Verdict> {
        let mut samples = self
            .samples
            .lock()
            .map_err(|_| anyhow!("The audio buffer lock was poisoned"))?;
        let waiting = samples.slots();
        let Ok(chunk) = samples.read_chunk(waiting) else {
            return Ok(verdict(&[], self.sample_rate));
        };
        // The ring wraps, so what was recorded can arrive as two runs of
        // samples. Both are measured; neither is copied.
        let (first, second) = chunk.as_slices();
        Ok(measure(first, second, self.sample_rate))
    }

    /// Throw away a capture that held no speech.
    ///
    /// Left in the ring, its samples would be prepended to whatever the user
    /// says next: `start_recording` clears the buffer, but a capture is only
    /// ever cleared by being taken, and this one is never taken.
    pub fn discard(self) -> Result<()> {
        let mut samples = self
            .samples
            .lock()
            .map_err(|_| anyhow!("The audio buffer lock was poisoned"))?;
        let waiting = samples.slots();
        if let Ok(chunk) = samples.read_chunk(waiting) {
            chunk.commit_all();
        }
        Ok(())
    }

    /// Drain the capture buffer, and write it as the mono 16-bit WAV the
    /// inference worker accepts - unless there is nothing in it.
    ///
    /// Measured before anything is written, because the samples are already
    /// in hand here and everything after this point costs a file, an IPC
    /// round trip and a visible state change to answer a press that held no
    /// speech.
    pub fn finish(self, dir: &Path) -> Result<Finished> {
        let mut samples = self
            .samples
            .lock()
            .map_err(|_| anyhow!("The audio buffer lock was poisoned"))?;
        let mut mono = Vec::with_capacity(samples.slots());
        while let Ok(sample) = samples.pop() {
            mono.push(sample);
        }
        drop(samples);

        let duration = Duration::from_secs_f64(mono.len() as f64 / self.sample_rate as f64);
        let (measured, peak) = match verdict(&mono, self.sample_rate) {
            Verdict::TooQuiet { seconds, rms, peak } => {
                return Ok(Finished::TooQuiet { seconds, rms, peak })
            }
            Verdict::Speech { rms, peak } => (rms, peak),
        };
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!(
            "utterance-{}.wav",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
        ));
        write_wav(&path, &mono, self.sample_rate, 1)?;
        Ok(Finished::Recorded(RecordedAudio { path, duration, rms: measured, peak }))
    }
}

pub struct RecordedAudio {
    /// The energy of the capture that was accepted, as the mean that decided
    /// it and as the loudest window that may come to.
    ///
    /// Kept because a floor can only be set from the captures it let through,
    /// not only from the ones it turned away. Numbers, never anything the
    /// user said.
    pub rms: f32,
    pub peak: f32,
    pub path: PathBuf,
    pub duration: Duration,
}

/// This is called on cpal's real-time thread. It only performs arithmetic,
/// atomic stores, and wait-free SPSC pushes; it never locks or allocates.
fn push_mono<T>(
    input: &[T],
    producer: &mut Producer<f32>,
    channels: usize,
    to_f32: impl Fn(T) -> f32,
    level: &AtomicU32,
    overflowed: &AtomicBool,
    recorded: &RecordedLevel,
) where
    T: Copy,
{
    let mut sum_squares = 0.0f64;
    let mut frames = 0usize;
    for frame in input.chunks_exact(channels) {
        let sample = frame.iter().map(|value| to_f32(*value)).sum::<f32>() / channels as f32;
        sum_squares += sample as f64 * sample as f64;
        frames += 1;
        if producer.push(sample).is_err() {
            overflowed.store(true, Ordering::Relaxed);
        }
    }
    if frames != 0 {
        level.store(
            (((sum_squares / frames as f64).sqrt() as f32) * 8.0)
                .clamp(0.0, 1.0)
                .to_bits(),
            Ordering::Relaxed,
        );
        // Folded in as it arrives rather than measured at the end, because the
        // point of this number is to be available while the key is still held.
        recorded.accumulate(sum_squares, frames);
    }
}

fn write_wav(path: &Path, samples: &[f32], sample_rate: u32, channels: u16) -> Result<()> {
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Samples at a steady amplitude, which is all the energy gate measures.
    fn tone(seconds: f64, amplitude: f32, rate: u32) -> Vec<f32> {
        let frames = (seconds * rate as f64) as usize;
        (0..frames)
            .map(|n| amplitude * (n as f32 * 0.2).sin())
            .collect()
    }

    /// The case that prompted this: tap the key and let go. There is nothing
    /// there to transcribe, and finding that out used to cost a file, an IPC
    /// round trip and a flash of "Transcribing".
    #[test]
    fn a_tap_of_the_key_is_answered_without_looking_at_the_audio_further() {
        let samples = tone(0.08, 0.4, 16_000);
        assert!(matches!(verdict(&samples, 16_000), Verdict::TooQuiet { .. }));
    }

    /// Short and loud is still short. A fragment below the floor is not a
    /// dictation, however much energy it carries.
    #[test]
    fn a_burst_of_noise_too_short_to_be_speech_is_refused() {
        let samples = tone(0.2, 0.9, 16_000);
        match verdict(&samples, 16_000) {
            Verdict::TooQuiet { seconds, .. } => assert!(seconds < MIN_SECONDS),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Long enough, and silent. Whisper answers silence with an invented
    /// sentence, so this must never reach it.
    #[test]
    fn a_long_but_silent_capture_is_refused() {
        let samples = vec![0.0f32; 16_000 * 3];
        match verdict(&samples, 16_000) {
            Verdict::TooQuiet { rms, .. } => assert!(rms < SILENCE_RMS),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// The failure that matters most. Rejecting quiet speech loses words the
    /// user actually said, which is worse than spending ASR time on noise, so
    /// anything above the floor goes on to the worker.
    #[test]
    fn quiet_speech_just_above_the_floor_is_still_a_dictation() {
        let samples = tone(1.0, SILENCE_RMS * 4.0, 16_000);
        assert!(matches!(verdict(&samples, 16_000), Verdict::Speech { .. }));
    }

    #[test]
    fn ordinary_speech_is_unaffected() {
        let samples = tone(2.0, 0.3, 16_000);
        assert!(matches!(verdict(&samples, 16_000), Verdict::Speech { .. }));
    }

    /// The whole point of measuring here: a press with nothing in it must
    /// cost nothing. No file on disk means no WAV to clean up, and it is also
    /// what proves the worker was never involved, since the worker is only
    /// ever handed a path.
    #[test]
    fn a_refused_capture_writes_no_file_at_all() {
        let dir = std::env::temp_dir().join(format!("privacyflow-quiet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let silence = vec![0.0f32; 16_000];
        let (mut producer, consumer) = RingBuffer::new(silence.len());
        for sample in &silence {
            producer.push(*sample).unwrap();
        }
        let captured = CapturedAudio {
            samples: Arc::new(Mutex::new(consumer)),
            sample_rate: 16_000,
            truncated: false,
            device_name: "test microphone".to_owned(),
        };

        let finished = captured.finish(&dir).unwrap();
        assert!(matches!(finished, Finished::TooQuiet { .. }));
        assert!(!dir.exists(), "a refused capture created the audio directory");
    }

    fn captured(samples: &[f32], rate: u32) -> CapturedAudio {
        let (mut producer, consumer) = RingBuffer::new(samples.len().max(1));
        for sample in samples {
            producer.push(*sample).unwrap();
        }
        CapturedAudio {
            samples: Arc::new(Mutex::new(consumer)),
            sample_rate: rate,
            truncated: false,
            device_name: "test microphone".to_owned(),
        }
    }

    /// The verdict has to exist the moment the key comes up, so it is taken
    /// without consuming the capture: a dictation that turns out to be real
    /// must still be there to be written afterwards.
    #[test]
    fn inspecting_a_capture_leaves_every_sample_in_place() {
        let speech = tone(1.0, 0.3, 16_000);
        let capture = captured(&speech, 16_000);

        assert!(matches!(capture.inspect().unwrap(), Verdict::Speech { .. }));
        assert!(matches!(capture.inspect().unwrap(), Verdict::Speech { .. }));

        let dir = std::env::temp_dir().join(format!("privacyflow-peek-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        match capture.finish(&dir).unwrap() {
            Finished::Recorded(audio) => {
                let reader = hound::WavReader::open(&audio.path).unwrap();
                assert_eq!(
                    reader.len() as usize,
                    speech.len(),
                    "inspecting the capture ate part of the dictation"
                );
            }
            Finished::TooQuiet { .. } => panic!("a real dictation was refused"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A refused capture is thrown away where it lies. Left in the ring, its
    /// samples would be prepended to whatever the user says next.
    #[test]
    fn discarding_a_refused_capture_empties_the_buffer() {
        let capture = captured(&vec![0.0f32; 16_000], 16_000);
        assert!(matches!(capture.inspect().unwrap(), Verdict::TooQuiet { .. }));
        let buffer = capture.samples.clone();
        capture.discard().unwrap();
        assert_eq!(buffer.lock().unwrap().slots(), 0);
    }

    /// The floor is one policy applied in two programs, and the app's copy is
    /// only a fast answer if it agrees with the worker's. They are compared
    /// against the worker's own source rather than against a number repeated
    /// in a comment here, which would drift without anything noticing.
    #[test]
    fn the_app_and_the_worker_hold_the_same_floor() {
        let Ok(root) = crate::router::research_root() else {
            eprintln!("skipped: the research repository is not beside this one");
            return;
        };
        let source = root.join("src").join("dictation_router").join("asr.py");
        let Ok(text) = std::fs::read_to_string(&source) else {
            eprintln!("skipped: {} is not readable", source.display());
            return;
        };
        let constant = |name: &str| -> f64 {
            text.lines()
                .find_map(|line| line.strip_prefix(&format!("{name} = ")))
                .unwrap_or_else(|| panic!("{name} is no longer defined in asr.py"))
                .trim()
                .parse()
                .expect("the worker's floor must be a number")
        };
        assert_eq!(constant("SILENCE_RMS") as f32, SILENCE_RMS);
        assert_eq!(constant("MIN_SECONDS"), MIN_SECONDS);
    }

    #[test]
    fn a_short_press_is_never_called_quiet() {
        // It would be over before the user could read the toast, let alone
        // lean in and answer it.
        assert!(!heading_for_refusal(Duration::from_millis(500), 0.0));
    }

    #[test]
    fn holding_a_press_that_is_heading_for_refusal_is_worth_saying() {
        assert!(heading_for_refusal(QUIET_WARNING_AFTER + Duration::from_millis(1), 0.0009));
    }

    #[test]
    fn a_press_that_will_be_accepted_is_left_alone() {
        assert!(!heading_for_refusal(Duration::from_secs(10), SILENCE_RMS * 2.0));
    }

    /// The floor refuses what is below it, so sitting exactly on it survives
    /// and must not be warned about. The warning and the refusal have to read
    /// the boundary the same way or they will disagree about the same capture.
    #[test]
    fn sitting_exactly_on_the_floor_is_not_a_refusal() {
        assert!(!heading_for_refusal(Duration::from_secs(10), SILENCE_RMS));
    }

    /// The live measurement and the verdict have to be the same number, or a
    /// warning would be about a capture other than the one being judged. This
    /// is what makes the warning a prediction rather than an estimate.
    #[test]
    fn the_running_measurement_matches_the_verdict_it_predicts() {
        let samples: Vec<f32> = (0..4_000)
            .map(|index| (index as f32 * 0.01).sin() * 0.002)
            .collect();
        let (mut producer, _consumer) = RingBuffer::new(samples.len());
        let level = AtomicU32::new(0.0f32.to_bits());
        let overflowed = AtomicBool::new(false);
        let recorded = RecordedLevel::default();
        // Delivered in buffers, the way a device delivers it.
        for buffer in samples.chunks(128) {
            push_mono(buffer, &mut producer, 1, |sample| sample, &level, &overflowed, &recorded);
        }
        let expected = root_mean_square(&samples);
        let measured = recorded.rms();
        assert!(
            (measured - expected).abs() < 1e-6,
            "running rms {measured} should match the verdict's {expected}"
        );
    }

    #[test]
    fn callback_downmixes_without_a_mutex() {
        let (mut producer, mut consumer) = RingBuffer::new(4);
        let level = AtomicU32::new(0.0f32.to_bits());
        let overflowed = AtomicBool::new(false);
        push_mono(
            &[1.0f32, -1.0, 0.5, 0.5],
            &mut producer,
            2,
            |sample| sample,
            &level,
            &overflowed,
            &RecordedLevel::default(),
        );
        assert_eq!(consumer.pop().unwrap(), 0.0);
        assert_eq!(consumer.pop().unwrap(), 0.5);
        assert!(!overflowed.load(Ordering::Relaxed));
    }

    /// A device that cannot serve the small buffer we want must be asked for
    /// one it can, rather than for a size it already said it rejects.
    #[test]
    fn the_requested_buffer_stays_inside_what_the_device_supports() {
        let clamped = requested_buffer_size(&SupportedBufferSize::Range {
            min: 512,
            max: 4096,
        });
        assert!(matches!(clamped, BufferSize::Fixed(512)));

        let ours = requested_buffer_size(&SupportedBufferSize::Range { min: 8, max: 4096 });
        assert!(matches!(ours, BufferSize::Fixed(TARGET_BUFFER_FRAMES)));

        let unknown = requested_buffer_size(&SupportedBufferSize::Unknown);
        assert!(matches!(unknown, BufferSize::Default));
    }
}
