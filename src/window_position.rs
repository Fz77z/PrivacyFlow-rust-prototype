//! Remember where the user put the capsule.
//!
//! eframe can persist window geometry, but only with its `persistence`
//! feature, which pulls in serde, ron and a home-directory crate to store one
//! pair of numbers. The app already has a data directory and serde_json, so
//! the position is kept here instead.
//!
//! What is remembered is the capsule's centre, not its corner. The capsule
//! has three sizes in minimal mode, and a corner moves when the size changes
//! while a centre does not, so centring is what makes the bead grow outward
//! from a fixed point rather than unfolding down and to the right.
//!
//! Written when a drag ends rather than continuously, so moving the capsule
//! costs one small write rather than one per frame.

use crate::platform::WorkArea;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The middle of the capsule, in the downward-y coordinates window positions
/// use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Centre {
    pub x: f32,
    pub y: f32,
}

/// What is actually on disk.
///
/// Two shapes, because files written before the capsule could change size
/// hold a top left corner. Reading one of those as a centre would move the
/// capsule by half its size, so the old shape is recognised and converted
/// rather than silently reinterpreted.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Stored {
    Centre { center_x: f32, center_y: f32 },
    TopLeft { x: f32, y: f32 },
}

impl Stored {
    fn into_centre(self, size: (f32, f32)) -> Centre {
        match self {
            Stored::Centre { center_x, center_y } => Centre { x: center_x, y: center_y },
            Stored::TopLeft { x, y } => {
                Centre { x: x + size.0 / 2.0, y: y + size.1 / 2.0 }
            }
        }
    }
}

#[derive(Serialize)]
struct Written {
    center_x: f32,
    center_y: f32,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("window.json")
}

/// Where the capsule was last left, if that is still somewhere usable.
///
/// `size` is the capsule's full size, used only to convert a file written in
/// the old corner-based format.
///
/// A remembered position is deliberately not trusted. Displays get
/// disconnected and resolutions change, and a capsule restored onto a monitor
/// that no longer exists is invisible and cannot be dragged back.
///
/// This matters more here than it would in an ordinary application. The
/// capsule is borderless, so AppKit's `constrainFrameRect:toScreen:` does not
/// rescue it: that only applies to titled windows. LocalFlow also sets
/// `LSUIElement`, so there is no Dock icon and no menu bar item, and the
/// console can only be opened from the capsule. A capsule restored off screen
/// is therefore an application that cannot be seen, quit, or moved, and the
/// instance lock stops a second copy being launched to recover.
///
/// There is a second route into the same failure, upstream of this code:
/// `egui-winit` converts a `with_position` value using `primary_monitor()`'s
/// scale factor while winit converts it back using `NSScreen::mainScreen`'s,
/// so on a mixed-DPI setup a position saved correctly can come back at double
/// or half its coordinates. Nothing here can fix that, which is why this
/// checks the value that comes back rather than trusting the one that went
/// out.
pub fn load(data_dir: &Path, size: (f32, f32)) -> Option<Centre> {
    let text = std::fs::read_to_string(path(data_dir)).ok()?;
    let stored: Stored = serde_json::from_str(&text).ok()?;
    let centre = stored.into_centre(size);
    if !centre.x.is_finite() || !centre.y.is_finite() {
        return None;
    }
    if !is_reachable(centre, &crate::platform::work_areas()) {
        eprintln!(
            "LocalFlow ignored the capsule's remembered position ({}, {}): it is not on any \
             display that is connected now.",
            centre.x, centre.y
        );
        return None;
    }
    Some(centre)
}

/// Whether a remembered centre still lands somewhere the user can reach.
///
/// A centre inside a display guarantees at least half the capsule is on
/// screen in both axes, which is enough to see and to grab.
///
/// An empty list of work areas means the displays could not be read, which is
/// not the same as every position being fine. It is treated as unreachable,
/// because letting macOS place the window costs a remembered position while
/// trusting an unchecked one can cost access to the application.
fn is_reachable(centre: Centre, areas: &[WorkArea]) -> bool {
    area_for(centre, areas).is_some()
}

/// The display a centre sits on, if any.
fn area_for(centre: Centre, areas: &[WorkArea]) -> Option<&WorkArea> {
    let x = centre.x as f64;
    let y = centre.y as f64;
    areas.iter().find(|area| {
        x >= area.x && x <= area.x + area.width && y >= area.y && y <= area.y + area.height
    })
}

/// The top left corner at which to put a window of this size.
///
/// Centred on the anchor, then pushed back inside the display the anchor is
/// on, so that a bead parked near an edge does not grow its label off the
/// screen. Clamping against the anchor's own display rather than the first in
/// the list is what stops a capsule on a second monitor being dragged onto
/// the primary one.
///
/// With no display to clamp against, the window is centred and left alone:
/// `load` has already refused to restore an unreachable centre, so this only
/// happens for a capsule the user has in hand.
pub fn place(centre: Centre, size: (f32, f32), areas: &[WorkArea]) -> (f32, f32) {
    let left = centre.x - size.0 / 2.0;
    let top = centre.y - size.1 / 2.0;
    let Some(area) = area_for(centre, areas) else {
        return (left.round(), top.round());
    };
    let clamp = |value: f32, low: f64, span: f64, extent: f32| {
        let highest = (low + span) as f32 - extent;
        // A window wider than the display would make the bounds cross. The
        // left edge wins, because a capsule running off the right is still
        // grabbable and one running off the left is not.
        value.min(highest).max(low as f32)
    };
    (
        clamp(left, area.x, area.width, size.0).round(),
        clamp(top, area.y, area.height, size.1).round(),
    )
}

/// Record where the capsule now sits.
///
/// A failure here loses a convenience and nothing else, so it is reported
/// rather than propagated: refusing to dictate because a preference could not
/// be written would be a worse trade than starting centred next time.
pub fn save(data_dir: &Path, centre: Centre) {
    let Ok(text) = serde_json::to_string(&Written { center_x: centre.x, center_y: centre.y })
    else {
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
    const BEAD: (f32, f32) = (46.0, 14.0);

    fn laptop() -> WorkArea {
        WorkArea { x: 0.0, y: 25.0, width: 1512.0, height: 907.0 }
    }

    /// A second display placed to the left of the primary one, which is where
    /// negative coordinates come from.
    fn left_of_laptop() -> WorkArea {
        WorkArea { x: -1920.0, y: 0.0, width: 1920.0, height: 1080.0 }
    }

    /// Files written before the capsule could change size hold the window's
    /// top left corner. Reading one of those as though it were a centre moves
    /// the capsule by half its size, which is 120 by 28 points.
    #[test]
    fn a_position_saved_in_the_old_format_converts_to_a_centre() {
        let stored: Stored = serde_json::from_str(r#"{"x": 600.0, "y": 800.0}"#).unwrap();
        let centre = stored.into_centre(CAPSULE);
        assert_eq!(centre.x, 720.0);
        assert_eq!(centre.y, 828.0);
    }

    /// And the new format is taken at face value, with no conversion.
    #[test]
    fn a_position_saved_in_the_new_format_is_already_a_centre() {
        let stored: Stored =
            serde_json::from_str(r#"{"center_x": 720.0, "center_y": 828.0}"#).unwrap();
        let centre = stored.into_centre(CAPSULE);
        assert_eq!(centre.x, 720.0);
        assert_eq!(centre.y, 828.0);
    }

    #[test]
    fn a_centre_on_a_connected_display_is_restored() {
        assert!(is_reachable(Centre { x: 600.0, y: 800.0 }, &[laptop()]));
    }

    /// The failure this check exists for: the capsule was left on a display
    /// that is no longer plugged in, so its coordinates are perfectly finite
    /// and point at nothing. With no Dock icon and no menu bar item, that is
    /// an application the user cannot reach.
    #[test]
    fn a_centre_on_a_display_that_is_gone_is_rejected() {
        let on_the_external = Centre { x: -1200.0, y: 400.0 };
        assert!(is_reachable(on_the_external, &[laptop(), left_of_laptop()]));
        assert!(
            !is_reachable(on_the_external, &[laptop()]),
            "unplugging the external display must retire positions that were on it"
        );
    }

    /// Not knowing where the displays are is not permission to restore
    /// anything: an unchecked position can cost access to the application.
    #[test]
    fn an_unknown_display_layout_rejects_every_centre() {
        assert!(!is_reachable(Centre { x: 600.0, y: 800.0 }, &[]));
    }

    /// The ordinary case: nowhere near an edge, so the window is simply
    /// centred on the anchor and nothing is clamped.
    #[test]
    fn a_capsule_away_from_any_edge_is_centred_on_its_anchor() {
        let (x, y) = place(Centre { x: 700.0, y: 500.0 }, CAPSULE, &[laptop()]);
        assert_eq!((x, y), (580.0, 472.0));
    }

    /// A bead parked near the right edge grows to 240 points wide when
    /// pointed at. Without clamping, the label and the console icon would
    /// grow off the screen.
    #[test]
    fn growing_near_an_edge_stays_on_screen() {
        // Chosen so the bead fits where it is and the grown capsule does not.
        // The bead spans 1427..1473 of a 1512 wide display, which is inside
        // it; the full capsule would span 1330..1570, which is not.
        let near_the_edge = Centre { x: 1450.0, y: 500.0 };
        let (bead_x, _) = place(near_the_edge, BEAD, &[laptop()]);
        assert_eq!(bead_x, 1427.0, "the bead itself still fits, so it is not moved");
        let (full_x, _) = place(near_the_edge, CAPSULE, &[laptop()]);
        assert_eq!(full_x, 1272.0, "the grown capsule is pushed back on screen");
    }

    /// Clamping is done against the display the anchor is on, not against the
    /// first one in the list, or a capsule on the external display would be
    /// dragged onto the laptop screen.
    #[test]
    fn clamping_uses_the_display_the_capsule_is_on() {
        let on_the_external = Centre { x: -1000.0, y: 500.0 };
        let (x, _) = place(on_the_external, CAPSULE, &[laptop(), left_of_laptop()]);
        assert_eq!(x, -1120.0);
    }
}
