//! Remember where the user put the capsule.
//!
//! eframe can persist window geometry, but only with its `persistence`
//! feature, which pulls in serde, ron and a home-directory crate to store one
//! pair of numbers. The app already has a data directory and serde_json, so
//! the position is kept here instead.
//!
//! Written when a drag ends rather than continuously, so moving the capsule
//! costs one small write rather than one per frame.

use crate::platform::WorkArea;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// How much of the capsule has to land on a display for a remembered position
/// to be worth restoring. Enough to see it and to get a pointer on it, rather
/// than any overlap at all: a capsule with two points on screen is visible in
/// the strict sense and cannot be grabbed and dragged back.
const MIN_VISIBLE_WIDTH: f64 = 60.0;
const MIN_VISIBLE_HEIGHT: f64 = 20.0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowPosition {
    pub x: f32,
    pub y: f32,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("window.json")
}

/// Where the capsule was last left, if that is still somewhere usable.
///
/// A remembered position is deliberately not trusted. Displays get
/// disconnected and resolutions change, and a capsule restored onto a monitor
/// that no longer exists is invisible and cannot be dragged back. Anything
/// unreadable or implausible is ignored in favour of letting macOS place the
/// window, which is a worse position but always a visible one.
///
/// This matters more here than it would in an ordinary application. The
/// capsule is borderless, so AppKit's `constrainFrameRect:toScreen:` does not
/// rescue it: that only applies to titled windows. LocalFlow also sets
/// `LSUIElement`, so there is no Dock icon and no menu bar item, and the
/// console can only be opened from the capsule. A capsule restored off screen
/// is therefore an application that cannot be seen, quit, or moved, and the
/// instance lock stops a second copy being launched to recover.
pub fn load(data_dir: &Path, size: (f32, f32)) -> Option<WindowPosition> {
    let text = std::fs::read_to_string(path(data_dir)).ok()?;
    let position: WindowPosition = serde_json::from_str(&text).ok()?;
    if !position.x.is_finite() || !position.y.is_finite() {
        return None;
    }
    if !is_reachable(position, size, &crate::platform::work_areas()) {
        eprintln!(
            "LocalFlow ignored the capsule's remembered position ({}, {}): it is not on any \
             display that is connected now.",
            position.x, position.y
        );
        return None;
    }
    Some(position)
}

/// Whether a remembered position still puts enough of the capsule somewhere
/// the user can see and grab it.
///
/// Split out from `load` as a function over plain rectangles so the rule can
/// be tested without a display attached.
///
/// An empty list of work areas means the displays could not be read, which is
/// not the same as every position being fine. It is treated as unreachable,
/// because letting macOS place the window costs a remembered position and
/// trusting an unchecked one can cost access to the application.
fn is_reachable(position: WindowPosition, size: (f32, f32), areas: &[WorkArea]) -> bool {
    let left = position.x as f64;
    let top = position.y as f64;
    let right = left + size.0 as f64;
    let bottom = top + size.1 as f64;
    areas.iter().any(|area| {
        let visible_width = right.min(area.x + area.width) - left.max(area.x);
        let visible_height = bottom.min(area.y + area.height) - top.max(area.y);
        visible_width >= MIN_VISIBLE_WIDTH && visible_height >= MIN_VISIBLE_HEIGHT
    })
}

/// Record where the capsule now sits.
///
/// A failure here loses a convenience and nothing else, so it is reported
/// rather than propagated: refusing to dictate because a preference could not
/// be written would be a worse trade than starting centred next time.
pub fn save(data_dir: &Path, position: WindowPosition) {
    let Ok(text) = serde_json::to_string(&position) else {
        return;
    };
    if let Err(error) = std::fs::write(path(data_dir), text) {
        eprintln!("LocalFlow could not remember the capsule's position: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPSULE: (f32, f32) = (240.0, 56.0);

    fn laptop() -> WorkArea {
        WorkArea { x: 0.0, y: 25.0, width: 1512.0, height: 907.0 }
    }

    /// A second display placed to the left of the primary one, which is where
    /// negative coordinates come from.
    fn left_of_laptop() -> WorkArea {
        WorkArea { x: -1920.0, y: 0.0, width: 1920.0, height: 1080.0 }
    }

    #[test]
    fn a_position_on_a_connected_display_is_restored() {
        let position = WindowPosition { x: 600.0, y: 800.0 };
        assert!(is_reachable(position, CAPSULE, &[laptop()]));
    }

    /// The failure this whole check exists for: the capsule was left on a
    /// display that is no longer plugged in, so its coordinates are perfectly
    /// finite and point at nothing.
    #[test]
    fn a_position_on_a_display_that_is_gone_is_rejected() {
        let on_the_external = WindowPosition { x: -1200.0, y: 400.0 };
        assert!(is_reachable(on_the_external, CAPSULE, &[laptop(), left_of_laptop()]));
        assert!(
            !is_reachable(on_the_external, CAPSULE, &[laptop()]),
            "unplugging the external display must retire positions that were on it"
        );
    }

    /// Dragging can leave the capsule mostly off the edge, and a mixed-DPI
    /// restore can return a saved position at double its coordinates. Both
    /// arrive here as a rectangle that barely overlaps.
    #[test]
    fn a_position_hanging_off_the_edge_is_rejected() {
        let barely_on = WindowPosition { x: 1512.0 - 20.0, y: 500.0 };
        assert!(!is_reachable(barely_on, CAPSULE, &[laptop()]));
        let below_the_bottom = WindowPosition { x: 600.0, y: 25.0 + 907.0 - 10.0 };
        assert!(!is_reachable(below_the_bottom, CAPSULE, &[laptop()]));
    }

    /// Not knowing where the displays are is not permission to restore
    /// anything: an unchecked position can cost access to the application.
    #[test]
    fn an_unknown_display_layout_rejects_every_position() {
        assert!(!is_reachable(WindowPosition { x: 600.0, y: 800.0 }, CAPSULE, &[]));
    }
}
