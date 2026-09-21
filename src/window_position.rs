//! Remember where the user put the capsule.
//!
//! eframe can persist window geometry, but only with its `persistence`
//! feature, which pulls in serde, ron and a home-directory crate to store one
//! pair of numbers. The app already has a data directory and serde_json, so
//! the position is kept here instead.
//!
//! Written when a drag ends rather than continuously, so moving the capsule
//! costs one small write rather than one per frame.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
pub fn load(data_dir: &Path) -> Option<WindowPosition> {
    let text = std::fs::read_to_string(path(data_dir)).ok()?;
    let position: WindowPosition = serde_json::from_str(&text).ok()?;
    if !position.x.is_finite() || !position.y.is_finite() {
        return None;
    }
    Some(position)
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
