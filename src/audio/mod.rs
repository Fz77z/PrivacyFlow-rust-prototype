use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer, RingBuffer};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const MAX_RECORDING_SECONDS: usize = 120;

pub struct Recorder {
    stream: cpal::Stream,
    samples: Consumer<f32>,
    sample_rate: u32,
    level: Arc<AtomicU32>,
    overflowed: Arc<AtomicBool>,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("No input microphone found"))?;
        let config = device
            .default_input_config()
            .context("Could not read default microphone config")?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        if channels == 0 {
            return Err(anyhow!("Microphone reported zero input channels"));
        }
        // The callback downmixes to mono, so this is a fixed, preallocated
        // upper bound of two minutes rather than an unbounded Vec.
        let capacity = (sample_rate as usize)
            .checked_mul(MAX_RECORDING_SECONDS)
            .ok_or_else(|| anyhow!("Microphone sample rate is too large"))?;
        let (mut producer, samples) = RingBuffer::new(capacity);
        let level = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let callback_level = level.clone();
        let overflowed = Arc::new(AtomicBool::new(false));
        let callback_overflowed = overflowed.clone();
        let err_fn = |err| eprintln!("LocalFlow audio stream error: {err}");

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
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
                &config.into(),
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
                &config.into(),
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
        stream.play().context("Could not start microphone stream")?;
        Ok(Self {
            stream,
            samples,
            sample_rate,
            level,
            overflowed,
        })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn stop_to_wav(self, dir: &Path) -> Result<RecordedAudio> {
        let Self {
            stream,
            mut samples,
            sample_rate,
            overflowed,
            ..
        } = self;
        // Only consume the SPSC buffer after the audio callback has stopped.
        drop(stream);
        if overflowed.load(Ordering::Relaxed) {
            return Err(anyhow!(
                "Recording exceeded the two-minute local audio buffer; no text was sent"
            ));
        }
        let mut mono = Vec::with_capacity(samples.slots());
        while let Ok(sample) = samples.pop() {
            mono.push(sample);
        }
        let duration = Duration::from_secs_f64(mono.len() as f64 / sample_rate as f64);
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!(
            "utterance-{}.wav",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
        ));
        write_wav(&path, &mono, sample_rate, 1)?;
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
}
