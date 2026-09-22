/// How a spring moves: how quickly it swings and how much it is held back.
///
/// Frequency is in cycles per second of the undamped swing. A damping ratio
/// below one overshoots its target a little before settling, which is what
/// makes growth read as the capsule popping open; at one it arrives without
/// overshooting, which is how something tidying itself away should move.
#[derive(Debug, Clone, Copy)]
pub struct Feel {
    pub frequency: f32,
    pub damping: f32,
}

/// Opening: quick, with a few percent of overshoot.
pub const GROW: Feel = Feel { frequency: 3.4, damping: 0.72 };

/// Closing: a little slower and without overshoot, so it settles rather than
/// bounces on its way back to rest.
pub const SHRINK: Feel = Feel { frequency: 2.6, damping: 1.0 };

/// Integration is done in small fixed steps, whatever the frame rate, so a
/// slow frame changes how far the spring travels, never whether it is stable.
const STEP_SECONDS: f32 = 1.0 / 240.0;

/// Close enough to the target, and slow enough, to call the motion over.
/// Below a hundredth of a point nothing on screen changes.
const SETTLED_DISTANCE: f32 = 0.01;
const SETTLED_SPEED: f32 = 0.1;

/// One value eased towards a target by a damped spring.
///
/// A spring rather than a fixed-duration tween because it carries velocity:
/// a capsule told to shrink halfway through growing turns around smoothly
/// instead of restarting a curve from standstill, and linear tweens are the
/// main reason interface motion reads as mechanical.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    value: f32,
    velocity: f32,
}

impl Spring {
    pub fn new(value: f32) -> Self {
        Self { value, velocity: 0.0 }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Advances the spring by `seconds` towards `target`, and returns whether
    /// it is still moving, which is whether the caller needs another frame.
    pub fn step(&mut self, target: f32, seconds: f32, feel: Feel) -> bool {
        let angular = std::f32::consts::TAU * feel.frequency;
        let mut remaining = seconds;
        while remaining > 0.0 {
            let step = remaining.min(STEP_SECONDS);
            let acceleration = -angular * angular * (self.value - target)
                - 2.0 * feel.damping * angular * self.velocity;
            self.velocity += acceleration * step;
            self.value += self.velocity * step;
            remaining -= step;
        }
        let settled = (self.value - target).abs() < SETTLED_DISTANCE
            && self.velocity.abs() < SETTLED_SPEED;
        if settled {
            self.value = target;
            self.velocity = 0.0;
        }
        !settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a spring at 60 frames a second until it stops, returning the
    /// largest value it reached and when it last moved more than a point
    /// away from its target, which is when the eye sees it arrive.
    fn run(from: f32, to: f32, feel: Feel) -> (f32, f32) {
        let mut spring = Spring::new(from);
        let mut peak = from;
        let mut elapsed = 0.0;
        let mut arrived = 0.0;
        while spring.step(to, 1.0 / 60.0, feel) {
            peak = peak.max(spring.value());
            elapsed += 1.0 / 60.0;
            if (spring.value() - to).abs() > 1.0 {
                arrived = elapsed;
            }
            assert!(elapsed < 2.0, "the spring never settled");
        }
        (peak.max(spring.value()), arrived)
    }

    /// The whole point of the two feels: opening pops past its target by a
    /// small amount, and neither of them takes long enough to feel sluggish.
    #[test]
    fn growing_overshoots_slightly_and_settles_quickly() {
        let (peak, elapsed) = run(88.0, 216.0, GROW);
        let overshoot = (peak - 216.0) / (216.0 - 88.0);
        assert!(overshoot > 0.01 && overshoot < 0.08, "overshoot was {overshoot}");
        assert!(elapsed < 0.35, "took {elapsed}s to arrive");
    }

    /// Shrinking back must never dip below where it is going: a capsule that
    /// undershoots the bead reads as flinching.
    #[test]
    fn shrinking_never_passes_its_target() {
        let mut spring = Spring::new(216.0);
        while spring.step(72.0, 1.0 / 60.0, SHRINK) {
            assert!(spring.value() >= 72.0, "dipped to {}", spring.value());
        }
        assert_eq!(spring.value(), 72.0);
    }

    /// A long stall between frames, such as the first frame after the app
    /// has been idle, must not throw the spring off.
    #[test]
    fn a_long_frame_does_not_destabilise_it() {
        let mut spring = Spring::new(72.0);
        spring.step(216.0, 0.5, GROW);
        assert!(spring.value() > 150.0 && spring.value() < 240.0, "{}", spring.value());
    }
}
