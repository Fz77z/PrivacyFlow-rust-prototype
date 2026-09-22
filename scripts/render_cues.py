#!/usr/bin/env python3
"""Render the two push-to-talk cue sounds into assets/sounds/.

The cues are kept as a script rather than as two opaque binaries so their
character can be retuned by editing numbers here and running this again. The
application only ever reads the rendered files, so nothing in the build
depends on Python being present.

Both cues are the same 110 ms shape: a sine gliding between two pitches a
perfect fourth apart, with a quiet second harmonic for body, under an
envelope that starts and ends at exactly zero so neither end clicks. Press
rises, release falls, which is the only thing distinguishing them by ear.

Usage: python3 scripts/render_cues.py
"""

import array
import math
import os
import wave

SAMPLE_RATE = 48_000
DURATION_SECONDS = 0.110

# A perfect fourth. The interval is what makes the pair read as a matched
# question and answer rather than as two unrelated beeps.
LOW_HZ = 329.63
HIGH_HZ = 440.0

# Enough of the octave above to give the tone a body, far too little to be
# heard as a separate pitch.
HARMONIC_LEVEL = 0.12

ATTACK_SECONDS = 0.008
RELEASE_SECONDS = 0.004
# How far the body of the sound has decayed by the time it ends.
DECAY_FLOOR = 0.02

# Peak amplitude, about -21 dBFS. Chosen to sit under speech rather than over
# it: the microphone is live while the press cue plays.
PEAK = 0.085


def envelope(position):
    """The amplitude at a position through the cue, from 0.0 to 1.0.

    A raised cosine opens and closes the sound, and an exponential decay
    between them gives it the fast falloff of something struck rather than
    the flat body of a beep.
    """
    elapsed = position * DURATION_SECONDS
    remaining = DURATION_SECONDS - elapsed

    body = DECAY_FLOOR ** position
    if elapsed < ATTACK_SECONDS:
        body *= 0.5 - 0.5 * math.cos(math.pi * elapsed / ATTACK_SECONDS)
    if remaining < RELEASE_SECONDS:
        body *= 0.5 - 0.5 * math.cos(math.pi * remaining / RELEASE_SECONDS)
    return body


def render(start_hz, end_hz):
    """Render one cue gliding from one pitch to the other, as f32 samples.

    The glide is exponential in frequency, which is how pitch is heard, and
    the phase is integrated rather than computed per sample so the waveform
    stays continuous across the glide.
    """
    frames = int(SAMPLE_RATE * DURATION_SECONDS)
    samples = []
    phase = 0.0
    for index in range(frames):
        position = index / frames
        frequency = start_hz * (end_hz / start_hz) ** position
        value = math.sin(phase) + HARMONIC_LEVEL * math.sin(2.0 * phase)
        samples.append(value * envelope(position))
        phase += 2.0 * math.pi * frequency / SAMPLE_RATE

    # Normalising after the fact, because the harmonic and the envelope make
    # the true peak hard to predict and the level is the whole point.
    loudest = max(abs(sample) for sample in samples)
    return [sample / loudest * PEAK for sample in samples]


def write(path, samples):
    """Write mono 16-bit PCM, the format the application decodes."""
    encoded = array.array(
        "h", [int(round(max(-1.0, min(1.0, sample)) * 32767.0)) for sample in samples]
    )
    with wave.open(path, "wb") as out:
        out.setnchannels(1)
        out.setsampwidth(2)
        out.setframerate(SAMPLE_RATE)
        out.writeframes(encoded.tobytes())
    print(f"wrote {path} ({len(samples)} frames)")


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    directory = os.path.join(root, "assets", "sounds")
    os.makedirs(directory, exist_ok=True)
    write(os.path.join(directory, "press.wav"), render(LOW_HZ, HIGH_HZ))
    write(os.path.join(directory, "release.wav"), render(HIGH_HZ, LOW_HZ))


if __name__ == "__main__":
    main()
