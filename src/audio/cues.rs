//! The two short sounds that confirm a push-to-talk press and release.
//!
//! Shaped deliberately unlike `Microphone`, which pauses its stream between
//! dictations. Restarting a paused stream costs tens of milliseconds on this
//! machine, and a press cue that arrives that late stops reading as feedback
//! for the keypress. This stream therefore runs for the lifetime of the
//! application, writing silence whenever no cue is playing. A microphone left
//! live has a privacy cost that made pausing worth the latency; an output
//! stream has none.
//!
//! The sounds themselves are rendered by `scripts/render_cues.py` and
//! embedded in the binary, so an installed bundle cannot lose one.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const PRESS_WAV: &[u8] = include_bytes!("../../assets/sounds/press.wav");
const RELEASE_WAV: &[u8] = include_bytes!("../../assets/sounds/release.wav");

/// Which sound to play. The numbers are the values carried through the atomic
/// the audio callback reads, so they are part of the wire format between the
/// UI thread and the callback rather than incidental.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Press = 1,
    Release = 2,
}

const NOTHING_REQUESTED: u8 = 0;

/// An output device opened once and then held open, able to play either cue.
///
/// Requests are a single atomic store from the UI thread, which the callback
/// swaps out. That keeps the real-time path free of locks and allocation, in
/// keeping with the capture side, and it means a press arriving while the
/// previous cue is still sounding simply restarts it.
pub struct Cues {
    /// Never read. Dropping it stops the stream, so it has to outlive the
    /// requests that feed it.
    _stream: cpal::Stream,
    requested: Arc<AtomicU8>,
}

impl Cues {
    /// Open the default output device and start the silent stream.
    ///
    /// Returns an error rather than degrading quietly: a machine with no
    /// usable output is a fact the user should be told, not one to discover
    /// by noticing nothing ever beeps.
    pub fn open() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("No audio output device found"))?;
        let supported = device
            .default_output_config()
            .context("Could not read default output config")?;
        let sample_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        if channels == 0 {
            return Err(anyhow!("Output device reported zero channels"));
        }

        // Resampled once, here, so the callback only ever copies. A 44.1 kHz
        // device playing these 48 kHz assets untouched would sound about a
        // tone and a half sharp.
        let press = resample(&decode(PRESS_WAV).context("press cue")?, sample_rate);
        let release = resample(&decode(RELEASE_WAV).context("release cue")?, sample_rate);

        let requested = Arc::new(AtomicU8::new(NOTHING_REQUESTED));
        let callback_requested = requested.clone();
        let mut playing: Option<(Arc<[f32]>, usize)> = None;
        let config: cpal::StreamConfig = supported.clone().into();
        let err_fn = |error| eprintln!("LocalFlow cue stream error: {error}");

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |out: &mut [f32], _| {
                    fill(out, channels, &callback_requested, &press, &release, &mut playing, |s| s)
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |out: &mut [i16], _| {
                    fill(out, channels, &callback_requested, &press, &release, &mut playing, |s| {
                        (s * i16::MAX as f32) as i16
                    })
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config,
                move |out: &mut [u16], _| {
                    fill(out, channels, &callback_requested, &press, &release, &mut playing, |s| {
                        (((s + 1.0) * 0.5) * u16::MAX as f32) as u16
                    })
                },
                err_fn,
                None,
            )?,
            other => return Err(anyhow!("Unsupported output sample format: {other:?}")),
        };

        // cpal's CoreAudio backend already started the unit inside
        // `build_output_stream`, but saying so explicitly keeps this correct
        // on a backend that does not.
        stream.play().context("Could not start the cue stream")?;

        Ok(Self { _stream: stream, requested })
    }

    /// Ask for a cue. Returns immediately; the sound starts on the next
    /// output callback, within one buffer period.
    pub fn play(&self, cue: Cue) {
        self.requested.store(cue as u8, Ordering::Relaxed);
    }
}

/// Fill one output buffer, starting a newly requested cue and mixing whatever
/// is playing across every channel.
///
/// Runs on the real-time thread, so it allocates nothing and takes no locks.
/// The sample conversion is passed in because the only thing that differs
/// between the supported output formats is that last step.
fn fill<T: cpal::Sample>(
    out: &mut [T],
    channels: usize,
    requested: &AtomicU8,
    press: &Arc<[f32]>,
    release: &Arc<[f32]>,
    playing: &mut Option<(Arc<[f32]>, usize)>,
    convert: impl Fn(f32) -> T,
) {
    // Taking the request clears it, so one store plays one cue. A press that
    // lands mid-cue restarts from the top rather than being layered over
    // itself, which is what a user pressing twice in a second expects.
    match requested.swap(NOTHING_REQUESTED, Ordering::Relaxed) {
        value if value == Cue::Press as u8 => *playing = Some((press.clone(), 0)),
        value if value == Cue::Release as u8 => *playing = Some((release.clone(), 0)),
        _ => {}
    }

    for frame in out.chunks_mut(channels.max(1)) {
        let sample = match playing {
            Some((cue, cursor)) => {
                let sample = cue.get(*cursor).copied().unwrap_or(0.0);
                *cursor += 1;
                if *cursor >= cue.len() {
                    *playing = None;
                }
                sample
            }
            None => 0.0,
        };
        for slot in frame.iter_mut() {
            *slot = convert(sample);
        }
    }
}

/// Decode one embedded cue into mono f32 samples paired with its rate.
///
/// The assets are written by `scripts/render_cues.py` and are always mono
/// 16-bit, so anything else means the asset and this decoder have drifted
/// apart, which is worth an error rather than a guess.
fn decode(bytes: &[u8]) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::new(std::io::Cursor::new(bytes))
        .context("Could not read the embedded cue")?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(anyhow!(
            "Cue asset is {} channel {}-bit; expected mono 16-bit",
            spec.channels,
            spec.bits_per_sample
        ));
    }
    let samples = reader
        .samples::<i16>()
        .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
        .collect::<Result<Vec<f32>, _>>()
        .context("Could not decode the embedded cue")?;
    Ok((samples, spec.sample_rate))
}

/// Resample a decoded cue to the output device's rate.
///
/// Linear interpolation, which is well short of what a resampler owes a piece
/// of music and entirely sufficient for a 110 ms tone that is about to be
/// played at a twentieth of full scale.
fn resample((samples, source_rate): &(Vec<f32>, u32), target_rate: u32) -> Arc<[f32]> {
    if *source_rate == target_rate || samples.is_empty() {
        return samples.as_slice().into();
    }
    let ratio = *source_rate as f64 / target_rate as f64;
    let frames = ((samples.len() as f64) / ratio).round() as usize;
    let mut resampled = Vec::with_capacity(frames);
    for index in 0..frames {
        let position = index as f64 * ratio;
        let left = position.floor() as usize;
        let fraction = (position - left as f64) as f32;
        let before = samples.get(left).copied().unwrap_or(0.0);
        let after = samples.get(left + 1).copied().unwrap_or(before);
        resampled.push(before + (after - before) * fraction);
    }
    resampled.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The assets and this decoder have to agree. If someone reruns the
    /// render script with a different format, this is what says so, rather
    /// than a silent absence of sound on the user's machine.
    #[test]
    fn both_cues_decode_as_mono_at_the_rendered_rate() {
        for bytes in [PRESS_WAV, RELEASE_WAV] {
            let (samples, rate) = decode(bytes).expect("cue must decode");
            assert_eq!(rate, 48_000);
            assert!(!samples.is_empty());
        }
    }

    /// The cues play while the microphone is live and while the user is doing
    /// something else. A render script edit that pushed them towards full
    /// scale would turn a confirmation into an interruption.
    #[test]
    fn neither_cue_is_loud() {
        for bytes in [PRESS_WAV, RELEASE_WAV] {
            let (samples, _) = decode(bytes).unwrap();
            let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
            assert!(peak > 0.0, "a silent cue is not a cue");
            assert!(peak < 0.15, "cue peaks at {peak}, which is too loud to sit under speech");
        }
    }

    /// Halving the rate halves the length and keeps the shape. Getting this
    /// wrong is inaudible in a test and obvious on a 44.1 kHz device, where
    /// the cue would come out at the wrong pitch.
    #[test]
    fn resampling_scales_length_and_preserves_the_ramp() {
        let ramp: Vec<f32> = (0..100).map(|index| index as f32 / 99.0).collect();
        let halved = resample(&(ramp.clone(), 48_000), 24_000);
        assert_eq!(halved.len(), 50);
        assert!((halved[0] - 0.0).abs() < 1e-6);
        assert!((halved[25] - 0.505).abs() < 0.02, "midpoint was {}", halved[25]);
    }

    #[test]
    fn resampling_to_the_same_rate_changes_nothing() {
        let ramp: Vec<f32> = (0..10).map(|index| index as f32).collect();
        assert_eq!(&*resample(&(ramp.clone(), 48_000), 48_000), &ramp[..]);
    }
}
