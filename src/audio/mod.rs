use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SupportedBufferSize};
use rtrb::{Consumer, Producer, RingBuffer};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_RECORDING_SECONDS: usize = 120;

/// Requested capture buffer, in frames.
///
/// This is the dominant cost of restarting a paused stream: the smaller the
/// buffer, the sooner the first callback arrives. Measured on the development
/// machine, the device default cost about 57 ms from `play()` to the first
/// sample, 128 frames about 33 ms, and 64 frames about 29 ms. 128 is taken
/// over 64 because it doubles the time the callback has to run for almost the
/// same gain.
const TARGET_BUFFER_FRAMES: u32 = 128;

/// A microphone opened once and then kept open, paused, for the lifetime of
/// the application.
///
/// Opening the device and building the stream costs over a hundred
/// milliseconds, and doing that when the hotkey is pressed spends it out of
/// the first moments of speech. Paying it once at startup means a keypress
/// only has to restart an already-built stream. The stream is paused whenever
/// LocalFlow is not recording, so the microphone is not live between
/// dictations.
pub struct Microphone {
    stream: cpal::Stream,
    /// Only the pipeline thread ever locks this. The audio callback holds the
    /// producer and never touches the lock, so the real-time path stays
    /// wait-free.
    samples: Arc<Mutex<Consumer<f32>>>,
    sample_rate: u32,
    level: Arc<AtomicU32>,
    overflowed: Arc<AtomicBool>,
}

impl Microphone {
    pub fn open() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("No input microphone found"))?;
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
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = overflowed.clone();
        let err_fn = |err| eprintln!("LocalFlow audio stream error: {err}");

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
                    )
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("Unsupported microphone sample format: {other:?}")),
        };

        Ok(Self {
            stream,
            samples: Arc::new(Mutex::new(consumer)),
            sample_rate,
            level,
            overflowed,
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
        self.stream
            .pause()
            .context("Could not pause microphone stream")?;
        self.level.store(0.0f32.to_bits(), Ordering::Relaxed);
        if self.overflowed.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "Recording exceeded the two-minute local audio buffer; no text was sent"
            ));
        }
        Ok(CapturedAudio {
            samples: self.samples.clone(),
            sample_rate: self.sample_rate,
        })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
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

/// A finished recording that has not been drained or encoded yet.
pub struct CapturedAudio {
    samples: Arc<Mutex<Consumer<f32>>>,
    sample_rate: u32,
}

impl CapturedAudio {
    /// Drain the capture buffer into a mono 16-bit WAV, which is the only
    /// format the inference worker accepts.
    pub fn write_wav(self, dir: &Path) -> Result<RecordedAudio> {
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
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!(
            "utterance-{}.wav",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
        ));
        write_wav(&path, &mono, self.sample_rate, 1)?;
        Ok(RecordedAudio { path, duration })
    }
}

pub struct RecordedAudio {
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
) where
    T: Copy,
{
    let mut sum_squares = 0.0;
    let mut frames = 0usize;
    for frame in input.chunks_exact(channels) {
        let sample = frame.iter().map(|value| to_f32(*value)).sum::<f32>() / channels as f32;
        sum_squares += sample * sample;
        frames += 1;
        if producer.push(sample).is_err() {
            overflowed.store(true, Ordering::Relaxed);
        }
    }
    if frames != 0 {
        level.store(
            ((sum_squares / frames as f32).sqrt() * 8.0)
                .clamp(0.0, 1.0)
                .to_bits(),
            Ordering::Relaxed,
        );
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
