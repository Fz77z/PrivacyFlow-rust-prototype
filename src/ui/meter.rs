/// How loud the microphone has to be before the bars move at all, and how loud
/// fills them, in decibels relative to the level the audio thread reports.
///
/// The reported level is the raw RMS times eight, clamped to one, so this
/// window is roughly -54 to -30 dBFS. That is set from this machine's own
/// microphone, which is quiet: silent captures measured about -56 dBFS and
/// whole dictations about -54 dBFS including their pauses (see
/// docs/investigations/2026-09-23-silence-floor-false-refusals.md). A window
/// placed for a typical microphone, starting at -48 dBFS, left speech here
/// barely moving the bars. Hearing is logarithmic, so the window is too.
const QUIET_DB: f32 = -36.0;
const LOUD_DB: f32 = -12.0;

/// How fast the meter rises to meet a louder sound and falls away after it,
/// as time constants in seconds. Rising fast and falling slowly is how every
/// good level meter behaves: syllables land immediately, and the gaps between
/// them do not flicker.
const ATTACK_SECONDS: f32 = 0.025;
const RELEASE_SECONDS: f32 = 0.16;

/// How far each bar lags the voice, in seconds, and how much of the voice it
/// shows. The lags differ so a syllable ripples across the mark instead of
/// lifting all four bars together, which is what made the old meter read as
/// a single block nudging up and down. Nothing here is invented motion: each
/// bar is the same measured voice, only later and slightly smaller.
const BAR_LAG_SECONDS: [f32; 4] = [0.09, 0.0, 0.045, 0.13];
const BAR_WEIGHT: [f32; 4] = [0.78, 1.0, 0.9, 0.7];

/// Turns the microphone's level into how far each of the mark's four bars
/// should stand while the user is speaking, from 0 (silent) to 1 (loud).
#[derive(Debug, Clone, Default)]
pub struct VoiceMeter {
    envelope: f32,
    bars: [f32; 4],
}

impl VoiceMeter {
    /// Forgets the last dictation, so the next one starts from silence rather
    /// than from wherever the previous voice left off.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Takes one reading of the level, `seconds` after the last one, and
    /// returns the bars to paint.
    pub fn update(&mut self, level: f32, seconds: f32) -> [f32; 4] {
        let loudness = loudness(level);
        let time_constant =
            if loudness > self.envelope { ATTACK_SECONDS } else { RELEASE_SECONDS };
        self.envelope = follow(self.envelope, loudness, seconds, time_constant);
        for (index, bar) in self.bars.iter_mut().enumerate() {
            let lagged = follow(*bar, self.envelope, seconds, BAR_LAG_SECONDS[index]);
            *bar = lagged;
        }
        self.bars()
    }

    pub fn bars(&self) -> [f32; 4] {
        std::array::from_fn(|index| (self.bars[index] * BAR_WEIGHT[index]).clamp(0.0, 1.0))
    }
}

/// Where a level sits in the window the bars use, from 0 to 1.
fn loudness(level: f32) -> f32 {
    if level <= 0.0 {
        return 0.0;
    }
    let decibels = 20.0 * level.log10();
    ((decibels - QUIET_DB) / (LOUD_DB - QUIET_DB)).clamp(0.0, 1.0)
}

/// Moves `current` towards `target` as a one-pole filter with the given time
/// constant. A time constant of zero follows the target exactly.
fn follow(current: f32, target: f32, seconds: f32, time_constant: f32) -> f32 {
    if time_constant <= 0.0 {
        return target;
    }
    let keep = (-seconds / time_constant).exp();
    target + (current - target) * keep
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the meter at 60 frames a second on one steady level.
    fn hold(meter: &mut VoiceMeter, level: f32, seconds: f32) -> [f32; 4] {
        let mut bars = meter.bars();
        for _ in 0..(seconds * 60.0) as usize {
            bars = meter.update(level, 1.0 / 60.0);
        }
        bars
    }

    /// Speech on this machine's quiet microphone must move the bars a lot,
    /// and its silent floor (about -56 dBFS, a level near 0.013) must leave
    /// them still. The speech level, -44 dBFS or a level near 0.05, is an
    /// estimate: only whole-dictation averages were measured, at about -54
    /// dBFS, and their pauses pull that well below the level while talking.
    #[test]
    fn quiet_speech_fills_the_bars_and_room_noise_does_not() {
        let mut meter = VoiceMeter::default();
        let room = hold(&mut meter, 0.0126, 0.5);
        assert!(room.iter().all(|bar| *bar == 0.0), "room noise moved the bars: {room:?}");
        let speech = hold(&mut meter, 0.05, 0.5);
        assert!(speech[1] > 0.4, "speech barely moved the bars: {speech:?}");
    }

    /// The ripple: just after the voice starts, the leading bar is ahead of
    /// the lagging ones, rather than all four moving as one block.
    #[test]
    fn a_syllable_reaches_the_bars_at_different_times() {
        let mut meter = VoiceMeter::default();
        let bars = hold(&mut meter, 0.5, 0.1);
        assert!(bars[1] > bars[3] + 0.1, "the bars moved together: {bars:?}");
    }

    /// Rising fast and falling slow: a gap between words should not drop the
    /// bars as quickly as a word raised them.
    #[test]
    fn the_bars_fall_more_slowly_than_they_rise() {
        let mut meter = VoiceMeter::default();
        let risen = hold(&mut meter, 0.5, 0.05)[1];
        hold(&mut meter, 0.5, 0.5);
        let peak = meter.bars()[1];
        let fallen = peak - hold(&mut meter, 0.0, 0.05)[1];
        assert!(risen > fallen, "rose {risen} in 50ms but fell {fallen}");
    }
}
